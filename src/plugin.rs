use std::collections::HashSet;
use std::fmt;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use ring::digest;
use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::FromRow;

use crate::aircraft::faa::{
    admit_aircraft_source_identity, AircraftAdmissionError, BlockReason, FaaSerialCorrection,
};
use crate::aircraft::repair::{
    record_bound_source_serial_correction, record_bound_source_visual_correction,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{
    parse_listing_html_for_avionics_validation, validate_source_url, GeminiListingExtractor,
};
use crate::html::listing::source::listing_extraction_source;
use crate::listing::avionics::correction::validate_or_correct_listing_avionics;
use crate::listing::avionics::disposition::{
    record_automatic_occurrence_dispositions, AutomaticOccurrenceDisposition,
};
use crate::listing::avionics::extraction::validate_unbound_current_avionics_extraction;
use crate::listing::review::attach_pending_review_submission;
use crate::listings::{
    create_listing_with_progress_and_occurrence_dispositions,
    finalize_signed_source_listing_after_receipt, get_listing, resume_bound_replay_listing,
    resume_signed_source_correction_listing, resume_signed_source_visual_correction_listing,
    ListingCreationMode, ListingStoreError, SignedSourceListingBinding,
    SourceVisualRegistrationCorrection, SOURCE_IDENTITY_RECEIPT_PENDING,
};
use crate::models::{
    ListingPreview, ParsedListing, PluginInstall, PluginSubmission, PluginSubmissionRequest,
    SaleListing, User,
};

const CURRENT_CHECKPOINT_FIELDS: &[&str] = &[
    "manufacturer",
    "model",
    "variant",
    "model_year",
    "asking_price_usd",
    "currency",
    "airframe_hours",
    "engine_hours",
    "engine_time_basis",
    "engine_time_evidence",
    "engine_time_confidence",
    "propeller_hours",
    "propeller_time_basis",
    "propeller_time_evidence",
    "propeller_time_confidence",
    "installed_engine",
    "installed_propeller",
    "registration_number",
    "serial_number",
    "status",
    "avionics",
    "valuation_facts",
    "visual_identity_recovery",
];

const MAX_RENDERED_HTML_BYTES: usize = 5 * 1024 * 1024;
const SIGNATURE_PREFIX: &str = "aircost-plugin-v1";

macro_rules! query_as_one {
    ($db:expr, $row:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_one(pool).await
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_one(pool).await
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

#[derive(Debug)]
pub enum PluginStoreError {
    Validation(String),
    Permission(String),
    NotFound(String),
    AircraftAdmission(AircraftAdmissionError),
    AdmissionBlocked(PluginReplayAdmissionBlock),
    Database(String),
}

impl fmt::Display for PluginStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginStoreError::Validation(message)
            | PluginStoreError::Permission(message)
            | PluginStoreError::NotFound(message)
            | PluginStoreError::Database(message) => write!(formatter, "{message}"),
            PluginStoreError::AdmissionBlocked(reason) => {
                write!(formatter, "replay admission is blocked: {}", reason.code())
            }
            PluginStoreError::AircraftAdmission(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for PluginStoreError {}

impl From<sqlx::Error> for PluginStoreError {
    fn from(error: sqlx::Error) -> Self {
        PluginStoreError::Database(error.to_string())
    }
}

impl From<anyhow::Error> for PluginStoreError {
    fn from(error: anyhow::Error) -> Self {
        PluginStoreError::Validation(error.to_string())
    }
}

impl From<ListingStoreError> for PluginStoreError {
    fn from(error: ListingStoreError) -> Self {
        match error {
            ListingStoreError::Validation(message) | ListingStoreError::State(message) => {
                PluginStoreError::Validation(message)
            }
            ListingStoreError::AircraftAdmission(error) => {
                PluginStoreError::AircraftAdmission(error)
            }
            ListingStoreError::Ingestion {
                listing_id,
                message,
            } => PluginStoreError::Validation(format!(
                "listing {listing_id} was quarantined: {message}"
            )),
            ListingStoreError::NotFound(message) => PluginStoreError::NotFound(message),
            ListingStoreError::Permission(message) => PluginStoreError::Permission(message),
            ListingStoreError::Database(message) => PluginStoreError::Database(message),
        }
    }
}

pub(crate) type StoreResult<T> = Result<T, PluginStoreError>;
pub type PluginProgressSender = tokio::sync::mpsc::UnboundedSender<Value>;

#[derive(Debug)]
pub struct PluginSubmissionOutcome {
    pub submission: PluginSubmission,
    pub preview: Option<ListingPreview>,
    pub listing: Option<SaleListing>,
}

#[derive(Debug, Serialize)]
pub struct PluginUrlStatus {
    pub submitted: bool,
    pub submission: Option<PluginSubmission>,
    pub listing_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PluginExtractionCheckpoint {
    pub submission_id: i64,
    pub rendered_html_sha256: String,
    pub extracted_listing_sha256: String,
    pub avionics_occurrence_count: usize,
    #[serde(skip)]
    pub(crate) exact_extracted_listing_json: String,
    #[serde(skip)]
    pub(crate) exact_capture: PluginReplayCaptureAttestation,
}

#[derive(Debug)]
pub(crate) struct PluginReplayCaptureAttestation {
    pub(crate) submission_id: i64,
    pub(crate) user_id: i64,
    pub(crate) plugin_install_id: i64,
    pub(crate) public_key_base64: String,
    pub(crate) install_revoked_at: Option<String>,
    pub(crate) source_url: String,
    pub(crate) submitted_at: String,
    pub(crate) rendered_html: String,
    pub(crate) rendered_html_sha256: String,
    pub(crate) signature_base64: String,
    pub(crate) extracted_listing_json: String,
    pub(crate) canonical_listing_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PluginExtractionPreflight {
    pub submission_id: i64,
    pub capture_valid: bool,
    pub current_checkpoint: Option<PluginExtractionCheckpoint>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PluginListingReplayOutcome {
    Materialized {
        submission_id: i64,
        listing: SaleListing,
    },
    Rejected {
        submission_id: i64,
        rejection: PluginReplayTerminalRejection,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginReplayTerminalRejection {
    MissingRegistration,
    NonNRegistration,
    InvalidNNumber,
    SerialConflict,
}

impl PluginReplayTerminalRejection {
    pub fn stage(self) -> &'static str {
        "faa_aircraft_admission"
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::MissingRegistration => "missing_registration",
            Self::NonNRegistration => "non_n_registration",
            Self::InvalidNNumber => "invalid_n_number",
            Self::SerialConflict => "serial_conflict",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginReplayAdmissionBlock {
    LookupFailed,
    ListingNotFound,
    RegistrySnapshotUnavailable,
    RegistrationNotFound,
    RegistrationNotCovered,
    AmbiguousRegistration,
    RegistryAircraftIdentityUnavailable,
    AircraftManufacturerMismatch,
    AircraftModelMismatch,
    CanonicalIdentityAssignmentMissing,
    CanonicalIdentityAssignmentMismatch,
}

impl PluginReplayAdmissionBlock {
    pub fn code(self) -> &'static str {
        match self {
            Self::LookupFailed => "faa_lookup_failed",
            Self::ListingNotFound => "faa_listing_not_found",
            Self::RegistrySnapshotUnavailable => "faa_registry_snapshot_unavailable",
            Self::RegistrationNotFound => "faa_registration_not_found",
            Self::RegistrationNotCovered => "faa_registration_not_covered",
            Self::AmbiguousRegistration => "faa_ambiguous_registration",
            Self::RegistryAircraftIdentityUnavailable => {
                "faa_registry_aircraft_identity_unavailable"
            }
            Self::AircraftManufacturerMismatch => "faa_aircraft_manufacturer_mismatch",
            Self::AircraftModelMismatch => "faa_aircraft_model_mismatch",
            Self::CanonicalIdentityAssignmentMissing => "faa_canonical_identity_assignment_missing",
            Self::CanonicalIdentityAssignmentMismatch => {
                "faa_canonical_identity_assignment_mismatch"
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PluginReplayCaptureState {
    pub submission_id: i64,
    pub rendered_html_sha256: String,
    pub checkpoint: Option<PluginExtractionCheckpoint>,
    pub canonical_listing_id: Option<i64>,
    pub materialization_receipt_listing_id: Option<i64>,
}

#[derive(Debug, FromRow)]
struct PluginSubmissionRow {
    id: i64,
    user_id: i64,
    plugin_install_id: i64,
    source_url: String,
    submitted_at: String,
    rendered_html_sha256: String,
    signature_base64: String,
    extracted_listing_json: Option<String>,
    extraction_error: Option<String>,
    canonical_listing_id: Option<i64>,
}

#[derive(Debug, FromRow)]
struct PluginCheckpointRow {
    id: i64,
    user_id: i64,
    plugin_install_id: i64,
    public_key_base64: String,
    install_revoked_at: Option<String>,
    source_url: String,
    submitted_at: String,
    rendered_html: String,
    rendered_html_sha256: String,
    signature_base64: String,
    extracted_listing_json: Option<String>,
    extraction_error: Option<String>,
    canonical_listing_id: Option<i64>,
}

#[derive(Debug, FromRow)]
struct ListingIdRow {
    id: i64,
}

#[derive(Debug, FromRow)]
struct MaterializationReceiptRow {
    aircraft_sale_listing_id: i64,
    rendered_html_sha256: String,
    extracted_listing_sha256: String,
}

fn replay_capture_attestation(
    stored: &PluginCheckpointRow,
    extracted_listing_json: &str,
) -> PluginReplayCaptureAttestation {
    PluginReplayCaptureAttestation {
        submission_id: stored.id,
        user_id: stored.user_id,
        plugin_install_id: stored.plugin_install_id,
        public_key_base64: stored.public_key_base64.clone(),
        install_revoked_at: stored.install_revoked_at.clone(),
        source_url: stored.source_url.clone(),
        submitted_at: stored.submitted_at.clone(),
        rendered_html: stored.rendered_html.clone(),
        rendered_html_sha256: stored.rendered_html_sha256.clone(),
        signature_base64: stored.signature_base64.clone(),
        extracted_listing_json: extracted_listing_json.to_string(),
        canonical_listing_id: stored.canonical_listing_id,
    }
}

pub async fn plugin_submission_owner(db: &AppDb, submission_id: i64) -> StoreResult<User> {
    query_as_optional!(
        db,
        User,
        r#"
        SELECT owner.id, owner.email, owner.display_name,
               owner.auth_provider, owner.auth_subject
        FROM plugin_submissions submission
        JOIN users owner ON owner.id = submission.user_id
        WHERE submission.id = ?
        "#,
        submission_id
    )?
    .ok_or_else(|| PluginStoreError::NotFound("plugin submission not found".to_string()))
}

pub async fn register_plugin_install(
    db: &AppDb,
    user: &User,
    public_key_base64: &str,
) -> StoreResult<PluginInstall> {
    validate_public_key(public_key_base64)?;
    Ok(query_as_one!(
        db,
        PluginInstall,
        r#"
        INSERT INTO plugin_installs (
          user_id,
          public_key_base64
        )
        VALUES (?, ?)
        RETURNING id, user_id, public_key_base64, created_at, revoked_at
        "#,
        user.id,
        public_key_base64.trim()
    )?)
}

pub async fn submit_plugin_html(
    db: &AppDb,
    user: &User,
    request: &PluginSubmissionRequest,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<PluginSubmissionOutcome> {
    submit_plugin_html_with_progress(db, user, request, extractor, None).await
}

pub async fn submit_plugin_html_with_progress(
    db: &AppDb,
    user: &User,
    request: &PluginSubmissionRequest,
    extractor: Option<&GeminiListingExtractor>,
    progress: Option<&PluginProgressSender>,
) -> StoreResult<PluginSubmissionOutcome> {
    emit_plugin_progress(
        progress,
        "verifying_upload",
        "Verifying upload request and signature",
    );
    validate_submission_request(request)?;
    let install = plugin_install_for_user(db, user.id, request.plugin_install_id).await?;
    let rendered_html_sha256 = sha256_hex(request.rendered_html.as_bytes());
    verify_submission_signature(
        &install.public_key_base64,
        request.plugin_install_id,
        &request.source_url,
        &rendered_html_sha256,
        &request.signature,
    )?;
    if let Some(existing) = exact_signed_capture_submission(
        db,
        user.id,
        request.plugin_install_id,
        &request.source_url,
        &rendered_html_sha256,
    )
    .await?
    {
        let Some(listing_id) = existing.canonical_listing_id else {
            return reprocess_plugin_submission(db, user, existing.id, extractor).await;
        };
        debug_assert_eq!(existing.canonical_listing_id, Some(listing_id));
        return recover_or_return_bound_signed_submission(db, user, existing.id, extractor).await;
    }

    let mut preview = None;
    let mut listing = None;
    let mut extracted_listing_json = None;
    let mut extraction_error = None;
    let mut canonical_listing_id = None;
    let mut occurrence_dispositions: Vec<AutomaticOccurrenceDisposition> = Vec::new();
    let mut source_serial_correction = None;
    let mut source_visual_correction = None;
    let mut prepared_submission: Option<PluginSubmission> = None;
    let mut durable_materialization_error = None;

    if let Some(extractor) = extractor {
        emit_plugin_progress(
            progress,
            "extracting_listing",
            "Extracting listing data from page",
        );
        match extract_capture_to_current_checkpoint(
            &request.source_url,
            &request.rendered_html,
            extractor,
        )
        .await
        {
            Ok((parsed_preview, checkpoint_payload)) => {
                extracted_listing_json = Some(checkpoint_payload);
                let source_admission = admit_aircraft_source_identity(
                    db,
                    parsed_preview.parsed_listing.registration_number.as_deref(),
                    parsed_preview.parsed_listing.serial_number.as_deref(),
                    parsed_preview.context_text.as_deref(),
                )
                .await;
                let submission = insert_plugin_submission(
                    db,
                    user.id,
                    request.plugin_install_id,
                    &request.source_url,
                    &request.rendered_html,
                    &rendered_html_sha256,
                    &request.signature,
                    extracted_listing_json.as_ref(),
                    None,
                    None,
                )
                .await?;
                let bound_extracted_listing_json = extracted_listing_json
                    .as_ref()
                    .expect("the extracted checkpoint was assigned before admission")
                    .to_string();
                let binding = signed_source_listing_binding(
                    submission.id,
                    submission.user_id,
                    submission.plugin_install_id,
                    &install.public_key_base64,
                    install.revoked_at.as_deref(),
                    &submission.source_url,
                    &submission.submitted_at,
                    &request.rendered_html,
                    &submission.rendered_html_sha256,
                    &submission.signature_base64,
                    Some(bound_extracted_listing_json.clone()),
                    None,
                    bound_extracted_listing_json,
                );
                prepared_submission = Some(submission);
                match create_listing_with_progress_and_occurrence_dispositions(
                    db,
                    user.id,
                    &parsed_preview,
                    None,
                    Some(extractor),
                    progress,
                    ListingCreationMode::SignedSource,
                    Some(&binding),
                )
                .await
                {
                    Ok(created) => {
                        canonical_listing_id = Some(created.listing.id);
                        occurrence_dispositions = created.occurrence_dispositions;
                        source_serial_correction = created.source_serial_correction;
                        source_visual_correction = created.source_visual_correction;
                        listing = Some(created.listing);
                    }
                    Err(ListingStoreError::Ingestion {
                        listing_id,
                        message,
                    }) => {
                        canonical_listing_id = Some(listing_id);
                        source_serial_correction = admit_aircraft_source_identity(
                            db,
                            parsed_preview.parsed_listing.registration_number.as_deref(),
                            parsed_preview.parsed_listing.serial_number.as_deref(),
                            parsed_preview.context_text.as_deref(),
                        )
                        .await
                        .ok()
                        .and_then(|admission| admission.serial_correction);
                        durable_materialization_error = Some(message);
                    }
                    Err(error) => {
                        let prepared = prepared_submission.as_ref().ok_or_else(|| {
                            PluginStoreError::Database(
                                "signed capture lost its prepared extraction checkpoint"
                                    .to_string(),
                            )
                        })?;
                        let retained = plugin_submission_for_user(db, user.id, prepared.id).await?;
                        if let Some(listing_id) = retained.canonical_listing_id {
                            canonical_listing_id = Some(listing_id);
                            source_serial_correction = source_admission
                                .as_ref()
                                .ok()
                                .and_then(|admission| admission.serial_correction.clone());
                            durable_materialization_error = Some(error.to_string());
                        } else {
                            extraction_error = Some(error.to_string());
                        }
                    }
                }
                preview = Some(parsed_preview);
            }
            Err(error) => {
                extraction_error = Some(format!("{error:#}"));
            }
        }
    } else {
        extraction_error =
            Some("GEMINI_API_KEY must be set to extract plugin submissions".to_string());
    }

    emit_plugin_progress(
        progress,
        "recording_submission",
        "Recording plugin submission",
    );
    if let Some(message) = durable_materialization_error {
        return Err(PluginStoreError::Database(format!(
            "signed capture stopped after its atomic listing binding: {message}"
        )));
    }
    let materialized = async {
        let submission = if let Some(prepared) = prepared_submission.as_ref() {
            update_plugin_submission_result(
                db,
                user.id,
                prepared.id,
                extracted_listing_json.as_ref(),
                extraction_error.as_deref(),
                canonical_listing_id,
            )
            .await?
        } else {
            let submission = insert_plugin_submission(
                db,
                user.id,
                request.plugin_install_id,
                &request.source_url,
                &request.rendered_html,
                &rendered_html_sha256,
                &request.signature,
                extracted_listing_json.as_ref(),
                extraction_error.as_deref(),
                canonical_listing_id,
            )
            .await?;
            submission
        };
        attach_submission_to_pending_review_if_needed(db, user, listing.as_ref(), &submission)
            .await?;
        if let Some(created_listing) = listing.as_ref() {
            record_automatic_occurrence_dispositions(
                db,
                created_listing.id,
                submission.id,
                user.id,
                &occurrence_dispositions,
            )
            .await
            .map_err(PluginStoreError::Database)?;
        }
        // The immutable correction receipt is the last fallible materialization
        // operation. A corrected capture is already atomically bound inside its
        // private database receipt gate, so an exact retry can resume here.
        record_source_serial_correction_if_present(
            db,
            user.id,
            canonical_listing_id,
            &submission,
            source_serial_correction.as_ref(),
        )
        .await?;
        record_source_visual_correction_if_present(
            db,
            user.id,
            canonical_listing_id,
            &submission,
            source_visual_correction.as_ref(),
        )
        .await?;
        Ok::<PluginSubmission, PluginStoreError>(submission)
    }
    .await;
    let submission = materialized?;
    if source_serial_correction.is_some() || source_visual_correction.is_some() {
        let listing_id = canonical_listing_id.ok_or_else(|| {
            PluginStoreError::Database(
                "corrected source listing lost its canonical identifier after receipt".to_string(),
            )
        })?;
        listing = Some(
            finalize_signed_source_listing_after_receipt(db, user.id, listing_id, submission.id)
                .await?,
        );
    }

    if submission.canonical_listing_id.is_some() {
        let retained = load_checkpoint_capture(db, user.id, submission.id).await?;
        let (listing_id, extracted_listing_sha256) =
            validate_bound_signed_checkpoint(db, &retained).await?;
        record_materialization_receipt(db, &retained, listing_id, &extracted_listing_sha256)
            .await?;
    }

    Ok(PluginSubmissionOutcome {
        submission,
        preview,
        listing,
    })
}

pub async fn plugin_url_status(
    db: &AppDb,
    user: &User,
    source_url: &str,
) -> StoreResult<PluginUrlStatus> {
    validate_source_url(source_url)?;
    let submission = query_as_optional!(
        db,
        PluginSubmissionRow,
        r#"
        SELECT
          id,
          user_id,
          plugin_install_id,
          source_url,
          submitted_at,
          rendered_html_sha256,
          signature_base64,
          extracted_listing_json,
          extraction_error,
          canonical_listing_id
        FROM plugin_submissions
        WHERE user_id = ? AND source_url = ?
        ORDER BY submitted_at DESC, id DESC
        LIMIT 1
        "#,
        user.id,
        source_url
    )?;
    let submission = submission.map(plugin_submission_from_row).transpose()?;
    let listing_id = match submission
        .as_ref()
        .and_then(|submission| submission.canonical_listing_id)
    {
        Some(listing_id) => Some(listing_id),
        None => latest_listing_id_for_source_url(db, user.id, source_url).await?,
    };
    Ok(PluginUrlStatus {
        submitted: submission.is_some() || listing_id.is_some(),
        submission,
        listing_id,
    })
}

pub async fn reprocess_plugin_submission(
    db: &AppDb,
    user: &User,
    submission_id: i64,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<PluginSubmissionOutcome> {
    let stored = load_checkpoint_capture(db, user.id, submission_id).await?;
    validate_stored_checkpoint_capture(db, &stored).await?;
    if stored.canonical_listing_id.is_some() {
        return recover_or_return_bound_signed_submission(db, user, stored.id, extractor).await;
    }
    let mut preview = None;
    let mut listing = None;
    let mut extracted_listing_json = None;
    let mut extraction_error = None;
    let mut canonical_listing_id = stored.canonical_listing_id;
    let mut occurrence_dispositions: Vec<AutomaticOccurrenceDisposition> = Vec::new();
    let mut source_serial_correction = None;
    let mut source_visual_correction = None;
    let mut durable_materialization_error = None;

    if let Some(extractor) = extractor {
        match extract_capture_to_current_checkpoint(
            &stored.source_url,
            &stored.rendered_html,
            extractor,
        )
        .await
        {
            Ok((parsed_preview, checkpoint_payload)) => {
                let preflight_source_admission = admit_aircraft_source_identity(
                    db,
                    parsed_preview.parsed_listing.registration_number.as_deref(),
                    parsed_preview.parsed_listing.serial_number.as_deref(),
                    parsed_preview.context_text.as_deref(),
                )
                .await;
                let preflight_source_correction = preflight_source_admission
                    .as_ref()
                    .ok()
                    .and_then(|admission| admission.serial_correction.as_ref());
                if let (Some(existing_listing_id), Some(correction)) = (
                    stored.canonical_listing_id,
                    preflight_source_correction.as_ref(),
                ) {
                    let retained_payload = stored
                        .extracted_listing_json
                        .as_deref()
                        .and_then(|value| serde_json::from_str::<Value>(value).ok());
                    if retained_payload.as_ref() != Some(&checkpoint_payload)
                        || !exact_source_correction_receipt_exists(
                            db,
                            user.id,
                            stored.id,
                            existing_listing_id,
                            parsed_preview.parsed_listing.registration_number.as_deref(),
                            correction,
                        )
                        .await?
                    {
                        return Err(PluginStoreError::Validation(
                            "a bound signed capture cannot replace its existing listing with a newly corrected duplicate; repair the existing binding explicitly"
                                .to_string(),
                        ));
                    }
                    let submission = plugin_submission_for_user(db, user.id, stored.id).await?;
                    let mut existing_listing =
                        get_listing(db, user.id, existing_listing_id).await?;
                    if existing_listing.ingestion_state != "ready" {
                        existing_listing = finalize_signed_source_listing_after_receipt(
                            db,
                            user.id,
                            existing_listing_id,
                            stored.id,
                        )
                        .await?;
                    }
                    return Ok(PluginSubmissionOutcome {
                        submission,
                        preview: Some(parsed_preview),
                        listing: Some(existing_listing),
                    });
                }
                let checkpoint_payload_json = checkpoint_payload.to_string();
                extracted_listing_json = Some(checkpoint_payload);
                // Every newly materialized signed capture supplies the exact
                // source/checkpoint binding. Paid avionics grounding can be
                // reached independently of FAA correction, and its same-case
                // capability must never exist without this durable scope.
                let signed_source_binding = stored.canonical_listing_id.is_none().then(|| {
                    signed_source_listing_binding(
                        stored.id,
                        stored.user_id,
                        stored.plugin_install_id,
                        &stored.public_key_base64,
                        stored.install_revoked_at.as_deref(),
                        &stored.source_url,
                        &stored.submitted_at,
                        &stored.rendered_html,
                        &stored.rendered_html_sha256,
                        &stored.signature_base64,
                        stored.extracted_listing_json.clone(),
                        stored.extraction_error.clone(),
                        checkpoint_payload_json.clone(),
                    )
                });
                match create_listing_with_progress_and_occurrence_dispositions(
                    db,
                    user.id,
                    &parsed_preview,
                    None,
                    Some(extractor),
                    None,
                    ListingCreationMode::SignedSource,
                    signed_source_binding.as_ref(),
                )
                .await
                {
                    Ok(created) => {
                        canonical_listing_id = Some(created.listing.id);
                        occurrence_dispositions = created.occurrence_dispositions;
                        source_serial_correction = created.source_serial_correction;
                        source_visual_correction = created.source_visual_correction;
                        listing = Some(created.listing);
                    }
                    Err(ListingStoreError::Ingestion {
                        listing_id,
                        message,
                    }) => {
                        canonical_listing_id = Some(listing_id);
                        source_serial_correction = admit_aircraft_source_identity(
                            db,
                            parsed_preview.parsed_listing.registration_number.as_deref(),
                            parsed_preview.parsed_listing.serial_number.as_deref(),
                            parsed_preview.context_text.as_deref(),
                        )
                        .await
                        .ok()
                        .and_then(|admission| admission.serial_correction);
                        durable_materialization_error = Some(message);
                    }
                    Err(error) => {
                        if signed_source_binding.is_some() {
                            let retained =
                                plugin_submission_for_user(db, user.id, stored.id).await?;
                            if let Some(listing_id) = retained.canonical_listing_id {
                                canonical_listing_id = Some(listing_id);
                                source_serial_correction = preflight_source_correction.cloned();
                                durable_materialization_error = Some(error.to_string());
                            } else {
                                extraction_error = Some(error.to_string());
                            }
                        } else {
                            extraction_error = Some(error.to_string());
                        }
                    }
                }
                preview = Some(parsed_preview);
            }
            Err(error) => {
                extraction_error = Some(format!("{error:#}"));
            }
        }
    } else {
        extraction_error =
            Some("GEMINI_API_KEY must be set to extract plugin submissions".to_string());
    }

    if let Some(message) = durable_materialization_error {
        return Err(PluginStoreError::Database(format!(
            "reprocessed corrected capture stopped after its atomic listing binding: {message}"
        )));
    }

    let materialized = async {
        let submission = update_plugin_submission_result(
            db,
            user.id,
            stored.id,
            extracted_listing_json.as_ref(),
            extraction_error.as_deref(),
            canonical_listing_id,
        )
        .await?;
        attach_submission_to_pending_review_if_needed(db, user, listing.as_ref(), &submission)
            .await?;
        if let Some(created_listing) = listing.as_ref() {
            record_automatic_occurrence_dispositions(
                db,
                created_listing.id,
                submission.id,
                user.id,
                &occurrence_dispositions,
            )
            .await
            .map_err(PluginStoreError::Database)?;
        }
        record_source_serial_correction_if_present(
            db,
            user.id,
            canonical_listing_id,
            &submission,
            source_serial_correction.as_ref(),
        )
        .await?;
        record_source_visual_correction_if_present(
            db,
            user.id,
            canonical_listing_id,
            &submission,
            source_visual_correction.as_ref(),
        )
        .await?;
        Ok::<PluginSubmission, PluginStoreError>(submission)
    }
    .await;
    let submission = materialized?;
    if source_serial_correction.is_some() || source_visual_correction.is_some() {
        let listing_id = canonical_listing_id.ok_or_else(|| {
            PluginStoreError::Database(
                "corrected reprocessed listing lost its canonical identifier after receipt"
                    .to_string(),
            )
        })?;
        listing = Some(
            finalize_signed_source_listing_after_receipt(db, user.id, listing_id, submission.id)
                .await?,
        );
    }

    if submission.canonical_listing_id.is_some() {
        let retained = load_checkpoint_capture(db, user.id, submission.id).await?;
        let (listing_id, extracted_listing_sha256) =
            validate_bound_signed_checkpoint(db, &retained).await?;
        record_materialization_receipt(db, &retained, listing_id, &extracted_listing_sha256)
            .await?;
    }

    Ok(PluginSubmissionOutcome {
        submission,
        preview,
        listing,
    })
}

async fn attach_submission_to_pending_review_if_needed(
    db: &AppDb,
    user: &User,
    listing: Option<&SaleListing>,
    submission: &PluginSubmission,
) -> StoreResult<()> {
    let Some(listing) = listing else {
        return Ok(());
    };
    let pending_count = query_as_one!(
        db,
        (i64,),
        "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        listing.id
    )?
    .0;
    if pending_count == 0 {
        return Ok(());
    }
    attach_pending_review_submission(db, listing.id, submission.id, user.id)
        .await
        .map_err(|error| {
            PluginStoreError::Database(format!(
                "listing review was staged but its plugin submission could not be attached: {error}"
            ))
        })
}

async fn record_source_serial_correction_if_present(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: Option<i64>,
    submission: &PluginSubmission,
    correction: Option<&FaaSerialCorrection>,
) -> StoreResult<()> {
    let Some(correction) = correction else {
        return Ok(());
    };
    let listing_id = listing_id.ok_or_else(|| {
        PluginStoreError::Database(
            "FAA source serial correction lost its materialized listing".to_string(),
        )
    })?;
    record_bound_source_serial_correction(db, owner_user_id, listing_id, submission.id, correction)
        .await
        .map_err(|error| {
            PluginStoreError::Database(format!(
                "FAA source serial correction could not be recorded: {error}"
            ))
        })?;
    Ok(())
}

async fn record_source_visual_correction_if_present(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: Option<i64>,
    submission: &PluginSubmission,
    correction: Option<&SourceVisualRegistrationCorrection>,
) -> StoreResult<()> {
    let Some(correction) = correction else {
        return Ok(());
    };
    let listing_id = listing_id.ok_or_else(|| {
        PluginStoreError::Database(
            "visual source registration correction lost its materialized listing".to_string(),
        )
    })?;
    record_bound_source_visual_correction(db, owner_user_id, listing_id, submission.id, correction)
        .await
        .map_err(|error| {
            PluginStoreError::Database(format!(
                "visual source registration correction could not be recorded: {error}"
            ))
        })?;
    Ok(())
}

async fn latest_listing_id_for_source_url(
    db: &AppDb,
    user_id: i64,
    source_url: &str,
) -> StoreResult<Option<i64>> {
    let row = query_as_optional!(
        db,
        ListingIdRow,
        r#"
        SELECT id
        FROM aircraft_sale_listings
        WHERE source_url = ?
          AND (is_verified = TRUE OR created_by_user_id = ?)
        ORDER BY added_at DESC, id DESC
        LIMIT 1
        "#,
        source_url,
        user_id
    )?;
    Ok(row.map(|row| row.id))
}

async fn exact_signed_capture_submission(
    db: &AppDb,
    user_id: i64,
    plugin_install_id: i64,
    source_url: &str,
    rendered_html_sha256: &str,
) -> StoreResult<Option<PluginSubmission>> {
    let row = query_as_optional!(
        db,
        PluginSubmissionRow,
        r#"
        SELECT id, user_id, plugin_install_id, source_url, submitted_at,
               rendered_html_sha256, signature_base64, extracted_listing_json,
               extraction_error, canonical_listing_id
        FROM plugin_submissions
        WHERE user_id = ? AND plugin_install_id = ?
          AND source_url = ? AND rendered_html_sha256 = ?
        "#,
        user_id,
        plugin_install_id,
        source_url,
        rendered_html_sha256
    )?;
    row.map(plugin_submission_from_row).transpose()
}

async fn plugin_submission_for_user(
    db: &AppDb,
    user_id: i64,
    submission_id: i64,
) -> StoreResult<PluginSubmission> {
    let row = query_as_optional!(
        db,
        PluginSubmissionRow,
        r#"
        SELECT id, user_id, plugin_install_id, source_url, submitted_at,
               rendered_html_sha256, signature_base64, extracted_listing_json,
               extraction_error, canonical_listing_id
        FROM plugin_submissions WHERE id = ? AND user_id = ?
        "#,
        submission_id,
        user_id
    )?
    .ok_or_else(|| PluginStoreError::NotFound("plugin submission not found".to_string()))?;
    plugin_submission_from_row(row)
}

async fn automatic_occurrence_disposition_count(
    db: &AppDb,
    submission_id: i64,
) -> StoreResult<i64> {
    Ok(query_as_one!(
        db,
        (i64,),
        "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_dispositions WHERE plugin_submission_id = ?",
        submission_id
    )?
    .0)
}

fn retained_listing_context(source_url: &str, rendered_html: &str) -> StoreResult<String> {
    listing_extraction_source(source_url, rendered_html).map_err(|error| {
        PluginStoreError::Validation(format!("retained listing source is invalid: {error}"))
    })
}

async fn recover_bound_source_correction(
    db: &AppDb,
    user: &User,
    submission: &PluginSubmission,
    listing_id: i64,
    extractor: Option<&GeminiListingExtractor>,
    rendered_html: &str,
) -> StoreResult<PluginSubmissionOutcome> {
    let extracted = submission.extracted_listing_json.as_ref().ok_or_else(|| {
        PluginStoreError::Validation(
            "receipt-gated signed capture has no extraction checkpoint".to_string(),
        )
    })?;
    let (parsed_listing, identity_recovery) =
        parse_current_checkpoint_payload(&extracted.to_string())?;
    let preview = ListingPreview {
        source_url: Some(submission.source_url.clone()),
        parsed_listing,
        warnings: Vec::new(),
        identity_recovery,
        context_text: Some(retained_listing_context(
            &submission.source_url,
            rendered_html,
        )?),
    };
    let existing_dispositions = automatic_occurrence_disposition_count(db, submission.id).await?;
    let source_admission = admit_aircraft_source_identity(
        db,
        preview.parsed_listing.registration_number.as_deref(),
        preview.parsed_listing.serial_number.as_deref(),
        preview.context_text.as_deref(),
    )
    .await;
    let (source_serial_correction, source_visual_correction) = match source_admission {
        Ok(admission) => {
            let correction = admission.serial_correction.ok_or_else(|| {
                PluginStoreError::Validation(
                    "receipt-gated signed capture no longer requires its FAA serial correction"
                        .to_string(),
                )
            })?;
            if existing_dispositions == 0 {
                let resumed = resume_signed_source_correction_listing(
                    db, user.id, listing_id, &preview, extractor,
                )
                .await?;
                attach_submission_to_pending_review_if_needed(
                    db,
                    user,
                    Some(&resumed.listing),
                    submission,
                )
                .await?;
                record_automatic_occurrence_dispositions(
                    db,
                    listing_id,
                    submission.id,
                    user.id,
                    &resumed.occurrence_dispositions,
                )
                .await
                .map_err(PluginStoreError::Database)?;
            }
            (Some(correction), None)
        }
        Err(AircraftAdmissionError::Rejected {
            reason: BlockReason::RegistrationNotFound,
            ..
        }) => {
            let resumed = resume_signed_source_visual_correction_listing(
                db,
                user.id,
                listing_id,
                submission.id,
                &preview,
                extractor,
                rendered_html,
                existing_dispositions == 0,
            )
            .await?;
            if existing_dispositions == 0 {
                attach_submission_to_pending_review_if_needed(
                    db,
                    user,
                    Some(&resumed.listing),
                    submission,
                )
                .await?;
                record_automatic_occurrence_dispositions(
                    db,
                    listing_id,
                    submission.id,
                    user.id,
                    &resumed.occurrence_dispositions,
                )
                .await
                .map_err(PluginStoreError::Database)?;
            }
            (None, resumed.source_visual_correction)
        }
        Err(error) => return Err(PluginStoreError::Validation(error.to_string())),
    };
    record_source_serial_correction_if_present(
        db,
        user.id,
        Some(listing_id),
        submission,
        source_serial_correction.as_ref(),
    )
    .await?;
    record_source_visual_correction_if_present(
        db,
        user.id,
        Some(listing_id),
        submission,
        source_visual_correction.as_ref(),
    )
    .await?;
    let listing =
        finalize_signed_source_listing_after_receipt(db, user.id, listing_id, submission.id)
            .await?;
    Ok(PluginSubmissionOutcome {
        submission: submission.clone(),
        preview: Some(preview),
        listing: Some(listing),
    })
}

async fn exact_source_correction_receipt_exists(
    db: &AppDb,
    owner_user_id: i64,
    submission_id: i64,
    listing_id: i64,
    observed_registration: Option<&str>,
    correction: &FaaSerialCorrection,
) -> StoreResult<bool> {
    let Some(observed_registration) = observed_registration else {
        return Ok(false);
    };
    let count = query_as_one!(
        db,
        (i64,),
        r#"
        SELECT COUNT(*)
        FROM aircraft_listing_identity_correction_decisions decision
        JOIN plugin_submissions submission
          ON submission.id = decision.plugin_submission_id
        JOIN aircraft_sale_listings listing
          ON listing.id = decision.aircraft_sale_listing_id
        WHERE decision.correction_kind = 'faa_serial'
          AND decision.plugin_submission_id = ?
          AND decision.aircraft_sale_listing_id = ?
          AND decision.prior_registration_number = ?
          AND decision.prior_serial_number = ?
          AND decision.corrected_serial_number = ?
          AND decision.rendered_html_sha256 = submission.rendered_html_sha256
          AND submission.user_id = ?
          AND submission.canonical_listing_id = listing.id
          AND submission.extraction_error IS NULL
          AND listing.created_by_user_id = ?
          AND listing.registration_number = decision.corrected_registration_number
          AND listing.serial_number = decision.corrected_serial_number
        "#,
        submission_id,
        listing_id,
        observed_registration,
        correction.observed_serial_number.as_str(),
        correction.corrected_serial_number.as_str(),
        owner_user_id,
        owner_user_id
    )?
    .0;
    Ok(count == 1)
}

fn emit_plugin_progress(progress: Option<&PluginProgressSender>, stage: &str, message: &str) {
    if let Some(progress) = progress {
        let _ = progress.send(json!({
            "stage": stage,
            "status": "running",
            "message": message,
        }));
    }
}

fn extracted_listing_payload(preview: &ListingPreview) -> Value {
    canonical_current_checkpoint_payload(
        &preview.parsed_listing,
        preview.identity_recovery.as_ref(),
    )
}

pub(crate) fn canonical_current_checkpoint_payload(
    parsed_listing: &ParsedListing,
    identity_recovery: Option<&crate::aircraft::curation::visual::VisualIdentifierResolution>,
) -> Value {
    let mut payload = json!(parsed_listing);
    if let (Some(object), Some(identity_recovery)) = (payload.as_object_mut(), identity_recovery) {
        object.insert(
            "visual_identity_recovery".to_string(),
            json!(identity_recovery),
        );
    }
    payload
}

pub(crate) fn parse_current_checkpoint_payload(
    extracted_listing_json: &str,
) -> StoreResult<(
    ParsedListing,
    Option<crate::aircraft::curation::visual::VisualIdentifierResolution>,
)> {
    let mut value: Value = serde_json::from_str(extracted_listing_json).map_err(|error| {
        PluginStoreError::Validation(format!(
            "replay extraction is not valid checkpoint JSON: {error}"
        ))
    })?;
    let object = value.as_object_mut().ok_or_else(|| {
        PluginStoreError::Validation(
            "replay extraction checkpoint must be one top-level object".to_string(),
        )
    })?;
    if let Some(field) = object
        .keys()
        .find(|field| !CURRENT_CHECKPOINT_FIELDS.contains(&field.as_str()))
    {
        return Err(PluginStoreError::Validation(format!(
            "replay extraction checkpoint contains unsupported field {field:?}"
        )));
    }
    let identity_recovery = object
        .remove("visual_identity_recovery")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            PluginStoreError::Validation(format!(
                "replay visual identity recovery is invalid: {error}"
            ))
        })?;
    let parsed_listing = serde_json::from_value(value).map_err(|error| {
        PluginStoreError::Validation(format!(
            "replay extraction is not a current listing object: {error}"
        ))
    })?;
    if let Some(recovery) = identity_recovery.as_ref() {
        validate_checkpoint_visual_recovery(&parsed_listing, recovery)?;
    }
    Ok((parsed_listing, identity_recovery))
}

fn validate_checkpoint_visual_recovery(
    parsed_listing: &ParsedListing,
    recovery: &crate::aircraft::curation::visual::VisualIdentifierResolution,
) -> StoreResult<()> {
    use crate::aircraft::curation::visual::{
        evaluate_visual_registration_consensus, VisualConsensusStatus, VisualIdentifierStatus,
    };
    if recovery.model.trim().is_empty()
        || recovery.prompt_version.trim().is_empty()
        || recovery.schema_version.trim().is_empty()
        || (recovery.status == VisualIdentifierStatus::CandidatesVisible
            && recovery.candidates.is_empty())
    {
        return Err(PluginStoreError::Validation(
            "replay visual identity recovery is incomplete".to_string(),
        ));
    }
    let photo_ids = recovery
        .photos
        .iter()
        .map(|photo| photo.image_id.as_str())
        .collect::<HashSet<_>>();
    if recovery.photos.iter().any(|photo| {
        photo.image_id.trim().is_empty()
            || photo.sha256.len() != 64
            || !photo.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) || recovery.candidates.iter().any(|candidate| {
        candidate.evidence_count != candidate.evidence.len()
            || candidate.evidence.is_empty()
            || candidate
                .evidence
                .iter()
                .any(|evidence| !photo_ids.contains(evidence.image_id.as_str()))
    }) {
        return Err(PluginStoreError::Validation(
            "replay visual identity recovery has invalid photo evidence bindings".to_string(),
        ));
    }
    let recomputed = evaluate_visual_registration_consensus(&recovery.candidates);
    if recomputed != recovery.registration_consensus {
        return Err(PluginStoreError::Validation(
            "replay visual identity recovery consensus does not match its retained evidence"
                .to_string(),
        ));
    }
    if recovery.registration_consensus.status == VisualConsensusStatus::AutoAccept {
        let visual_n_number = recovery
            .registration_consensus
            .normalized_n_number
            .as_deref()
            .and_then(crate::aircraft::faa::normalize_n_number);
        let listing_n_number = parsed_listing
            .registration_number
            .as_deref()
            .and_then(crate::aircraft::faa::normalize_n_number);
        if visual_n_number.is_none() || visual_n_number != listing_n_number {
            return Err(PluginStoreError::Validation(
                "replay visual identity recovery does not match the checkpoint registration"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

async fn extract_capture_to_current_checkpoint(
    source_url: &str,
    rendered_html: &str,
    extractor: &GeminiListingExtractor,
) -> StoreResult<(ListingPreview, Value)> {
    let extraction =
        parse_listing_html_for_avionics_validation(source_url, rendered_html, extractor).await?;
    let mut preview = extraction.preview;
    let mut payload = extracted_listing_payload(&preview);
    payload["avionics"] = extraction.raw_avionics;
    let listing_text = preview.context_text.as_deref().ok_or_else(|| {
        PluginStoreError::Validation(
            "provider-backed listing extraction requires retained listing text".to_string(),
        )
    })?;
    let validated_occurrences = validate_or_correct_listing_avionics(
        extractor,
        extraction.correction_token,
        listing_text,
        source_url,
        rendered_html,
        &mut payload,
    )
    .await
    .map_err(PluginStoreError::Validation)?;
    preview.parsed_listing.avionics = validated_occurrences;
    Ok((preview, payload))
}

/// Run only the provider-backed current-schema extraction phase for one
/// already-admitted signed capture. Aircraft identity, avionics catalog
/// resolution, listing insertion, and finalization are intentionally excluded.
pub async fn checkpoint_plugin_submission_extraction(
    db: &AppDb,
    user: &User,
    submission_id: i64,
    extractor: &GeminiListingExtractor,
) -> StoreResult<PluginExtractionCheckpoint> {
    let stored = load_checkpoint_capture(db, user.id, submission_id).await?;
    if stored.canonical_listing_id.is_some() {
        return Err(PluginStoreError::Validation(
            "extraction checkpoint requires an unbound replay capture".to_string(),
        ));
    }
    validate_stored_checkpoint_capture(db, &stored).await?;
    let (_preview, payload) =
        extract_capture_to_current_checkpoint(&stored.source_url, &stored.rendered_html, extractor)
            .await?;
    let payload_json = payload.to_string();
    let occurrences = validate_unbound_current_avionics_extraction(
        &payload_json,
        &stored.source_url,
        &stored.rendered_html,
    )
    .map_err(PluginStoreError::Validation)?;
    let payload_sha256 = sha256_hex(payload_json.as_bytes());
    store_plugin_extraction_checkpoint(db, &stored, &payload_json).await?;
    Ok(PluginExtractionCheckpoint {
        submission_id,
        rendered_html_sha256: stored.rendered_html_sha256.clone(),
        extracted_listing_sha256: payload_sha256,
        avionics_occurrence_count: occurrences.len(),
        exact_extracted_listing_json: payload_json,
        exact_capture: replay_capture_attestation(&stored, &payload.to_string()),
    })
}

async fn store_plugin_extraction_checkpoint(
    db: &AppDb,
    stored: &PluginCheckpointRow,
    payload_json: &str,
) -> StoreResult<()> {
    let lock_install_sql = db.sql(match db.backend() {
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
    let sql = db.sql(match db.backend() {
        DatabaseBackend::Sqlite(_) => {
            r#"
        UPDATE plugin_submissions
        SET extracted_listing_json = ?,
            extraction_error = NULL,
            canonical_listing_id = NULL
        WHERE id = ?
          AND user_id = ?
          AND plugin_install_id = ?
          AND source_url = ?
          AND submitted_at = ?
          AND rendered_html = ?
          AND rendered_html_sha256 = ?
          AND signature_base64 = ?
          AND extracted_listing_json IS NULL
          AND extraction_error IS NULL
          AND canonical_listing_id IS NULL
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
        SET extracted_listing_json = ?,
            extraction_error = NULL,
            canonical_listing_id = NULL
        WHERE id = ?
          AND user_id = ?
          AND plugin_install_id = ?
          AND source_url = ?
          AND submitted_at = ?
          AND rendered_html = ?
          AND rendered_html_sha256 = ?
          AND signature_base64 = ?
          AND extracted_listing_json IS NULL
          AND extraction_error IS NULL
          AND canonical_listing_id IS NULL
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
    macro_rules! exact_checkpoint_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let install_id = sqlx::query_scalar::<_, i64>(&lock_install_sql)
                .bind(stored.plugin_install_id)
                .bind(stored.user_id)
                .bind(&stored.public_key_base64)
                .bind(stored.install_revoked_at.as_deref())
                .bind(&stored.submitted_at)
                .bind(&stored.submitted_at)
                .fetch_optional(&mut *transaction)
                .await?;
            if install_id != Some(stored.plugin_install_id) {
                return Err(PluginStoreError::Database(
                    "signed capture install changed while its extraction checkpoint was being stored"
                        .to_string(),
                ));
            }
            let changed = sqlx::query(&sql)
                .bind(payload_json)
                .bind(stored.id)
                .bind(stored.user_id)
                .bind(stored.plugin_install_id)
                .bind(&stored.source_url)
                .bind(&stored.submitted_at)
                .bind(&stored.rendered_html)
                .bind(&stored.rendered_html_sha256)
                .bind(&stored.signature_base64)
                .bind(&stored.public_key_base64)
                .bind(stored.install_revoked_at.as_deref())
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if changed != 1 {
                return Err(PluginStoreError::Database(
                    "signed capture changed while its extraction checkpoint was being stored"
                        .to_string(),
                ));
            }
            transaction.commit().await?;
            Ok::<(), PluginStoreError>(())
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => exact_checkpoint_transaction!(pool)?,
        DatabaseBackend::Postgres(pool) => exact_checkpoint_transaction!(pool)?,
    };
    Ok(())
}

/// Provider-free inspection of an extraction checkpoint. This recomputes the
/// capture hash, re-verifies its signature, and re-runs the strict extraction
/// schema/evidence gates without changing the database.
pub async fn inspect_plugin_submission_extraction(
    db: &AppDb,
    user_id: i64,
    submission_id: i64,
) -> StoreResult<PluginExtractionCheckpoint> {
    let stored = load_checkpoint_capture(db, user_id, submission_id).await?;
    validate_stored_checkpoint_capture(db, &stored).await?;
    if stored.extraction_error.is_some() {
        return Err(PluginStoreError::Validation(
            "capture has an extraction error instead of a current checkpoint".to_string(),
        ));
    }
    let extracted = stored.extracted_listing_json.as_deref().ok_or_else(|| {
        PluginStoreError::Validation(
            "capture has not reached the extraction checkpoint".to_string(),
        )
    })?;
    parse_current_checkpoint_payload(extracted)?;
    let occurrences = validate_unbound_current_avionics_extraction(
        extracted,
        &stored.source_url,
        &stored.rendered_html,
    )
    .map_err(PluginStoreError::Validation)?;
    Ok(PluginExtractionCheckpoint {
        submission_id,
        rendered_html_sha256: stored.rendered_html_sha256.clone(),
        extracted_listing_sha256: sha256_hex(extracted.as_bytes()),
        avionics_occurrence_count: occurrences.len(),
        exact_extracted_listing_json: extracted.to_string(),
        exact_capture: replay_capture_attestation(&stored, extracted),
    })
}

/// Provider-free admission check for extraction-only replay. Unlike
/// inspection, a valid capture without a checkpoint is a successful result:
/// it is precisely the state that `replay-extraction --apply` may advance.
pub async fn preflight_plugin_submission_extraction(
    db: &AppDb,
    user_id: i64,
    submission_id: i64,
) -> StoreResult<PluginExtractionPreflight> {
    let stored = load_checkpoint_capture(db, user_id, submission_id).await?;
    if stored.canonical_listing_id.is_some() {
        return Err(PluginStoreError::Validation(
            "extraction checkpoint requires an unbound replay capture".to_string(),
        ));
    }
    validate_stored_checkpoint_capture(db, &stored).await?;
    if stored.extraction_error.is_some() {
        return Err(PluginStoreError::Validation(
            "capture has an extraction error instead of a replayable checkpoint".to_string(),
        ));
    }
    let current_checkpoint = match stored.extracted_listing_json.as_deref() {
        Some(extracted) => {
            let occurrences = validate_unbound_current_avionics_extraction(
                extracted,
                &stored.source_url,
                &stored.rendered_html,
            )
            .map_err(PluginStoreError::Validation)?;
            parse_current_checkpoint_payload(extracted)?;
            Some(PluginExtractionCheckpoint {
                submission_id,
                rendered_html_sha256: stored.rendered_html_sha256.clone(),
                extracted_listing_sha256: sha256_hex(extracted.as_bytes()),
                avionics_occurrence_count: occurrences.len(),
                exact_extracted_listing_json: extracted.to_string(),
                exact_capture: replay_capture_attestation(&stored, extracted),
            })
        }
        None => None,
    };
    Ok(PluginExtractionPreflight {
        submission_id,
        capture_valid: true,
        current_checkpoint,
    })
}

async fn exact_materialization_receipt_listing_id(
    db: &AppDb,
    stored: &PluginCheckpointRow,
    extracted_listing_sha256: &str,
) -> StoreResult<Option<i64>> {
    let receipt = query_as_optional!(
        db,
        MaterializationReceiptRow,
        r#"
        SELECT aircraft_sale_listing_id, rendered_html_sha256, extracted_listing_sha256
        FROM plugin_submission_materialization_receipts
        WHERE plugin_submission_id = ?
        "#,
        stored.id
    )?;
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    if stored.canonical_listing_id != Some(receipt.aircraft_sale_listing_id)
        || receipt.rendered_html_sha256 != stored.rendered_html_sha256
        || receipt.extracted_listing_sha256 != extracted_listing_sha256
    {
        return Err(PluginStoreError::Database(
            "replay materialization receipt does not match its exact bound capture".to_string(),
        ));
    }
    Ok(Some(receipt.aircraft_sale_listing_id))
}

async fn record_materialization_receipt(
    db: &AppDb,
    stored: &PluginCheckpointRow,
    listing_id: i64,
    extracted_listing_sha256: &str,
) -> StoreResult<()> {
    let lock_install_sql = db.sql(match db.backend() {
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
    let sql = db.sql(match db.backend() {
        DatabaseBackend::Sqlite(_) => {
            r#"
        INSERT INTO plugin_submission_materialization_receipts (
          plugin_submission_id, aircraft_sale_listing_id,
          rendered_html_sha256, extracted_listing_sha256
        )
        SELECT submission.id, listing.id, submission.rendered_html_sha256, ?
        FROM plugin_submissions submission
        JOIN aircraft_sale_listings listing
          ON listing.id = submission.canonical_listing_id
        WHERE submission.id = ? AND submission.user_id = ?
          AND submission.plugin_install_id = ? AND submission.source_url = ?
          AND submission.submitted_at = ? AND submission.rendered_html = ?
          AND submission.rendered_html_sha256 = ? AND submission.signature_base64 = ?
          AND submission.extracted_listing_json = ? AND submission.extraction_error IS NULL
          AND submission.canonical_listing_id = ?
          AND listing.created_by_user_id = submission.user_id
          AND listing.source_url = submission.source_url
          AND EXISTS (
            SELECT 1 FROM plugin_installs exact_install
            WHERE exact_install.id = submission.plugin_install_id
              AND exact_install.user_id = submission.user_id
              AND exact_install.public_key_base64 = ?
              AND exact_install.revoked_at IS ?
              AND julianday(submission.submitted_at) IS NOT NULL
              AND (
                exact_install.revoked_at IS NULL
                OR (
                  julianday(exact_install.revoked_at) IS NOT NULL
                  AND julianday(submission.submitted_at)
                    <= julianday(exact_install.revoked_at)
                )
              )
          )
        ON CONFLICT (plugin_submission_id) DO NOTHING
        "#
        }
        DatabaseBackend::Postgres(_) => {
            r#"
        INSERT INTO plugin_submission_materialization_receipts (
          plugin_submission_id, aircraft_sale_listing_id,
          rendered_html_sha256, extracted_listing_sha256
        )
        SELECT submission.id, listing.id, submission.rendered_html_sha256, ?
        FROM plugin_submissions submission
        JOIN aircraft_sale_listings listing
          ON listing.id = submission.canonical_listing_id
        WHERE submission.id = ? AND submission.user_id = ?
          AND submission.plugin_install_id = ? AND submission.source_url = ?
          AND submission.submitted_at = ? AND submission.rendered_html = ?
          AND submission.rendered_html_sha256 = ? AND submission.signature_base64 = ?
          AND submission.extracted_listing_json = ? AND submission.extraction_error IS NULL
          AND submission.canonical_listing_id = ?
          AND listing.created_by_user_id = submission.user_id
          AND listing.source_url = submission.source_url
          AND EXISTS (
            SELECT 1 FROM plugin_installs exact_install
            WHERE exact_install.id = submission.plugin_install_id
              AND exact_install.user_id = submission.user_id
              AND exact_install.public_key_base64 = ?
              AND exact_install.revoked_at IS NOT DISTINCT FROM ?
              AND CAST(submission.submitted_at AS TIMESTAMPTZ) IS NOT NULL
              AND (
                exact_install.revoked_at IS NULL
                OR CAST(submission.submitted_at AS TIMESTAMPTZ)
                  <= CAST(exact_install.revoked_at AS TIMESTAMPTZ)
              )
          )
        ON CONFLICT (plugin_submission_id) DO NOTHING
        "#
        }
    });
    macro_rules! exact_receipt_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let install_id = sqlx::query_scalar::<_, i64>(&lock_install_sql)
                .bind(stored.plugin_install_id)
                .bind(stored.user_id)
                .bind(&stored.public_key_base64)
                .bind(stored.install_revoked_at.as_deref())
                .bind(&stored.submitted_at)
                .bind(&stored.submitted_at)
                .fetch_optional(&mut *transaction)
                .await?;
            if install_id != Some(stored.plugin_install_id) {
                return Err(PluginStoreError::Database(
                    "signed capture install changed before materialization completion".to_string(),
                ));
            }
            let changed = sqlx::query(&sql)
                .bind(extracted_listing_sha256)
                .bind(stored.id)
                .bind(stored.user_id)
                .bind(stored.plugin_install_id)
                .bind(&stored.source_url)
                .bind(&stored.submitted_at)
                .bind(&stored.rendered_html)
                .bind(&stored.rendered_html_sha256)
                .bind(&stored.signature_base64)
                .bind(stored.extracted_listing_json.as_deref())
                .bind(listing_id)
                .bind(&stored.public_key_base64)
                .bind(stored.install_revoked_at.as_deref())
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            transaction.commit().await?;
            Ok::<u64, PluginStoreError>(changed)
        }};
    }
    let changed = match db.backend() {
        DatabaseBackend::Sqlite(pool) => exact_receipt_transaction!(pool)?,
        DatabaseBackend::Postgres(pool) => exact_receipt_transaction!(pool)?,
    };
    if changed == 0
        && exact_materialization_receipt_listing_id(db, stored, extracted_listing_sha256).await?
            != Some(listing_id)
    {
        return Err(PluginStoreError::Database(
            "replay materialization completion could not be recorded exactly".to_string(),
        ));
    }
    Ok(())
}

/// Provider-free inspection used by resumable replay coordination. Unlike the
/// extraction-only preflight, this accepts an already-bound capture so a
/// worker can reconcile a domain write that committed before its run ledger
/// transition was recorded.
pub async fn inspect_plugin_replay_capture_state(
    db: &AppDb,
    user_id: i64,
    submission_id: i64,
) -> StoreResult<PluginReplayCaptureState> {
    let stored = load_checkpoint_capture(db, user_id, submission_id).await?;
    validate_stored_checkpoint_capture(db, &stored).await?;
    if stored.extraction_error.is_some() {
        return Err(PluginStoreError::Validation(
            "capture has an extraction error instead of a replayable checkpoint".to_string(),
        ));
    }
    let checkpoint = stored
        .extracted_listing_json
        .as_deref()
        .map(|extracted| {
            parse_current_checkpoint_payload(extracted)?;
            let occurrences = validate_unbound_current_avionics_extraction(
                extracted,
                &stored.source_url,
                &stored.rendered_html,
            )
            .map_err(PluginStoreError::Validation)?;
            Ok::<_, PluginStoreError>(PluginExtractionCheckpoint {
                submission_id,
                rendered_html_sha256: stored.rendered_html_sha256.clone(),
                extracted_listing_sha256: sha256_hex(extracted.as_bytes()),
                avionics_occurrence_count: occurrences.len(),
                exact_extracted_listing_json: extracted.to_string(),
                exact_capture: replay_capture_attestation(&stored, extracted),
            })
        })
        .transpose()?;
    let materialization_receipt_listing_id = match checkpoint.as_ref() {
        Some(checkpoint) => {
            exact_materialization_receipt_listing_id(
                db,
                &stored,
                &checkpoint.extracted_listing_sha256,
            )
            .await?
        }
        None => None,
    };
    Ok(PluginReplayCaptureState {
        submission_id,
        rendered_html_sha256: stored.rendered_html_sha256,
        checkpoint,
        canonical_listing_id: stored.canonical_listing_id,
        materialization_receipt_listing_id,
    })
}

async fn validate_bound_signed_checkpoint(
    db: &AppDb,
    stored: &PluginCheckpointRow,
) -> StoreResult<(i64, String)> {
    validate_stored_checkpoint_capture(db, stored).await?;
    let listing_id = stored.canonical_listing_id.ok_or_else(|| {
        PluginStoreError::Database("signed capture lost its bound listing identifier".to_string())
    })?;
    if stored.extraction_error.is_some() {
        return Err(PluginStoreError::Validation(
            "bound signed capture has an extraction failure".to_string(),
        ));
    }
    let extracted = stored.extracted_listing_json.as_deref().ok_or_else(|| {
        PluginStoreError::Validation(
            "bound signed capture has no retained extraction checkpoint".to_string(),
        )
    })?;
    parse_current_checkpoint_payload(extracted)?;
    validate_unbound_current_avionics_extraction(
        extracted,
        &stored.source_url,
        &stored.rendered_html,
    )
    .map_err(PluginStoreError::Validation)?;
    Ok((listing_id, sha256_hex(extracted.as_bytes())))
}

async fn recover_or_return_bound_signed_submission(
    db: &AppDb,
    user: &User,
    submission_id: i64,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<PluginSubmissionOutcome> {
    let stored = load_checkpoint_capture(db, user.id, submission_id).await?;
    let (listing_id, extracted_listing_sha256) =
        validate_bound_signed_checkpoint(db, &stored).await?;
    let listing =
        if exact_materialization_receipt_listing_id(db, &stored, &extracted_listing_sha256).await?
            == Some(listing_id)
        {
            get_listing(db, user.id, listing_id).await?
        } else {
            complete_bound_replay_materialization(
                db,
                user,
                &stored,
                &extracted_listing_sha256,
                extractor,
            )
            .await?
        };
    Ok(PluginSubmissionOutcome {
        submission: plugin_submission_for_user(db, user.id, submission_id).await?,
        preview: None,
        listing: Some(listing),
    })
}

async fn complete_bound_replay_materialization(
    db: &AppDb,
    user: &User,
    stored: &PluginCheckpointRow,
    extracted_listing_sha256: &str,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<SaleListing> {
    validate_stored_checkpoint_capture(db, stored).await?;
    let listing_id = stored.canonical_listing_id.ok_or_else(|| {
        PluginStoreError::Database("bound replay recovery lost its listing identifier".to_string())
    })?;
    if exact_materialization_receipt_listing_id(db, stored, extracted_listing_sha256).await?
        == Some(listing_id)
    {
        return get_listing(db, user.id, listing_id)
            .await
            .map_err(PluginStoreError::from);
    }

    let submission = plugin_submission_for_user(db, user.id, stored.id).await?;
    let current = get_listing(db, user.id, listing_id).await?;
    let listing = if current.ingestion_error.as_deref() == Some(SOURCE_IDENTITY_RECEIPT_PENDING) {
        Box::pin(recover_bound_source_correction(
            db,
            user,
            &submission,
            listing_id,
            extractor,
            &stored.rendered_html,
        ))
        .await?
        .listing
        .ok_or_else(|| {
            PluginStoreError::Database("recovered replay lost its corrected listing".to_string())
        })?
    } else if automatic_occurrence_disposition_count(db, stored.id).await? == 0 {
        let extracted = stored.extracted_listing_json.as_deref().ok_or_else(|| {
            PluginStoreError::Validation(
                "bound replay capture has no extraction checkpoint".to_string(),
            )
        })?;
        let (parsed_listing, identity_recovery) = parse_current_checkpoint_payload(extracted)?;
        let preview = ListingPreview {
            source_url: Some(stored.source_url.clone()),
            parsed_listing,
            warnings: Vec::new(),
            identity_recovery,
            context_text: Some(retained_listing_context(
                &stored.source_url,
                &stored.rendered_html,
            )?),
        };
        let resumed = resume_bound_replay_listing(
            db,
            user.id,
            listing_id,
            stored.id,
            &stored.rendered_html_sha256,
            extracted_listing_sha256,
            &preview,
            extractor,
        )
        .await?;
        attach_submission_to_pending_review_if_needed(
            db,
            user,
            Some(&resumed.listing),
            &submission,
        )
        .await?;
        record_automatic_occurrence_dispositions(
            db,
            listing_id,
            stored.id,
            user.id,
            &resumed.occurrence_dispositions,
        )
        .await
        .map_err(PluginStoreError::Database)?;
        resumed.listing
    } else {
        current
    };
    record_materialization_receipt(db, stored, listing_id, extracted_listing_sha256).await?;
    Ok(listing)
}

/// Materialize one exact extraction checkpoint through the ordinary listing
/// admission, aircraft, avionics, review, and finalization workflow. Listing
/// extraction is never called. The listing insert and capture binding are one
/// transaction; an exact completion receipt is written only after every child
/// projection has finished, so a bound partial commit is resumed rather than
/// mistaken for completion.
pub async fn materialize_plugin_submission_checkpoint(
    db: &AppDb,
    user: &User,
    submission_id: i64,
    expected_extracted_listing_sha256: &str,
    extractor: &GeminiListingExtractor,
) -> StoreResult<PluginListingReplayOutcome> {
    let stored = load_checkpoint_capture(db, user.id, submission_id).await?;
    let extracted_listing_json = stored.extracted_listing_json.as_deref().ok_or_else(|| {
        PluginStoreError::Validation(
            "replay capture has not reached the extraction checkpoint".to_string(),
        )
    })?;
    if sha256_hex(extracted_listing_json.as_bytes()) != expected_extracted_listing_sha256 {
        return Err(PluginStoreError::Validation(
            "replay extraction checkpoint does not match the pinned checkpoint hash".to_string(),
        ));
    }
    validate_stored_checkpoint_capture(db, &stored).await?;
    if stored.canonical_listing_id.is_some() {
        let listing = complete_bound_replay_materialization(
            db,
            user,
            &stored,
            expected_extracted_listing_sha256,
            Some(extractor),
        )
        .await?;
        return Ok(PluginListingReplayOutcome::Materialized {
            submission_id,
            listing,
        });
    }
    if stored.extraction_error.is_some() {
        return Err(PluginStoreError::Validation(
            "replay capture has an extraction error".to_string(),
        ));
    }
    preflight_replay_source_claim(db, stored.user_id, &stored.source_url).await?;
    validate_unbound_current_avionics_extraction(
        extracted_listing_json,
        &stored.source_url,
        &stored.rendered_html,
    )
    .map_err(PluginStoreError::Validation)?;
    let (parsed_listing, identity_recovery) =
        parse_current_checkpoint_payload(extracted_listing_json)?;
    let preview = ListingPreview {
        source_url: Some(stored.source_url.clone()),
        parsed_listing,
        warnings: Vec::new(),
        identity_recovery,
        context_text: Some(retained_listing_context(
            &stored.source_url,
            &stored.rendered_html,
        )?),
    };
    let signed_source_binding = signed_source_listing_binding(
        stored.id,
        stored.user_id,
        stored.plugin_install_id,
        &stored.public_key_base64,
        stored.install_revoked_at.as_deref(),
        &stored.source_url,
        &stored.submitted_at,
        &stored.rendered_html,
        &stored.rendered_html_sha256,
        &stored.signature_base64,
        Some(extracted_listing_json.to_string()),
        stored.extraction_error.clone(),
        extracted_listing_json.to_string(),
    );
    let creation = Box::pin(create_listing_with_progress_and_occurrence_dispositions(
        db,
        user.id,
        &preview,
        None,
        Some(extractor),
        None,
        ListingCreationMode::CreateOnly,
        Some(&signed_source_binding),
    ))
    .await;
    if let Err(error) = &creation {
        let retained = load_checkpoint_capture(db, user.id, stored.id).await?;
        if retained.canonical_listing_id.is_some() {
            return complete_bound_replay_materialization(
                db,
                user,
                &retained,
                expected_extracted_listing_sha256,
                Some(extractor),
            )
            .await
            .map(|listing| PluginListingReplayOutcome::Materialized {
                submission_id,
                listing,
            })
            .map_err(|recovery| {
                PluginStoreError::Database(format!(
                    "replay stopped after its atomic listing binding ({error}); deterministic recovery is pending: {recovery}"
                ))
            });
        }
    }
    let created = match creation {
        Ok(created) => created,
        Err(ListingStoreError::AircraftAdmission(error)) => {
            return match classify_replay_aircraft_admission(error) {
                Ok(rejection) => Ok(PluginListingReplayOutcome::Rejected {
                    submission_id,
                    rejection,
                }),
                Err(blocked) => Err(PluginStoreError::AdmissionBlocked(blocked)),
            };
        }
        Err(ListingStoreError::Validation(reason) | ListingStoreError::State(reason)) => {
            return Err(PluginStoreError::Validation(reason));
        }
        Err(error) => return Err(error.into()),
    };
    let listing_id = created.listing.id;
    let source_serial_correction = created.source_serial_correction.clone();
    let source_visual_correction = created.source_visual_correction.clone();
    let materialized = async {
        let bound_submission = PluginSubmission {
            id: stored.id,
            user_id: stored.user_id,
            plugin_install_id: stored.plugin_install_id,
            source_url: stored.source_url.clone(),
            submitted_at: stored.submitted_at.clone(),
            rendered_html_sha256: stored.rendered_html_sha256.clone(),
            signature_base64: stored.signature_base64.clone(),
            extracted_listing_json: Some(serde_json::from_str(extracted_listing_json).map_err(
                |error| {
                    PluginStoreError::Database(format!(
                        "stored extraction became invalid while binding replay: {error}"
                    ))
                },
            )?),
            extraction_error: None,
            canonical_listing_id: Some(listing_id),
        };
        let listing = get_listing(db, user.id, listing_id).await?;
        attach_submission_to_pending_review_if_needed(db, user, Some(&listing), &bound_submission)
            .await?;
        record_automatic_occurrence_dispositions(
            db,
            listing_id,
            stored.id,
            user.id,
            &created.occurrence_dispositions,
        )
        .await
        .map_err(PluginStoreError::Database)?;
        record_source_serial_correction_if_present(
            db,
            user.id,
            Some(listing_id),
            &bound_submission,
            source_serial_correction.as_ref(),
        )
        .await?;
        record_source_visual_correction_if_present(
            db,
            user.id,
            Some(listing_id),
            &bound_submission,
            source_visual_correction.as_ref(),
        )
        .await?;
        Ok::<(), PluginStoreError>(())
    }
    .await;
    materialized?;
    let listing = if source_serial_correction.is_some() || source_visual_correction.is_some() {
        finalize_signed_source_listing_after_receipt(db, user.id, listing_id, stored.id).await?
    } else {
        get_listing(db, user.id, listing_id).await?
    };
    record_materialization_receipt(db, &stored, listing_id, expected_extracted_listing_sha256)
        .await?;
    Ok(PluginListingReplayOutcome::Materialized {
        submission_id,
        listing,
    })
}

// This read avoids unnecessary FAA/catalog work for an obvious conflict. It is
// only an optimization: the owner/source unique index remains the atomic claim
// that decides concurrent inserts.
async fn preflight_replay_source_claim(
    db: &AppDb,
    owner_user_id: i64,
    source_url: &str,
) -> StoreResult<()> {
    let existing = query_as_optional!(
        db,
        ListingIdRow,
        r#"
        SELECT id FROM aircraft_sale_listings
        WHERE created_by_user_id = ? AND source_url = ?
        ORDER BY id LIMIT 1
        "#,
        owner_user_id,
        source_url
    )?;
    if existing.is_some() {
        return Err(PluginStoreError::Validation(
            "listing source is already claimed by this owner".to_string(),
        ));
    }
    Ok(())
}

fn classify_replay_aircraft_admission(
    error: AircraftAdmissionError,
) -> Result<PluginReplayTerminalRejection, PluginReplayAdmissionBlock> {
    match error {
        AircraftAdmissionError::LookupFailed { .. } => {
            Err(PluginReplayAdmissionBlock::LookupFailed)
        }
        AircraftAdmissionError::ListingNotFound { .. } => {
            Err(PluginReplayAdmissionBlock::ListingNotFound)
        }
        AircraftAdmissionError::Rejected { reason, .. } => match reason {
            BlockReason::MissingRegistration => {
                Ok(PluginReplayTerminalRejection::MissingRegistration)
            }
            BlockReason::NonNRegistration => Ok(PluginReplayTerminalRejection::NonNRegistration),
            BlockReason::InvalidNNumber => Ok(PluginReplayTerminalRejection::InvalidNNumber),
            BlockReason::SerialConflict => Ok(PluginReplayTerminalRejection::SerialConflict),
            BlockReason::RegistrySnapshotUnavailable => {
                Err(PluginReplayAdmissionBlock::RegistrySnapshotUnavailable)
            }
            BlockReason::RegistrationNotFound => {
                Err(PluginReplayAdmissionBlock::RegistrationNotFound)
            }
            BlockReason::RegistrationNotCovered => {
                Err(PluginReplayAdmissionBlock::RegistrationNotCovered)
            }
            BlockReason::AmbiguousRegistration => {
                Err(PluginReplayAdmissionBlock::AmbiguousRegistration)
            }
            BlockReason::RegistryAircraftIdentityUnavailable => {
                Err(PluginReplayAdmissionBlock::RegistryAircraftIdentityUnavailable)
            }
            BlockReason::AircraftManufacturerMismatch => {
                Err(PluginReplayAdmissionBlock::AircraftManufacturerMismatch)
            }
            BlockReason::AircraftModelMismatch => {
                Err(PluginReplayAdmissionBlock::AircraftModelMismatch)
            }
            BlockReason::CanonicalIdentityAssignmentMissing => {
                Err(PluginReplayAdmissionBlock::CanonicalIdentityAssignmentMissing)
            }
            BlockReason::CanonicalIdentityAssignmentMismatch => {
                Err(PluginReplayAdmissionBlock::CanonicalIdentityAssignmentMismatch)
            }
        },
    }
}

async fn load_checkpoint_capture(
    db: &AppDb,
    user_id: i64,
    submission_id: i64,
) -> StoreResult<PluginCheckpointRow> {
    query_as_optional!(
        db,
        PluginCheckpointRow,
        r#"
        SELECT submission.id,
               submission.user_id,
               submission.plugin_install_id,
               install.public_key_base64,
               install.revoked_at AS install_revoked_at,
               submission.source_url,
               submission.submitted_at,
               submission.rendered_html,
               submission.rendered_html_sha256,
               submission.signature_base64,
               submission.extracted_listing_json,
               submission.extraction_error,
               submission.canonical_listing_id
        FROM plugin_submissions submission
        JOIN plugin_installs install ON install.id = submission.plugin_install_id
        WHERE submission.id = ?
          AND submission.user_id = ?
          AND install.user_id = submission.user_id
        "#,
        submission_id,
        user_id
    )?
    .ok_or_else(|| PluginStoreError::NotFound("plugin submission not found".to_string()))
}

async fn validate_stored_checkpoint_capture(
    db: &AppDb,
    stored: &PluginCheckpointRow,
) -> StoreResult<()> {
    validate_source_url(&stored.source_url)?;
    if stored.rendered_html.trim().is_empty()
        || stored.rendered_html.len() > MAX_RENDERED_HTML_BYTES
    {
        return Err(PluginStoreError::Validation(
            "stored rendered HTML is empty or exceeds the admission limit".to_string(),
        ));
    }
    validate_observation_timestamp(&stored.submitted_at)?;
    let recomputed = sha256_hex(stored.rendered_html.as_bytes());
    if recomputed != stored.rendered_html_sha256 {
        return Err(PluginStoreError::Permission(
            "stored rendered HTML does not match its signed SHA-256".to_string(),
        ));
    }
    verify_submission_signature(
        &stored.public_key_base64,
        stored.plugin_install_id,
        &stored.source_url,
        &recomputed,
        &stored.signature_base64,
    )?;
    if let Some(revoked_at) = stored.install_revoked_at.as_deref() {
        let capture_precedes_revocation = match db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"SELECT
                     julianday(?) IS NOT NULL
                     AND julianday(?) IS NOT NULL
                     AND julianday(?) <= julianday(?)"#,
                )
                .bind(&stored.submitted_at)
                .bind(revoked_at)
                .bind(&stored.submitted_at)
                .bind(revoked_at)
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"SELECT
                     CAST($1 AS TIMESTAMPTZ) <= CAST($2 AS TIMESTAMPTZ)"#,
                )
                .bind(&stored.submitted_at)
                .bind(revoked_at)
                .fetch_one(pool)
                .await?
            }
        };
        if !capture_precedes_revocation {
            return Err(PluginStoreError::Permission(
                "signed capture was submitted after its plugin install was revoked".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_observation_timestamp(value: &str) -> StoreResult<()> {
    let bytes = value.as_bytes();
    let date_prefix_is_valid = bytes.len() >= 19
        && bytes.len() <= 40
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && matches!(bytes.get(10), Some(b' ' | b'T'))
        && bytes.get(13) == Some(&b':')
        && bytes.get(16) == Some(&b':')
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16) || index > 18 || byte.is_ascii_digit()
        });
    if !date_prefix_is_valid || value.chars().any(char::is_control) {
        return Err(PluginStoreError::Validation(
            "stored submission observation timestamp is invalid".to_string(),
        ));
    }
    Ok(())
}

pub fn signature_message(
    plugin_install_id: i64,
    source_url: &str,
    rendered_html_sha256: &str,
) -> String {
    format!("{SIGNATURE_PREFIX}\n{plugin_install_id}\n{source_url}\n{rendered_html_sha256}")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = digest::digest(&digest::SHA256, bytes);
    hex_encode(digest.as_ref())
}

#[allow(clippy::too_many_arguments)]
fn signed_source_listing_binding(
    submission_id: i64,
    user_id: i64,
    plugin_install_id: i64,
    install_public_key_base64: &str,
    install_revoked_at: Option<&str>,
    source_url: &str,
    submitted_at: &str,
    rendered_html: &str,
    rendered_html_sha256: &str,
    signature_base64: &str,
    expected_extracted_listing_json: Option<String>,
    expected_extraction_error: Option<String>,
    bound_extracted_listing_json: String,
) -> SignedSourceListingBinding {
    let expected_extracted_listing_sha256 = expected_extracted_listing_json
        .as_deref()
        .map(|checkpoint| sha256_hex(checkpoint.as_bytes()));
    let bound_extracted_listing_sha256 = sha256_hex(bound_extracted_listing_json.as_bytes());
    SignedSourceListingBinding {
        submission_id,
        user_id,
        plugin_install_id,
        install_public_key_base64: install_public_key_base64.to_string(),
        install_revoked_at: install_revoked_at.map(str::to_string),
        source_url: source_url.to_string(),
        submitted_at: submitted_at.to_string(),
        rendered_html: rendered_html.to_string(),
        rendered_html_sha256: rendered_html_sha256.to_string(),
        signature_base64: signature_base64.to_string(),
        expected_extracted_listing_json,
        expected_extracted_listing_sha256,
        expected_extraction_error,
        bound_extracted_listing_json,
        bound_extracted_listing_sha256,
    }
}

fn validate_public_key(public_key_base64: &str) -> StoreResult<()> {
    let bytes = decode_base64(public_key_base64, "public_key_base64")?;
    if bytes.len() != 65 || bytes.first() != Some(&0x04) {
        return Err(PluginStoreError::Validation(
            "public_key_base64 must be a raw uncompressed P-256 public key".to_string(),
        ));
    }
    Ok(())
}

fn validate_submission_request(request: &PluginSubmissionRequest) -> StoreResult<()> {
    validate_source_url(&request.source_url)?;
    if request.rendered_html.trim().is_empty() {
        return Err(PluginStoreError::Validation(
            "rendered_html cannot be empty".to_string(),
        ));
    }
    if request.rendered_html.len() > MAX_RENDERED_HTML_BYTES {
        return Err(PluginStoreError::Validation(format!(
            "rendered_html cannot exceed {MAX_RENDERED_HTML_BYTES} bytes"
        )));
    }
    if request.signature.trim().is_empty() {
        return Err(PluginStoreError::Validation(
            "signature cannot be empty".to_string(),
        ));
    }
    Ok(())
}

async fn plugin_install_for_user(
    db: &AppDb,
    user_id: i64,
    plugin_install_id: i64,
) -> StoreResult<PluginInstall> {
    let install = query_as_optional!(
        db,
        PluginInstall,
        r#"
        SELECT id, user_id, public_key_base64, created_at, revoked_at
        FROM plugin_installs
        WHERE id = ? AND user_id = ? AND revoked_at IS NULL
        "#,
        plugin_install_id,
        user_id
    )?;
    install.ok_or_else(|| {
        PluginStoreError::Permission(
            "plugin install is unknown, revoked, or belongs to another user".to_string(),
        )
    })
}

pub(crate) fn verify_submission_signature(
    public_key_base64: &str,
    plugin_install_id: i64,
    source_url: &str,
    rendered_html_sha256: &str,
    signature_base64: &str,
) -> StoreResult<()> {
    let public_key = decode_base64(public_key_base64, "stored public_key_base64")?;
    let signature = decode_base64(signature_base64, "signature")?;
    let message = signature_message(plugin_install_id, source_url, rendered_html_sha256);
    let verifier = UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, public_key);
    verifier
        .verify(message.as_bytes(), &signature)
        .map_err(|_| PluginStoreError::Permission("invalid plugin signature".to_string()))
}

async fn update_plugin_submission_result(
    db: &AppDb,
    user_id: i64,
    submission_id: i64,
    extracted_listing_json: Option<&Value>,
    extraction_error: Option<&str>,
    canonical_listing_id: Option<i64>,
) -> StoreResult<PluginSubmission> {
    let extracted_listing_json = extracted_listing_json.map(Value::to_string);
    let row = query_as_one!(
        db,
        PluginSubmissionRow,
        r#"
        UPDATE plugin_submissions
        SET
          extracted_listing_json = COALESCE(?, extracted_listing_json),
          extraction_error = ?,
          canonical_listing_id = COALESCE(?, canonical_listing_id)
        WHERE id = ? AND user_id = ?
        RETURNING
          id,
          user_id,
          plugin_install_id,
          source_url,
          submitted_at,
          rendered_html_sha256,
          signature_base64,
          extracted_listing_json,
          extraction_error,
          canonical_listing_id
        "#,
        extracted_listing_json.as_deref(),
        extraction_error,
        canonical_listing_id,
        submission_id,
        user_id
    )?;
    plugin_submission_from_row(row)
}

#[allow(clippy::too_many_arguments)]
async fn insert_plugin_submission(
    db: &AppDb,
    user_id: i64,
    plugin_install_id: i64,
    source_url: &str,
    rendered_html: &str,
    rendered_html_sha256: &str,
    signature_base64: &str,
    extracted_listing_json: Option<&Value>,
    extraction_error: Option<&str>,
    canonical_listing_id: Option<i64>,
) -> StoreResult<PluginSubmission> {
    let extracted_listing_json = extracted_listing_json.map(Value::to_string);
    let row = query_as_one!(
        db,
        PluginSubmissionRow,
        r#"
        INSERT INTO plugin_submissions (
          user_id,
          plugin_install_id,
          source_url,
          rendered_html,
          rendered_html_sha256,
          signature_base64,
          extracted_listing_json,
          extraction_error,
          canonical_listing_id
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        RETURNING
          id,
          user_id,
          plugin_install_id,
          source_url,
          submitted_at,
          rendered_html_sha256,
          signature_base64,
          extracted_listing_json,
          extraction_error,
          canonical_listing_id
        "#,
        user_id,
        plugin_install_id,
        source_url,
        rendered_html,
        rendered_html_sha256,
        signature_base64,
        extracted_listing_json.as_deref(),
        extraction_error,
        canonical_listing_id
    )?;
    plugin_submission_from_row(row)
}

fn plugin_submission_from_row(row: PluginSubmissionRow) -> StoreResult<PluginSubmission> {
    let extracted_listing_json = match row.extracted_listing_json {
        Some(value) => Some(serde_json::from_str(&value).map_err(|error| {
            PluginStoreError::Database(format!("stored extracted listing JSON is invalid: {error}"))
        })?),
        None => None,
    };
    Ok(PluginSubmission {
        id: row.id,
        user_id: row.user_id,
        plugin_install_id: row.plugin_install_id,
        source_url: row.source_url,
        submitted_at: row.submitted_at,
        rendered_html_sha256: row.rendered_html_sha256,
        signature_base64: row.signature_base64,
        extracted_listing_json,
        extraction_error: row.extraction_error,
        canonical_listing_id: row.canonical_listing_id,
    })
}

fn decode_base64(value: &str, field_name: &str) -> StoreResult<Vec<u8>> {
    BASE64_STANDARD
        .decode(value.trim())
        .map_err(|_| PluginStoreError::Validation(format!("{field_name} must be base64")))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine as _;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use serde_json::{json, Value};
    use sha2::{Digest as Sha2Digest, Sha256};
    use tokio::net::TcpListener;

    use super::{
        classify_replay_aircraft_admission, extract_capture_to_current_checkpoint,
        load_checkpoint_capture,
        materialize_plugin_submission_checkpoint as materialize_pinned_checkpoint,
        parse_current_checkpoint_payload, reprocess_plugin_submission, sha256_hex,
        signature_message, store_plugin_extraction_checkpoint, submit_plugin_html,
        update_plugin_submission_result, validate_stored_checkpoint_capture,
        verify_submission_signature, ListingIdRow, PluginListingReplayOutcome,
        PluginReplayAdmissionBlock, PluginReplayTerminalRejection, PluginStoreError, StoreResult,
    };
    use crate::aircraft::faa::{
        require_listing_faa_admission, store_release, AircraftAdmissionError, AircraftRecord,
        AircraftReference, BlockReason, MemberProvenance, Release, ReleaseFixtureBuilder,
        ReleaseMetadata, TargetCoverage,
    };
    use crate::avionics::catalog::{
        approved_avionics_identity_for_grounded_replay,
        grounded_resolution_receipt_basis_for_replay, AvionicsIdentityRequest,
    };
    use crate::avionics::fingerprint::{
        catalog_product_fingerprint_for_id, grounded_collision_closure_revision_sha256,
    };
    use crate::avionics::manufacturer::{
        ensure_manufacturer_identity, ManufacturerIdentityEvidence,
    };
    use crate::db::{AppDb, DatabaseBackend};
    use crate::gemini::usage::{SourceCorrelation, Store as UsageStore};
    use crate::models::{PluginSubmissionRequest, User};

    const CONTROLLER_ROLE_LISTING_URL: &str =
        "https://www.controller.com/listing/for-sale/252742967/example";

    async fn materialize_plugin_submission_checkpoint(
        db: &AppDb,
        user: &User,
        submission_id: i64,
        extractor: &crate::extract::GeminiListingExtractor,
    ) -> StoreResult<PluginListingReplayOutcome> {
        let checkpoint =
            super::inspect_plugin_submission_extraction(db, user.id, submission_id).await?;
        materialize_pinned_checkpoint(
            db,
            user,
            submission_id,
            &checkpoint.extracted_listing_sha256,
            extractor,
        )
        .await
    }

    fn replay_admission_error(reason: BlockReason) -> AircraftAdmissionError {
        AircraftAdmissionError::Rejected {
            listing_id: Some(23),
            reason,
            n_number: Some("N182PF".to_string()),
            snapshot_id: Some(2),
        }
    }

    #[test]
    fn replay_admission_classification_is_structural_and_closed() {
        for (reason, expected) in [
            (
                BlockReason::MissingRegistration,
                PluginReplayTerminalRejection::MissingRegistration,
            ),
            (
                BlockReason::NonNRegistration,
                PluginReplayTerminalRejection::NonNRegistration,
            ),
            (
                BlockReason::InvalidNNumber,
                PluginReplayTerminalRejection::InvalidNNumber,
            ),
            (
                BlockReason::SerialConflict,
                PluginReplayTerminalRejection::SerialConflict,
            ),
        ] {
            assert_eq!(
                classify_replay_aircraft_admission(replay_admission_error(reason)),
                Ok(expected)
            );
        }
        for (reason, expected) in [
            (
                BlockReason::RegistrySnapshotUnavailable,
                PluginReplayAdmissionBlock::RegistrySnapshotUnavailable,
            ),
            (
                BlockReason::RegistrationNotFound,
                PluginReplayAdmissionBlock::RegistrationNotFound,
            ),
            (
                BlockReason::RegistrationNotCovered,
                PluginReplayAdmissionBlock::RegistrationNotCovered,
            ),
            (
                BlockReason::AmbiguousRegistration,
                PluginReplayAdmissionBlock::AmbiguousRegistration,
            ),
            (
                BlockReason::RegistryAircraftIdentityUnavailable,
                PluginReplayAdmissionBlock::RegistryAircraftIdentityUnavailable,
            ),
            (
                BlockReason::AircraftManufacturerMismatch,
                PluginReplayAdmissionBlock::AircraftManufacturerMismatch,
            ),
            (
                BlockReason::AircraftModelMismatch,
                PluginReplayAdmissionBlock::AircraftModelMismatch,
            ),
            (
                BlockReason::CanonicalIdentityAssignmentMissing,
                PluginReplayAdmissionBlock::CanonicalIdentityAssignmentMissing,
            ),
            (
                BlockReason::CanonicalIdentityAssignmentMismatch,
                PluginReplayAdmissionBlock::CanonicalIdentityAssignmentMismatch,
            ),
        ] {
            assert_eq!(
                classify_replay_aircraft_admission(replay_admission_error(reason)),
                Err(expected)
            );
        }
        assert_eq!(
            classify_replay_aircraft_admission(AircraftAdmissionError::LookupFailed {
                listing_id: Some(23),
                message: "temporary registry error".to_string(),
            }),
            Err(PluginReplayAdmissionBlock::LookupFailed)
        );
        assert_eq!(
            classify_replay_aircraft_admission(AircraftAdmissionError::ListingNotFound {
                listing_id: 23,
            }),
            Err(PluginReplayAdmissionBlock::ListingNotFound)
        );
    }

    #[tokio::test]
    async fn extraction_checkpoint_is_immutable_after_the_first_compare_and_set() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'test-key') RETURNING id",
        )
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let submission_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, rendered_html,
                 rendered_html_sha256, signature_base64
               ) VALUES (?, ?, 'https://example.test/checkpoint-cas', '<html></html>', ?, 'signature')
               RETURNING id"#,
        )
        .bind(user.id)
        .bind(install_id)
        .bind("a".repeat(64))
        .fetch_one(pool)
        .await
        .unwrap();
        let stale_snapshot = load_checkpoint_capture(&db, user.id, submission_id)
            .await
            .unwrap();
        store_plugin_extraction_checkpoint(&db, &stale_snapshot, "{\"winner\":true}")
            .await
            .unwrap();
        let error =
            store_plugin_extraction_checkpoint(&db, &stale_snapshot, "{\"stale_worker\":true}")
                .await
                .unwrap_err();
        assert!(matches!(error, PluginStoreError::Database(_)));
        let stored: String = sqlx::query_scalar(
            "SELECT extracted_listing_json FROM plugin_submissions WHERE id = ?",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored, "{\"winner\":true}");
    }

    fn serial_correction_extraction() -> Value {
        json!({
            "manufacturer":"Cessna","model":"182","variant":"182T","model_year":2020,
            "asking_price_usd":400000.0,"currency":"USD","airframe_hours":900.0,
            "engine_hours":null,"engine_time_basis":"unknown","engine_time_evidence":null,
            "engine_time_confidence":null,"propeller_hours":null,
            "propeller_time_basis":"unknown","propeller_time_evidence":null,
            "propeller_time_confidence":null,"installed_engine":null,"installed_propeller":null,
            "registration_number":"N482TW","serial_number":"1823006","status":"active",
            "avionics":[],"valuation_facts":[]
        })
    }

    async fn extraction_handler(State(extraction): State<Value>) -> Json<Value> {
        Json(json!({
            "candidates": [{"content": {"parts": [{"text": extraction.to_string()}]}}],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 10}
        }))
    }

    async fn extraction_endpoint(extraction: Value) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", post(extraction_handler))
            .with_state(extraction);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/")
    }

    #[derive(Clone)]
    struct ExtractionSequenceState {
        responses: Arc<Vec<String>>,
        request_count: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<Value>>>,
    }

    async fn extraction_sequence_handler(
        State(state): State<ExtractionSequenceState>,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        let index = state.request_count.fetch_add(1, Ordering::SeqCst);
        state.requests.lock().unwrap().push(request);
        let content = state
            .responses
            .get(index)
            .cloned()
            .unwrap_or_else(|| "unexpected third listing extraction request".to_string());
        Json(json!({
            "candidates": [{"content": {"parts": [{"text": content}]}}],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 10,
                "totalTokenCount": 20
            }
        }))
    }

    async fn extraction_sequence_endpoint(
        responses: Vec<String>,
    ) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = ExtractionSequenceState {
            responses: Arc::new(responses),
            request_count: request_count.clone(),
            requests: requests.clone(),
        };
        let app = Router::new()
            .route("/", post(extraction_sequence_handler))
            .with_state(state);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}/"), request_count, requests)
    }

    fn listing_extraction_with_avionics(avionics: Value) -> Value {
        let mut extraction = serial_correction_extraction();
        extraction["registration_number"] = Value::Null;
        extraction["serial_number"] = Value::Null;
        extraction["avionics"] = avionics;
        extraction
    }

    fn installed_avionics(
        manufacturer: &str,
        model: &str,
        types: &[&str],
        quantity: i64,
        evidence: &str,
    ) -> Value {
        json!({
            "manufacturer": manufacturer,
            "model": model,
            "types": types,
            "quantity": quantity,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": evidence,
            "source_confidence": "high"
        })
    }

    fn controller_avionics_html(avionics: &str, autopilot: &str) -> String {
        format!(
            r#"<html><body>
            <main id="main-content" class="detail__main-content">
              <h1 class="detail__title">2020 CESSNA 182T</h1>
              <div class="listing-prices">
                <strong class="listing-prices__retail-price">$400,000</strong>
              </div>
              <div class="detail__specs">
                <h3 class="detail__specs-heading">General</h3>
                <div class="detail__specs-wrapper">
                  <div class="detail__specs-label">Year</div>
                  <div class="detail__specs-value">2020</div>
                  <div class="detail__specs-label">Manufacturer</div>
                  <div class="detail__specs-value">CESSNA</div>
                  <div class="detail__specs-label">Model</div>
                  <div class="detail__specs-value">182T</div>
                  <div class="detail__specs-label">Condition</div>
                  <div class="detail__specs-value">Used</div>
                </div>
                <h3 class="detail__specs-heading">Avionics</h3>
                <div class="detail__specs-wrapper">
                  <div class="detail__specs-label">Avionics/Radios</div>
                  <div class="detail__specs-value">{avionics}</div>
                  <div class="detail__specs-label">SVT</div>
                  <div class="detail__specs-value">Yes</div>
                  <div class="detail__specs-label">Autopilot</div>
                  <div class="detail__specs-value">{autopilot}</div>
                </div>
              </div>
            </main>
            </body></html>"#
        )
    }

    #[tokio::test]
    async fn valid_primary_listing_avionics_uses_one_lite_request() {
        let avionics = json!([installed_avionics(
            "Garmin",
            "G1000 NXi",
            &["Integrated Flight Deck"],
            1,
            "GARMIN G1000 NXI",
        )]);
        let primary = listing_extraction_with_avionics(avionics.clone());
        let (endpoint, request_count, requests) =
            extraction_sequence_endpoint(vec![primary.to_string()]).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let html = controller_avionics_html("GARMIN G1000 NXI", "GFC700");

        let (preview, payload) =
            extract_capture_to_current_checkpoint(CONTROLLER_ROLE_LISTING_URL, &html, &extractor)
                .await
                .unwrap();

        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert_eq!(extractor.listing_visual_recovery_call_count(), 1);
        assert_eq!(payload["avionics"], avionics);
        assert_eq!(preview.parsed_listing.avionics.len(), 1);
        assert_eq!(preview.parsed_listing.avionics[0].model, "G1000 NXi");

        let requests = requests.lock().unwrap();
        let request = &requests[0];
        let prompt = request["contents"][0]["parts"][0]["text"].as_str().unwrap();
        assert!(prompt.contains("satisfies the enforced response schema"));
        assert!(!prompt.contains("Return JSON with exactly this shape"));
        let response_schema = &request["generationConfig"]["responseSchema"];
        assert_eq!(response_schema["type"], "object");
        assert_eq!(
            response_schema["properties"]["asking_price_usd"]["type"],
            "number"
        );
        assert_eq!(response_schema["properties"]["avionics"]["type"], "array");
        assert!(response_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "valuation_facts"));
    }

    #[tokio::test]
    async fn invalid_cross_field_avionics_uses_one_flash_correction_and_changes_only_avionics() {
        let primary_avionics = json!([installed_avionics(
            "Garmin",
            "G1000 NXi",
            &["Integrated Flight Deck", "Flight Display", "Autopilot"],
            1,
            "GARMIN G1000 NXI SVT Yes",
        )]);
        let primary = listing_extraction_with_avionics(primary_avionics);
        let corrected_avionics = json!([
            installed_avionics(
                "Garmin",
                "G1000 NXi",
                &["Integrated Flight Deck"],
                1,
                "GARMIN G1000 NXI",
            ),
            installed_avionics("Garmin", "GFC700", &["Autopilot"], 1, "GFC700"),
        ]);
        let (endpoint, request_count, requests) = extraction_sequence_endpoint(vec![
            primary.to_string(),
            json!({"avionics": corrected_avionics.clone()}).to_string(),
        ])
        .await;
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let source = SourceCorrelation {
            kind: "plugin_submission".to_string(),
            id: "submission-25".to_string(),
        };
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint)
            .with_usage_store(UsageStore::new(&db))
            .with_usage_scope(
                "listing-avionics-correction-test",
                None,
                Some(source.clone()),
            );
        let html = controller_avionics_html("GARMIN G1000 NXI", "GFC700");

        let (preview, payload) =
            extract_capture_to_current_checkpoint(CONTROLLER_ROLE_LISTING_URL, &html, &extractor)
                .await
                .unwrap();

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert_eq!(extractor.listing_visual_recovery_call_count(), 1);
        assert_eq!(payload["avionics"], corrected_avionics);
        assert_eq!(preview.parsed_listing.avionics.len(), 2);
        let mut actual_non_avionics = payload.clone();
        actual_non_avionics
            .as_object_mut()
            .unwrap()
            .remove("avionics");
        let mut expected_non_avionics =
            json!(crate::extract::parsed_listing_from_model_output(&primary));
        expected_non_avionics
            .as_object_mut()
            .unwrap()
            .remove("avionics");
        assert_eq!(actual_non_avionics, expected_non_avionics);

        let requests = requests.lock().unwrap();
        let correction_prompt = requests[1]["contents"][0]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(correction_prompt.contains("Previous transient avionics JSON"));
        assert!(correction_prompt.contains("GARMIN G1000 NXI SVT Yes"));
        assert!(correction_prompt.contains("structurally visible source unit"));
        assert!(correction_prompt.contains("Every additional type on that suite row"));
        assert!(!correction_prompt.contains("\"asking_price_usd\": 400000"));
        assert!(!correction_prompt.contains("\"serial_number\""));
        drop(requests);

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let usage: Vec<(String, String, String, Option<String>, Option<String>, Option<String>)> =
            sqlx::query_as(
                "SELECT task, purpose, model, correlation_id, source_kind, source_id FROM gemini_api_usage ORDER BY id",
            )
            .fetch_all(pool)
            .await
            .unwrap();
        assert_eq!(
            usage,
            vec![
                (
                    "listing_extraction".to_string(),
                    "listing_extraction".to_string(),
                    "gemini-3.5-flash-lite".to_string(),
                    Some("listing-avionics-correction-test".to_string()),
                    Some(source.kind.clone()),
                    Some(source.id.clone()),
                ),
                (
                    "listing_extraction".to_string(),
                    "listing_avionics_validation_correction".to_string(),
                    "gemini-3.5-flash".to_string(),
                    Some("listing-avionics-correction-test".to_string()),
                    Some(source.kind),
                    Some(source.id),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn correction_reuses_primary_model_when_listing_fallback_is_unset() {
        let primary = listing_extraction_with_avionics(json!([installed_avionics(
            "Garmin",
            "G1000 NXi",
            &["Integrated Flight Deck", "Autopilot"],
            1,
            "GARMIN G1000 NXI SVT Yes",
        )]));
        let corrected_avionics = json!([installed_avionics(
            "Garmin",
            "G1000 NXi",
            &["Integrated Flight Deck"],
            1,
            "GARMIN G1000 NXI",
        )]);
        let corrected = json!({"avionics": corrected_avionics});
        let (endpoint, request_count, _) =
            extraction_sequence_endpoint(vec![primary.to_string(), corrected.to_string()]).await;
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint)
            .with_test_listing_fallback_model(None)
            .with_usage_store(UsageStore::new(&db));
        let html = controller_avionics_html("GARMIN G1000 NXI", "GFC700");

        extract_capture_to_current_checkpoint(CONTROLLER_ROLE_LISTING_URL, &html, &extractor)
            .await
            .unwrap();

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let usage: Vec<(String, String)> =
            sqlx::query_as("SELECT purpose, model FROM gemini_api_usage ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(
            usage,
            vec![
                (
                    "listing_extraction".to_string(),
                    "gemini-3.5-flash-lite".to_string(),
                ),
                (
                    "listing_avionics_validation_correction".to_string(),
                    "gemini-3.5-flash-lite".to_string(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn invalid_fallback_fails_closed_without_a_checkpoint_or_third_request() {
        let invalid = installed_avionics(
            "Garmin",
            "G1000 NXi",
            &["Integrated Flight Deck", "Autopilot"],
            1,
            "GARMIN G1000 NXI SVT Yes",
        );
        let primary = listing_extraction_with_avionics(json!([invalid.clone()]));
        let (endpoint, request_count, _) = extraction_sequence_endpoint(vec![
            primary.to_string(),
            json!({"avionics": [invalid]}).to_string(),
        ])
        .await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let html = controller_avionics_html("GARMIN G1000 NXI", "GFC700");

        let error =
            extract_capture_to_current_checkpoint(CONTROLLER_ROLE_LISTING_URL, &html, &extractor)
                .await
                .unwrap_err();

        assert!(error
            .to_string()
            .contains("failed after its single correction request"));
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert_eq!(extractor.listing_visual_recovery_call_count(), 1);
    }

    #[tokio::test]
    async fn primary_json_repair_consumes_the_only_retry_and_never_opens_semantic_correction() {
        let invalid = installed_avionics(
            "Garmin",
            "G1000 NXi",
            &["Integrated Flight Deck", "Autopilot"],
            1,
            "GARMIN G1000 NXI SVT Yes",
        );
        let repaired_primary = listing_extraction_with_avionics(json!([invalid]));
        let (endpoint, request_count, requests) = extraction_sequence_endpoint(vec![
            "not valid json".to_string(),
            repaired_primary.to_string(),
        ])
        .await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let html = controller_avionics_html("GARMIN G1000 NXI", "GFC700");

        extract_capture_to_current_checkpoint(CONTROLLER_ROLE_LISTING_URL, &html, &extractor)
            .await
            .unwrap_err();

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        let requests = requests.lock().unwrap();
        let retry_prompt = requests[1]["contents"][0]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(retry_prompt.contains("previous response was not valid JSON"));
        assert!(!retry_prompt.contains("Previous transient avionics JSON"));
    }

    #[tokio::test]
    async fn corrected_response_still_receives_atomic_controller_quantity_repair() {
        let primary = listing_extraction_with_avionics(json!([installed_avionics(
            "Garmin",
            "G5",
            &["Flight Display", "Autopilot"],
            1,
            "Garmin G5 attitude SVT Yes",
        )]));
        let corrected = json!({
            "avionics": [installed_avionics(
                "Garmin",
                "G5",
                &["Flight Display"],
                1,
                "Garmin G5 attitude",
            )]
        });
        let (endpoint, request_count, _) =
            extraction_sequence_endpoint(vec![primary.to_string(), corrected.to_string()]).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let html = controller_avionics_html("Garmin G5 attitude, Garmin G5 HSI", "Garmin GFC500");

        let (preview, payload) =
            extract_capture_to_current_checkpoint(CONTROLLER_ROLE_LISTING_URL, &html, &extractor)
                .await
                .unwrap();

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert_eq!(payload["avionics"][0]["quantity"], 2);
        assert_eq!(
            payload["avionics"][0]["source_evidence_text"],
            "Garmin G5 attitude, Garmin G5 HSI"
        );
        assert_eq!(preview.parsed_listing.avionics[0].quantity, 2);
    }

    async fn signed_submission_request(
        db: &AppDb,
        user: &User,
        source_url: &str,
        html: &str,
    ) -> PluginSubmissionRequest {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();
        let install = query_as_one!(
            db,
            ListingIdRow,
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, ?) RETURNING id",
            user.id,
            BASE64_STANDARD.encode(key_pair.public_key().as_ref())
        )
        .unwrap();
        let hash = sha256_hex(html.as_bytes());
        let signature = BASE64_STANDARD.encode(
            key_pair
                .sign(
                    &rng,
                    signature_message(install.id, source_url, &hash).as_bytes(),
                )
                .unwrap()
                .as_ref(),
        );
        PluginSubmissionRequest {
            plugin_install_id: install.id,
            source_url: source_url.to_string(),
            rendered_html: html.to_string(),
            signature,
        }
    }

    fn replay_release(n_number: &str, serial: &str) -> Release {
        ReleaseFixtureBuilder::new(
            ReleaseMetadata::official("2026-08-19", "a".repeat(64)),
            "b".repeat(64),
            "c".repeat(64),
            MemberProvenance {
                member_name: "MASTER.txt".to_string(),
                sha256: "d".repeat(64),
            },
            MemberProvenance {
                member_name: "ACFTREF.txt".to_string(),
                sha256: "e".repeat(64),
            },
            MemberProvenance {
                member_name: "ENGINE.txt".to_string(),
                sha256: "f".repeat(64),
            },
        )
        .coverage(vec![TargetCoverage {
            n_number: n_number.to_string(),
            matched: true,
        }])
        .aircraft(vec![AircraftRecord {
            n_number: n_number.to_string(),
            manufacturer_serial_raw: Some(serial.to_string()),
            manufacturer_serial_key: crate::aircraft::faa::normalize_serial_key(serial),
            aircraft_code: "2072738".to_string(),
            engine_code: None,
            year_manufactured: Some(2020),
            source_record_sha256: "1".repeat(64),
        }])
        .aircraft_references(vec![AircraftReference {
            aircraft_code: "2072738".to_string(),
            manufacturer_name: Some("CESSNA".to_string()),
            model_name: Some("182T".to_string()),
            aircraft_type_code: None,
            engine_type_code: None,
            category_code: None,
            certification_indicator_code: None,
            engine_count: Some(1),
            seat_count: Some(4),
            weight_class_code: None,
            cruise_speed_mph: None,
            type_certificate_data_sheet: Some("3A13".to_string()),
            type_certificate_holder: Some("Textron Aviation Inc.".to_string()),
        }])
        .build()
    }

    async fn seed_replay_curated_aircraft(db: &AppDb, user_id: i64) {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let variant_id: i64 = sqlx::query_scalar("SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1")
            .fetch_one(pool).await.unwrap();
        let listing_id: i64 = sqlx::query_scalar("INSERT INTO aircraft_sale_listings (aircraft_model_variant_id, created_by_user_id, source_url, model_year, asking_price_usd, airframe_hours, registration_number, serial_number, ingestion_state) VALUES (?, ?, 'https://example.test/replay-catalog-stage', 2020, 1000, 0, 'N482TW', '18283006', 'incomplete') RETURNING id")
            .bind(variant_id).bind(user_id).fetch_one(pool).await.unwrap();
        let input_json = r#"{"manufacturer":"Cessna","model":"182","variant":"182T"}"#;
        sqlx::query("INSERT INTO aircraft_listing_identity_input_observations (aircraft_sale_listing_id, source_url, observed_make, observed_family, observed_designation, model_year, serial_number, registration_number, input_json, observation_sha256) VALUES (?, 'https://example.test/replay-catalog-stage', 'Cessna', '182', '182T', 2020, '18283006', 'N482TW', ?, ?)")
            .bind(listing_id).bind(input_json).bind(sha256_hex(format!("{listing_id}:{input_json}").as_bytes())).execute(pool).await.unwrap();
        let grounding = require_listing_faa_admission(db, listing_id).await.unwrap();
        crate::aircraft::identity::seed_test_curated_identity_assignment(
            db, listing_id, &grounding,
        )
        .await
        .unwrap();
        sqlx::query("DELETE FROM aircraft_sale_listings WHERE id = ?")
            .bind(listing_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn seed_grounded_test_avionics(db: &AppDb) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let manufacturer_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Garmin', 'garmin') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let avionics_type_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Transponder', 'transponder') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let model_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_models (avionics_manufacturer_id, name, normalized_name) VALUES (?, 'GTX 345R', 'gtx345r') RETURNING id",
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(model_id)
        .bind(avionics_type_id)
        .execute(pool)
        .await
        .unwrap();
        ensure_manufacturer_identity(
            db,
            manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: "https://manufacturer.example/aviation/".to_string(),
                source_title: "Manufacturer aviation catalog".to_string(),
                evidence_text: "The manufacturer catalog identifies Garmin aviation products."
                    .to_string(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE avionics_models
               SET catalog_status = 'approved',
                   manufacturer_identifier_kind = 'manufacturer_part_number',
                   manufacturer_identifier = 'TEST-GTX-345R',
                   normalized_manufacturer_identifier = 'testgtx345r',
                   identity_source_url = 'https://manufacturer.example/manuals/gtx-345r.pdf',
                   identity_source_title = 'Manufacturer GTX 345R manual',
                   identity_evidence_text = 'The manufacturer manual identifies the GTX 345R.',
                   identity_evidence_kind = 'authoritative_reference',
                   identity_confidence = 'very_high',
                   catalog_reviewed_at = CURRENT_TIMESTAMP
               WHERE id = ?"#,
        )
        .bind(model_id)
        .execute(pool)
        .await
        .unwrap();
        assert!(approved_avionics_identity_for_grounded_replay(db, model_id)
            .await
            .unwrap()
            .is_some());
        model_id
    }

    fn grounded_occurrence_capability_sha256_for_test(
        seed: &crate::avionics::catalog::GroundedAvionicsResolutionReceiptSeed,
        source_notes: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"aircost:listing-avionics-grounded-occurrence-capability:v2");
        for value in [
            "0".to_string(),
            "primary".to_string(),
            "installed".to_string(),
            source_notes.to_string(),
            seed.avionics_model_id().to_string(),
            seed.requested_quantity().to_string(),
            seed.request_sha256().to_string(),
            seed.capability_sha256().to_string(),
            seed.product_fingerprint().to_string(),
            seed.collision_closure_sha256().to_string(),
        ] {
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    #[test]
    fn verifies_fixed_p256_signature() {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();
        let hash = sha256_hex(b"<html>listing</html>");
        let message = signature_message(42, "https://example.test/listing", &hash);
        let signature = key_pair.sign(&rng, message.as_bytes()).unwrap();

        let public_key_base64 = BASE64_STANDARD.encode(key_pair.public_key().as_ref());
        let signature_base64 = BASE64_STANDARD.encode(signature.as_ref());

        verify_submission_signature(
            &public_key_base64,
            42,
            "https://example.test/listing",
            &hash,
            &signature_base64,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn revoked_install_accepts_only_captures_submitted_before_revocation() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let request = signed_submission_request(
            &db,
            &user,
            "https://example.test/historical-revoked-install",
            "<html>historical signed capture</html>",
        )
        .await;
        let outcome = submit_plugin_html(&db, &user, &request, None)
            .await
            .expect("a currently active install should submit the capture");
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE plugin_submissions SET submitted_at = '2026-08-18 12:00:00' WHERE id = ?",
        )
        .bind(outcome.submission.id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE plugin_installs SET revoked_at = '2026-08-19 12:00:00' WHERE id = ?")
            .bind(request.plugin_install_id)
            .execute(pool)
            .await
            .unwrap();
        let historical = load_checkpoint_capture(&db, user.id, outcome.submission.id)
            .await
            .unwrap();
        validate_stored_checkpoint_capture(&db, &historical)
            .await
            .expect("a capture signed and submitted before revocation remains replayable");

        sqlx::query(
            "UPDATE plugin_submissions SET submitted_at = '2026-08-19T01:00:00+02:00' WHERE id = ?",
        )
        .bind(outcome.submission.id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE plugin_installs SET revoked_at = '2026-08-18T23:30:00Z' WHERE id = ?")
            .bind(request.plugin_install_id)
            .execute(pool)
            .await
            .unwrap();
        let lexically_later_but_earlier_instant =
            load_checkpoint_capture(&db, user.id, outcome.submission.id)
                .await
                .unwrap();
        validate_stored_checkpoint_capture(&db, &lexically_later_but_earlier_instant)
            .await
            .expect("revocation comparison must use parsed instants rather than TEXT ordering");

        sqlx::query(
            "UPDATE plugin_submissions SET submitted_at = '2026-08-18T22:00:00-02:00' WHERE id = ?",
        )
        .bind(outcome.submission.id)
        .execute(pool)
        .await
        .unwrap();
        let lexically_earlier_but_later_instant =
            load_checkpoint_capture(&db, user.id, outcome.submission.id)
                .await
                .unwrap();
        let error = validate_stored_checkpoint_capture(&db, &lexically_earlier_but_later_instant)
            .await
            .expect_err("a post-revocation instant must fail despite its earlier TEXT spelling");
        assert!(matches!(error, PluginStoreError::Permission(_)));

        sqlx::query(
            "UPDATE plugin_submissions SET submitted_at = '2026-08-18 12:00:00' WHERE id = ?",
        )
        .bind(outcome.submission.id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE plugin_installs SET revoked_at = '2026-08-17 12:00:00' WHERE id = ?")
            .bind(request.plugin_install_id)
            .execute(pool)
            .await
            .unwrap();
        let impossible = load_checkpoint_capture(&db, user.id, outcome.submission.id)
            .await
            .unwrap();
        let error = validate_stored_checkpoint_capture(&db, &impossible)
            .await
            .expect_err("a capture timestamp after revocation is not historically valid");
        assert!(matches!(error, PluginStoreError::Permission(_)));
    }

    #[tokio::test]
    async fn failed_reprocess_update_preserves_prior_extraction_and_listing_link() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");

        let variant = query_as_one!(
            &db,
            ListingIdRow,
            r#"
            SELECT aircraft_model_variant_id AS id
            FROM aircraft_sale_listing_pending_compatibility_placeholder
            WHERE singleton_id = 1
            "#
        )
        .expect("pending aircraft placeholder should exist");
        let listing = query_as_one!(
            &db,
            ListingIdRow,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id,
              created_by_user_id,
              source_url,
              model_year,
              asking_price_usd,
              airframe_hours
            )
            VALUES (?, ?, 'https://example.test/listing', 2020, 100000, 500)
            RETURNING id
            "#,
            variant.id,
            user.id
        )
        .expect("listing should seed");
        let install = query_as_one!(
            &db,
            ListingIdRow,
            r#"
            INSERT INTO plugin_installs (user_id, public_key_base64)
            VALUES (?, 'test-key')
            RETURNING id
            "#,
            user.id
        )
        .expect("plugin install should seed");
        let prior_extraction = json!({
            "listing": {
                "registration_number": "N12345"
            }
        });
        let submission = query_as_one!(
            &db,
            ListingIdRow,
            r#"
            INSERT INTO plugin_submissions (
              user_id,
              plugin_install_id,
              source_url,
              rendered_html,
              rendered_html_sha256,
              signature_base64,
              extracted_listing_json,
              canonical_listing_id
            )
            VALUES (?, ?, 'https://example.test/listing', '<html></html>', 'hash', 'signature', ?, ?)
            RETURNING id
            "#,
            user.id,
            install.id,
            prior_extraction.to_string(),
            listing.id
        )
        .expect("plugin submission should seed");

        let updated = update_plugin_submission_result(
            &db,
            user.id,
            submission.id,
            None,
            Some("new extraction failed"),
            None,
        )
        .await
        .expect("failed reprocessing result should be recorded");

        assert_eq!(updated.extracted_listing_json, Some(prior_extraction));
        assert_eq!(
            updated.extraction_error.as_deref(),
            Some("new extraction failed")
        );
        assert_eq!(updated.canonical_listing_id, Some(listing.id));
    }

    #[tokio::test]
    async fn replay_faa_setup_gap_is_retryable_and_leaves_checkpoint_unbound() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();
        let public_key = BASE64_STANDARD.encode(key_pair.public_key().as_ref());
        let install = query_as_one!(
            &db,
            ListingIdRow,
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, ?) RETURNING id",
            user.id,
            public_key
        )
        .unwrap();
        let source_url = "https://example.test/replay-faa-rejection";
        let html = "<html><body><p>Garmin G5 Electronic Flight Instrument</p></body></html>";
        let hash = sha256_hex(html.as_bytes());
        let signature = BASE64_STANDARD.encode(
            key_pair
                .sign(
                    &rng,
                    signature_message(install.id, source_url, &hash).as_bytes(),
                )
                .unwrap()
                .as_ref(),
        );
        let extraction = json!({
            "manufacturer": "Cessna", "model": "182", "variant": "182T",
            "model_year": 2020, "asking_price_usd": 200000.0, "currency": "USD",
            "airframe_hours": 500.0, "engine_hours": null,
            "engine_time_basis": "unknown", "engine_time_evidence": null,
            "engine_time_confidence": null, "propeller_hours": null,
            "propeller_time_basis": "unknown", "propeller_time_evidence": null,
            "propeller_time_confidence": null, "installed_engine": null,
            "installed_propeller": null, "registration_number": "N182PF",
            "serial_number": "182TEST", "status": "active",
            "avionics": [{
                "manufacturer": "Garmin", "model": "G5", "types": ["Flight Display"],
                "quantity": 1, "configuration_action": "installed", "replaces": null,
                "source_evidence_text": "Garmin G5 Electronic Flight Instrument", "source_confidence": "high"
            }],
            "valuation_facts": []
        })
        .to_string();
        let submission = query_as_one!(
            &db,
            ListingIdRow,
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, submitted_at, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json
            ) VALUES (?, ?, ?, '2026-07-20 12:34:56', ?, ?, ?, ?) RETURNING id
            "#,
            user.id,
            install.id,
            source_url,
            html,
            &hash,
            signature,
            extraction.as_str()
        )
        .unwrap();
        let extractor =
            crate::extract::GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let error = materialize_plugin_submission_checkpoint(&db, &user, submission.id, &extractor)
            .await
            .expect_err("a missing FAA snapshot must remain retryable");
        assert!(matches!(
            error,
            PluginStoreError::AdmissionBlocked(
                PluginReplayAdmissionBlock::RegistrySnapshotUnavailable
            )
        ));
        let retained: (Option<i64>, Option<String>, Option<String>) = match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query_as(
                "SELECT canonical_listing_id, extracted_listing_json, extraction_error FROM plugin_submissions WHERE id = ?",
            )
            .bind(submission.id)
            .fetch_one(pool)
            .await
            .unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(retained, (None, Some(extraction), None));
    }

    #[tokio::test]
    async fn replay_corrects_only_the_working_serial_and_records_the_bound_decision() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();
        let install = query_as_one!(
            &db,
            ListingIdRow,
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, ?) RETURNING id",
            user.id,
            BASE64_STANDARD.encode(key_pair.public_key().as_ref())
        )
        .unwrap();
        let source_url = "https://example.test/replay-serial-correction";
        let html = "<html><body><h1>2020 Cessna 182T</h1><p>Registration N482TW; Serial Number 1823006</p></body></html>";
        let hash = sha256_hex(html.as_bytes());
        let signature = BASE64_STANDARD.encode(
            key_pair
                .sign(
                    &rng,
                    signature_message(install.id, source_url, &hash).as_bytes(),
                )
                .unwrap()
                .as_ref(),
        );
        let extraction = json!({
            "manufacturer":"Cessna","model":"182","variant":"182T","model_year":2020,
            "asking_price_usd":400000.0,"currency":"USD","airframe_hours":900.0,
            "engine_hours":null,"engine_time_basis":"unknown","engine_time_evidence":null,
            "engine_time_confidence":null,"propeller_hours":null,
            "propeller_time_basis":"unknown","propeller_time_evidence":null,
            "propeller_time_confidence":null,"installed_engine":null,"installed_propeller":null,
            "registration_number":"N482TW","serial_number":"1823006","status":"active",
            "avionics":[],"valuation_facts":[]
        })
        .to_string();
        let submission = query_as_one!(
            &db,
            ListingIdRow,
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, submitted_at, rendered_html,
                 rendered_html_sha256, signature_base64, extracted_listing_json
               ) VALUES (?, ?, ?, '2026-07-20 12:34:56', ?, ?, ?, ?) RETURNING id"#,
            user.id,
            install.id,
            source_url,
            html,
            hash,
            signature,
            extraction.as_str()
        )
        .unwrap();
        let extractor =
            crate::extract::GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let outcome =
            materialize_plugin_submission_checkpoint(&db, &user, submission.id, &extractor)
                .await
                .unwrap();
        let PluginListingReplayOutcome::Materialized { listing, .. } = outcome else {
            panic!("the current FAA serial must make replay materializable");
        };
        assert_eq!(listing.serial_number.as_deref(), Some("18283006"));
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let retained: (String, String, String, String) = sqlx::query_as(
            "SELECT submission.extracted_listing_json, listing.serial_number, decision.prior_serial_number, decision.corrected_serial_number FROM plugin_submissions submission JOIN aircraft_sale_listings listing ON listing.id = submission.canonical_listing_id JOIN aircraft_listing_identity_correction_decisions decision ON decision.plugin_submission_id = submission.id AND decision.correction_kind = 'faa_serial' WHERE submission.id = ?",
        )
        .bind(submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            retained,
            (
                extraction,
                "18283006".into(),
                "1823006".into(),
                "18283006".into()
            )
        );
    }

    #[tokio::test]
    async fn replay_post_bind_failure_recovers_the_same_binding_and_receipt() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();
        let install = query_as_one!(
            &db,
            ListingIdRow,
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, ?) RETURNING id",
            user.id,
            BASE64_STANDARD.encode(key_pair.public_key().as_ref())
        )
        .unwrap();
        let source_url = "https://example.test/replay-post-bind-failure";
        let html = "<html><body><p>N482TW serial 1823006</p></body></html>";
        let hash = sha256_hex(html.as_bytes());
        let signature = BASE64_STANDARD.encode(
            key_pair
                .sign(
                    &rng,
                    signature_message(install.id, source_url, &hash).as_bytes(),
                )
                .unwrap()
                .as_ref(),
        );
        let extraction = serial_correction_extraction();
        let submission = query_as_one!(
            &db,
            ListingIdRow,
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, rendered_html,
                 rendered_html_sha256, signature_base64, extracted_listing_json
               ) VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id"#,
            user.id,
            install.id,
            source_url,
            html,
            &hash,
            signature,
            extraction.to_string()
        )
        .unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"CREATE TRIGGER force_replay_child_projection_failure
               BEFORE INSERT ON aircraft_listing_identity_input_observations
               BEGIN SELECT RAISE(ABORT, 'forced replay child projection failure'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();
        let extractor =
            crate::extract::GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        materialize_plugin_submission_checkpoint(&db, &user, submission.id, &extractor)
            .await
            .expect_err("forced post-bind child failure must retain gated replay state");
        let retained: (i64, String, Option<String>, i64, i64) = sqlx::query_as(
            r#"SELECT submission.canonical_listing_id,
                      listing.ingestion_state,
                      listing.ingestion_error,
                      (SELECT count(*) FROM aircraft_sale_listings WHERE source_url = ?),
                      (SELECT count(*) FROM aircraft_listing_identity_correction_decisions)
               FROM plugin_submissions submission
               JOIN aircraft_sale_listings listing
                 ON listing.id = submission.canonical_listing_id
               WHERE submission.id = ?"#,
        )
        .bind(source_url)
        .bind(submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained.1, "quarantined");
        assert_eq!(
            retained.2.as_deref(),
            Some(crate::listings::SOURCE_IDENTITY_RECEIPT_PENDING)
        );
        assert_eq!((retained.3, retained.4), (1, 0));

        sqlx::query("DROP TRIGGER force_replay_child_projection_failure")
            .execute(pool)
            .await
            .unwrap();
        let recovered =
            materialize_plugin_submission_checkpoint(&db, &user, submission.id, &extractor)
                .await
                .expect("an exact replay retry must finish the durable post-bind state");
        let PluginListingReplayOutcome::Materialized { listing, .. } = recovered else {
            panic!("durable replay recovery must materialize the listing");
        };
        assert_eq!(listing.id, retained.0);
        let counts: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT count(*) FROM plugin_submissions WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_sale_listings WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_listing_identity_correction_decisions
                   WHERE plugin_submission_id = ? AND correction_kind = 'faa_serial')"#,
        )
        .bind(source_url)
        .bind(source_url)
        .bind(submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(counts, (1, 1, 1));
    }

    #[tokio::test]
    async fn ordinary_replay_post_bind_failure_resumes_before_writing_completion_receipt() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();
        let install = query_as_one!(
            &db,
            ListingIdRow,
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, ?) RETURNING id",
            user.id,
            BASE64_STANDARD.encode(key_pair.public_key().as_ref())
        )
        .unwrap();
        let source_url = "https://example.test/replay-ordinary-post-bind-failure";
        let html = "<html><body><p>N482TW serial 18283006</p></body></html>";
        let hash = sha256_hex(html.as_bytes());
        let signature = BASE64_STANDARD.encode(
            key_pair
                .sign(
                    &rng,
                    signature_message(install.id, source_url, &hash).as_bytes(),
                )
                .unwrap()
                .as_ref(),
        );
        let mut extraction = serial_correction_extraction();
        extraction["serial_number"] = json!("18283006");
        let submission = query_as_one!(
            &db,
            ListingIdRow,
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, submitted_at, rendered_html,
                 rendered_html_sha256, signature_base64, extracted_listing_json
               ) VALUES (?, ?, ?, '2026-07-20 12:34:56', ?, ?, ?, ?) RETURNING id"#,
            user.id,
            install.id,
            source_url,
            html,
            &hash,
            signature,
            extraction.to_string()
        )
        .unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"CREATE TRIGGER force_ordinary_replay_child_projection_failure
               BEFORE INSERT ON aircraft_listing_identity_input_observations
               BEGIN SELECT RAISE(ABORT, 'forced ordinary replay child projection failure'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();
        let extractor =
            crate::extract::GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        materialize_plugin_submission_checkpoint(&db, &user, submission.id, &extractor)
            .await
            .expect_err("a post-bind child failure must leave a resumable bound listing");
        let partial: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT submission.canonical_listing_id,
                      (SELECT count(*) FROM aircraft_sale_listings WHERE source_url = ?),
                      (SELECT count(*) FROM plugin_submission_materialization_receipts
                        WHERE plugin_submission_id = ?)
               FROM plugin_submissions submission WHERE submission.id = ?"#,
        )
        .bind(source_url)
        .bind(submission.id)
        .bind(submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!((partial.1, partial.2), (1, 0));

        sqlx::query("DROP TRIGGER force_ordinary_replay_child_projection_failure")
            .execute(pool)
            .await
            .unwrap();
        let recovered =
            materialize_plugin_submission_checkpoint(&db, &user, submission.id, &extractor)
                .await
                .expect("an exact retry must finish the bound child projections");
        let PluginListingReplayOutcome::Materialized { listing, .. } = recovered else {
            panic!("ordinary replay recovery must materialize the listing");
        };
        assert_eq!(listing.id, partial.0);
        let receipt: (i64, String, String) = sqlx::query_as(
            r#"SELECT aircraft_sale_listing_id, rendered_html_sha256,
                      extracted_listing_sha256
               FROM plugin_submission_materialization_receipts
               WHERE plugin_submission_id = ?"#,
        )
        .bind(submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(receipt.0, listing.id);
        assert_eq!(receipt.1, hash);
        assert_eq!(receipt.2, sha256_hex(extraction.to_string().as_bytes()));
    }

    #[tokio::test]
    async fn signed_submit_corrects_serial_and_records_one_bound_receipt() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let extraction = serial_correction_extraction();
        let endpoint = extraction_endpoint(extraction.clone()).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let source_url = "https://example.test/signed-submit-serial-correction";
        let html = "<html><body><h1>2020 Cessna 182T</h1><p>Registration N482TW; Serial Number 1823006</p></body></html>";
        let request = signed_submission_request(&db, &user, source_url, html).await;

        let outcome = submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect("normal signed submission should admit the FAA correction");
        let submission_id = outcome.submission.id;
        let listing = outcome
            .listing
            .expect("corrected listing should materialize");
        let listing_id = listing.id;
        assert_eq!(listing.serial_number.as_deref(), Some("18283006"));
        assert_eq!(
            outcome
                .submission
                .extracted_listing_json
                .as_ref()
                .and_then(|value| value.get("serial_number"))
                .and_then(Value::as_str),
            Some("1823006")
        );
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let receipt_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM aircraft_listing_identity_correction_decisions WHERE plugin_submission_id = ? AND correction_kind = 'faa_serial'",
        )
        .bind(outcome.submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(receipt_count, 1);

        let repeated = submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect("repeating one exact signed capture must be idempotent");
        assert_eq!(repeated.submission.id, submission_id);
        assert_eq!(
            repeated.listing.as_ref().map(|listing| listing.id),
            Some(listing_id)
        );

        let reprocessed = reprocess_plugin_submission(&db, &user, submission_id, Some(&extractor))
            .await
            .expect("reprocessing the bound exact checkpoint must reuse its receipt");
        assert_eq!(reprocessed.submission.id, submission_id);
        assert_eq!(
            reprocessed.listing.as_ref().map(|listing| listing.id),
            Some(listing_id)
        );
        let counts: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT count(*) FROM plugin_submissions WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_sale_listings WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_listing_identity_correction_decisions
                   WHERE plugin_submission_id = ? AND correction_kind = 'faa_serial')"#,
        )
        .bind(source_url)
        .bind(source_url)
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(counts, (1, 1, 1));
    }

    #[tokio::test]
    async fn ordinary_signed_submit_prepares_and_atomically_binds_exact_checkpoint() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let mut extraction = serial_correction_extraction();
        extraction["serial_number"] = json!("18283006");
        let endpoint = extraction_endpoint(extraction.clone()).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let source_url = "https://example.test/signed-submit-exact-checkpoint";
        let html = "<html><body><h1>2020 Cessna 182T</h1><p>Registration N482TW; Serial Number 18283006</p></body></html>";
        let request = signed_submission_request(&db, &user, source_url, html).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"CREATE TRIGGER require_prepared_signed_checkpoint
               BEFORE INSERT ON aircraft_sale_listings
               WHEN NEW.source_url = 'https://example.test/signed-submit-exact-checkpoint'
                AND NOT EXISTS (
                  SELECT 1 FROM plugin_submissions submission
                  WHERE submission.source_url = NEW.source_url
                    AND submission.canonical_listing_id IS NULL
                    AND submission.extracted_listing_json IS NOT NULL
                    AND submission.extraction_error IS NULL
                )
               BEGIN
                 SELECT RAISE(ABORT, 'signed checkpoint was not prepared before listing insert');
               END"#,
        )
        .execute(pool)
        .await
        .unwrap();

        let outcome = submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect("ordinary signed materialization must prepare its exact checkpoint first");
        let listing_id = outcome.listing.expect("listing should materialize").id;
        assert_eq!(outcome.submission.canonical_listing_id, Some(listing_id));
        assert_eq!(
            outcome.submission.extracted_listing_json,
            Some(extraction),
            "the atomically bound checkpoint must be the exact extracted payload"
        );
    }

    #[tokio::test]
    async fn ordinary_signed_submit_recovers_new_listing_without_reextracting() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let mut extraction = serial_correction_extraction();
        extraction["serial_number"] = json!("18283006");
        let (endpoint, request_count, _) =
            extraction_sequence_endpoint(vec![extraction.to_string()]).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let source_url = "https://example.test/signed-submit-new-recovery";
        let html = "<html><body><p>N482TW serial 18283006</p></body></html>";
        let request = signed_submission_request(&db, &user, source_url, html).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"CREATE TRIGGER force_new_signed_child_projection_failure
               BEFORE INSERT ON aircraft_listing_identity_input_observations
               BEGIN SELECT RAISE(ABORT, 'forced new signed child projection failure'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();

        submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect_err("the child projection failure must retain an incomplete exact binding");
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        let retained: (i64, i64, Option<String>, i64) = sqlx::query_as(
            r#"SELECT submission.id, submission.canonical_listing_id,
                      submission.extraction_error,
                      (SELECT count(*) FROM plugin_submission_materialization_receipts receipt
                        WHERE receipt.plugin_submission_id = submission.id)
               FROM plugin_submissions submission WHERE submission.source_url = ?"#,
        )
        .bind(source_url)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained.2, None);
        assert_eq!(retained.3, 0);

        sqlx::query("DROP TRIGGER force_new_signed_child_projection_failure")
            .execute(pool)
            .await
            .unwrap();
        let recovered = submit_plugin_html(&db, &user, &request, None)
            .await
            .expect("the retained signed checkpoint must recover without an extractor");
        assert_eq!(recovered.submission.id, retained.0);
        assert_eq!(
            recovered.listing.as_ref().map(|listing| listing.id),
            Some(retained.1)
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        let receipt_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM plugin_submission_materialization_receipts WHERE plugin_submission_id = ?",
        )
        .bind(retained.0)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(receipt_count, 1);
    }

    #[tokio::test]
    #[ignore = "requires the separately integrated grounded-capability v2 schema"]
    async fn bound_signed_retry_consumes_retained_capability_without_gemini() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let avionics_model_id = seed_grounded_test_avionics(&db).await;
        let source_url = "https://example.test/signed-retained-capability";
        let source_notes = "Garmin GTX 345R installed";
        let html =
            format!("<html><body><p>N482TW serial 18283006</p><p>{source_notes}</p></body></html>");
        let request = signed_submission_request(&db, &user, source_url, &html).await;
        let extraction = json!({
            "manufacturer":"Cessna","model":"182","variant":"182T","model_year":2020,
            "asking_price_usd":400000.0,"currency":"USD","airframe_hours":900.0,
            "engine_hours":null,"engine_time_basis":"unknown","engine_time_evidence":null,
            "engine_time_confidence":null,"propeller_hours":null,
            "propeller_time_basis":"unknown","propeller_time_evidence":null,
            "propeller_time_confidence":null,"installed_engine":null,"installed_propeller":null,
            "registration_number":"N482TW","serial_number":"18283006","status":"active",
            "avionics":[{
                "manufacturer":"Garmin","model":"GTX 345R","types":["Transponder"],
                "quantity":1,"configuration_action":"installed","replaces":null,
                "source_evidence_text":source_notes,"source_confidence":"high"
            }],
            "valuation_facts":[]
        });
        let extracted_listing_json = extraction.to_string();
        let extracted_listing_sha256 = sha256_hex(extracted_listing_json.as_bytes());
        let rendered_html_sha256 = sha256_hex(html.as_bytes());
        let listing_context = super::retained_listing_context(source_url, &html)
            .unwrap()
            .split_whitespace()
            .take(900)
            .collect::<Vec<_>>()
            .join(" ");
        let identity = approved_avionics_identity_for_grounded_replay(&db, avionics_model_id)
            .await
            .unwrap()
            .unwrap();
        let identity_request = AvionicsIdentityRequest {
            aircraft_manufacturer: "Cessna".to_string(),
            aircraft_model: "182".to_string(),
            aircraft_variant: "182T".to_string(),
            model_year: 2020,
            source_url: source_url.to_string(),
            listing_context,
            requires_listing_evidence: true,
            authoritative_direct_source_urls: Vec::new(),
            authoritative_identity_anchors: Vec::new(),
            manufacturer: "Garmin".to_string(),
            model: "GTX 345R".to_string(),
            avionics_types: vec!["Transponder".to_string()],
            quantity: 1,
        };
        let product_fingerprint = catalog_product_fingerprint_for_id(&db, avionics_model_id)
            .await
            .unwrap();
        let collision_closure = grounded_collision_closure_revision_sha256(&db, avionics_model_id)
            .await
            .unwrap();
        let seed = grounded_resolution_receipt_basis_for_replay(&identity_request, &identity)
            .bind_catalog_snapshot(product_fingerprint.clone(), collision_closure.clone());
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let variant_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO aircraft_sale_listings (
                 aircraft_model_variant_id, created_by_user_id, source_url, model_year,
                 asking_price_usd, currency, status, ingestion_state,
                 registration_number, serial_number, airframe_hours
               ) VALUES (?, ?, ?, 2020, 400000, 'USD', 'active', 'incomplete',
                         'N482TW', '18283006', 900)
               RETURNING id"#,
        )
        .bind(variant_id)
        .bind(user.id)
        .bind(source_url)
        .fetch_one(pool)
        .await
        .unwrap();
        let submission_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, rendered_html,
                 rendered_html_sha256, signature_base64, extracted_listing_json,
                 extraction_error, canonical_listing_id
               ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?)
               RETURNING id"#,
        )
        .bind(user.id)
        .bind(request.plugin_install_id)
        .bind(source_url)
        .bind(html.as_str())
        .bind(rendered_html_sha256.as_str())
        .bind(request.signature.as_str())
        .bind(extracted_listing_json.as_str())
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
                 product_fingerprint, collision_closure_sha256, policy_version
               ) VALUES (?, ?, 0, 'primary', ?, 1, 'installed', ?, ?, ?, ?, ?, ?, ?,
                         'listing_avionics_grounded_capability_v2')"#,
        )
        .bind(listing_id)
        .bind(submission_id)
        .bind(avionics_model_id)
        .bind(seed.request_sha256())
        .bind(grounded_occurrence_capability_sha256_for_test(
            &seed,
            source_notes,
        ))
        .bind(seed.bind(listing_id).resolution_sha256())
        .bind(rendered_html_sha256.as_str())
        .bind(extracted_listing_sha256.as_str())
        .bind(product_fingerprint.as_str())
        .bind(collision_closure.as_str())
        .execute(pool)
        .await
        .unwrap();

        let recovered = submit_plugin_html(&db, &user, &request, None)
            .await
            .expect("the exact retained capability must complete without Gemini");
        assert_eq!(recovered.submission.id, submission_id);
        assert_eq!(
            recovered.listing.as_ref().map(|listing| listing.id),
            Some(listing_id)
        );
        let state: (i64, i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT count(*) FROM aircraft_sale_listing_avionics
                   WHERE aircraft_sale_listing_id = ?),
                 (SELECT count(*) FROM aircraft_sale_listing_avionics_authorizations authorization
                    JOIN aircraft_sale_listing_avionics link
                      ON link.id = authorization.listing_link_id
                   WHERE link.aircraft_sale_listing_id = ?
                     AND authorization.authorization_kind = 'same_case_grounded'),
                 (SELECT count(*) FROM aircraft_sale_listing_avionics_grounded_capabilities
                   WHERE listing_id = ?),
                 (SELECT count(*) FROM gemini_api_usage)"#,
        )
        .bind(listing_id)
        .bind(listing_id)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(state, (1, 1, 0, 0));
    }

    #[tokio::test]
    async fn signed_submit_recovers_matching_verified_listing_without_reextracting() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let mut extraction = serial_correction_extraction();
        extraction["serial_number"] = json!("18283006");
        let (endpoint, request_count, _) =
            extraction_sequence_endpoint(vec![extraction.to_string(), extraction.to_string()])
                .await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"CREATE TRIGGER force_matching_baseline_receipt_failure
               BEFORE INSERT ON plugin_submission_materialization_receipts
               BEGIN SELECT RAISE(ABORT, 'forced baseline receipt failure'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();
        let original_source = "https://example.test/signed-matching-original";
        let original_request = signed_submission_request(
            &db,
            &user,
            original_source,
            "<html><body><p>N482TW serial 18283006 original</p></body></html>",
        )
        .await;
        submit_plugin_html(&db, &user, &original_request, Some(&extractor))
            .await
            .expect_err("the baseline completion receipt is intentionally interrupted");
        let listing_id: i64 = sqlx::query_scalar(
            "SELECT canonical_listing_id FROM plugin_submissions WHERE source_url = ?",
        )
        .bind(original_source)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("DROP TRIGGER force_matching_baseline_receipt_failure")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM plugin_submissions WHERE source_url = ?")
            .bind(original_source)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("UPDATE aircraft_sale_listings SET is_verified = TRUE WHERE id = ?")
            .bind(listing_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(&format!(
            r#"CREATE TRIGGER force_matching_signed_projection_failure
               BEFORE UPDATE OF added_at ON aircraft_sale_listings
               WHEN OLD.id = {listing_id}
               BEGIN SELECT RAISE(ABORT, 'forced matching signed projection failure'); END"#
        ))
        .execute(pool)
        .await
        .unwrap();

        let matching_source = "https://example.test/signed-matching-recovery";
        let matching_request = signed_submission_request(
            &db,
            &user,
            matching_source,
            "<html><body><p>N482TW serial 18283006 matching</p></body></html>",
        )
        .await;
        submit_plugin_html(&db, &user, &matching_request, Some(&extractor))
            .await
            .expect_err("the matching projection failure must retain its exact binding");
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        let retained: (i64, i64, Option<String>, i64, String) = sqlx::query_as(
            r#"SELECT submission.id, submission.canonical_listing_id,
                      submission.extraction_error,
                      (SELECT count(*) FROM plugin_submission_materialization_receipts receipt
                        WHERE receipt.plugin_submission_id = submission.id),
                      listing.source_url
               FROM plugin_submissions submission
               JOIN aircraft_sale_listings listing
                 ON listing.id = submission.canonical_listing_id
               WHERE submission.source_url = ?"#,
        )
        .bind(matching_source)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained.1, listing_id);
        assert_eq!(retained.2, None);
        assert_eq!(retained.3, 0);
        assert_eq!(retained.4, matching_source);

        sqlx::query("DROP TRIGGER force_matching_signed_projection_failure")
            .execute(pool)
            .await
            .unwrap();
        let recovered = submit_plugin_html(&db, &user, &matching_request, None)
            .await
            .expect("the matching exact checkpoint must recover without an extractor");
        assert_eq!(recovered.submission.id, retained.0);
        assert_eq!(
            recovered.listing.as_ref().map(|listing| listing.id),
            Some(listing_id)
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        let receipt_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM plugin_submission_materialization_receipts WHERE plugin_submission_id = ?",
        )
        .bind(retained.0)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(receipt_count, 1);
    }

    #[tokio::test]
    async fn signed_extraction_failure_remains_unbound_without_completion_receipt() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let request = signed_submission_request(
            &db,
            &user,
            "https://example.test/signed-extraction-failure",
            "<html><body>not extracted</body></html>",
        )
        .await;

        let outcome = submit_plugin_html(&db, &user, &request, None)
            .await
            .expect("an extraction failure remains a retained submission outcome");
        assert!(outcome.listing.is_none());
        assert!(outcome.submission.canonical_listing_id.is_none());
        assert!(outcome.submission.extraction_error.is_some());
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let receipt_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM plugin_submission_materialization_receipts WHERE plugin_submission_id = ?",
        )
        .bind(outcome.submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(receipt_count, 0);
    }

    #[tokio::test]
    async fn signed_submit_recovers_bound_listing_after_process_stops_before_receipt() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let extraction = serial_correction_extraction();
        let endpoint = extraction_endpoint(extraction).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let source_url = "https://example.test/signed-submit-receipt-conflict";
        let html = "<html><body><p>N482TW serial 1823006</p></body></html>";
        let request = signed_submission_request(&db, &user, source_url, html).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"CREATE TRIGGER force_source_receipt_failure
               BEFORE INSERT ON aircraft_listing_identity_correction_decisions
               BEGIN SELECT RAISE(ABORT, 'forced source receipt conflict'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect_err("forced receipt conflict must fail the signed submission");
        assert!(error.to_string().contains("could not be recorded"));
        let retained: (i64, i64, String, String, Option<String>, i64) = sqlx::query_as(
            r#"SELECT submission.id, submission.canonical_listing_id,
                      submission.extracted_listing_json, listing.ingestion_state,
                      listing.ingestion_error,
                      (SELECT count(*) FROM aircraft_listing_identity_correction_decisions)
               FROM plugin_submissions submission
               JOIN aircraft_sale_listings listing ON listing.id = submission.canonical_listing_id
               WHERE submission.source_url = ?"#,
        )
        .bind(source_url)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&retained.2).unwrap()["serial_number"],
            "1823006"
        );
        assert_eq!(retained.3, "quarantined");
        assert_eq!(
            retained.4.as_deref(),
            Some(crate::listings::SOURCE_IDENTITY_RECEIPT_PENDING)
        );
        assert_eq!(retained.5, 0);
        sqlx::query("DROP TRIGGER force_source_receipt_failure")
            .execute(pool)
            .await
            .unwrap();

        let recovered = submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect("an exact retry must finish the durable post-bind state");
        assert_eq!(recovered.submission.id, retained.0);
        assert_eq!(
            recovered.listing.as_ref().map(|listing| listing.id),
            Some(retained.1)
        );
        let counts: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT count(*) FROM plugin_submissions WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_sale_listings WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_listing_identity_correction_decisions
                   WHERE plugin_submission_id = ? AND correction_kind = 'faa_serial')"#,
        )
        .bind(source_url)
        .bind(source_url)
        .bind(retained.0)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(counts, (1, 1, 1));
    }

    #[tokio::test]
    async fn signed_submit_recovers_after_atomic_bind_before_child_projection() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let endpoint = extraction_endpoint(serial_correction_extraction()).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let source_url = "https://example.test/signed-submit-child-projection-failure";
        let html = "<html><body><p>N482TW serial 1823006</p></body></html>";
        let request = signed_submission_request(&db, &user, source_url, html).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"CREATE TRIGGER force_identity_child_projection_failure
               BEFORE INSERT ON aircraft_listing_identity_input_observations
               BEGIN SELECT RAISE(ABORT, 'forced child projection failure'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect_err("a child failure must stop after the durable atomic bind");
        assert!(error.to_string().contains("atomic listing binding"));
        let retained: (i64, i64, String, Option<String>, i64, i64) = sqlx::query_as(
            r#"SELECT submission.id, submission.canonical_listing_id,
                      listing.ingestion_state, listing.ingestion_error,
                      (SELECT count(*) FROM aircraft_listing_identity_input_observations
                        WHERE aircraft_sale_listing_id = listing.id),
                      (SELECT count(*) FROM aircraft_listing_identity_correction_decisions
                        WHERE plugin_submission_id = submission.id)
               FROM plugin_submissions submission
               JOIN aircraft_sale_listings listing
                 ON listing.id = submission.canonical_listing_id
               WHERE submission.source_url = ?"#,
        )
        .bind(source_url)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained.2, "quarantined");
        assert_eq!(
            retained.3.as_deref(),
            Some(crate::listings::SOURCE_IDENTITY_RECEIPT_PENDING)
        );
        assert_eq!((retained.4, retained.5), (0, 0));

        sqlx::query("DROP TRIGGER force_identity_child_projection_failure")
            .execute(pool)
            .await
            .unwrap();
        let recovered = submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect("the exact retry must deterministically finish child projections");
        assert_eq!(recovered.submission.id, retained.0);
        assert_eq!(
            recovered.listing.as_ref().map(|listing| listing.id),
            Some(retained.1)
        );
        let counts: (i64, i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT count(*) FROM plugin_submissions WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_sale_listings WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_listing_identity_input_observations
                   WHERE aircraft_sale_listing_id = ?),
                 (SELECT count(*) FROM aircraft_listing_identity_correction_decisions
                   WHERE plugin_submission_id = ? AND correction_kind = 'faa_serial')"#,
        )
        .bind(source_url)
        .bind(source_url)
        .bind(retained.1)
        .bind(retained.0)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(counts, (1, 1, 1, 1));
    }

    #[tokio::test]
    async fn failed_first_extraction_retry_atomically_replaces_checkpoint_and_corrects_serial() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let source_url = "https://example.test/failed-first-extraction-correction";
        let html = "<html><body><p>N482TW serial 1823006</p></body></html>";
        let request = signed_submission_request(&db, &user, source_url, html).await;

        let failed = submit_plugin_html(&db, &user, &request, None)
            .await
            .expect("the failed extraction checkpoint must be retained");
        assert!(failed.listing.is_none());
        assert!(failed.submission.extracted_listing_json.is_none());
        assert!(failed.submission.extraction_error.is_some());
        assert!(failed.submission.canonical_listing_id.is_none());

        let endpoint = extraction_endpoint(serial_correction_extraction()).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let recovered = submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect("the exact retry must replace the failed checkpoint while atomically binding");
        assert_eq!(recovered.submission.id, failed.submission.id);
        let listing = recovered
            .listing
            .expect("the retry must materialize a listing");
        assert_eq!(listing.serial_number.as_deref(), Some("18283006"));
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let counts: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT count(*) FROM plugin_submissions WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_sale_listings WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_listing_identity_correction_decisions
                   WHERE plugin_submission_id = ? AND correction_kind = 'faa_serial')"#,
        )
        .bind(source_url)
        .bind(source_url)
        .bind(failed.submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(counts, (1, 1, 1));
    }

    #[tokio::test]
    async fn failed_first_extraction_retry_recovers_after_atomic_bind_child_failure() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let source_url = "https://example.test/failed-first-extraction-child-failure";
        let html = "<html><body><p>N482TW serial 1823006</p></body></html>";
        let request = signed_submission_request(&db, &user, source_url, html).await;
        let failed = submit_plugin_html(&db, &user, &request, None)
            .await
            .expect("the failed extraction checkpoint must be retained");
        let endpoint = extraction_endpoint(serial_correction_extraction()).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"CREATE TRIGGER force_reprocessed_identity_child_failure
               BEFORE INSERT ON aircraft_listing_identity_input_observations
               BEGIN SELECT RAISE(ABORT, 'forced reprocessed child failure'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect_err("the child failure must stop after atomic checkpoint replacement and bind");
        assert!(error.to_string().contains("atomic listing binding"));
        let retained: (i64, String, Option<String>, String, Option<String>, i64) = sqlx::query_as(
            r#"SELECT submission.canonical_listing_id,
                          submission.extracted_listing_json,
                          submission.extraction_error,
                          listing.ingestion_state,
                          listing.ingestion_error,
                          (SELECT count(*) FROM aircraft_listing_identity_correction_decisions
                            WHERE plugin_submission_id = submission.id)
                   FROM plugin_submissions submission
                   JOIN aircraft_sale_listings listing
                     ON listing.id = submission.canonical_listing_id
                   WHERE submission.id = ?"#,
        )
        .bind(failed.submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&retained.1).unwrap()["serial_number"],
            "1823006"
        );
        assert_eq!(retained.2, None);
        assert_eq!(retained.3, "quarantined");
        assert_eq!(
            retained.4.as_deref(),
            Some(crate::listings::SOURCE_IDENTITY_RECEIPT_PENDING)
        );
        assert_eq!(retained.5, 0);

        sqlx::query("DROP TRIGGER force_reprocessed_identity_child_failure")
            .execute(pool)
            .await
            .unwrap();
        let recovered = submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect("the exact retry must converge from the replaced bound checkpoint");
        assert_eq!(recovered.submission.id, failed.submission.id);
        assert_eq!(
            recovered.listing.as_ref().map(|listing| listing.id),
            Some(retained.0)
        );
        let counts: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT count(*) FROM plugin_submissions WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_sale_listings WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_listing_identity_correction_decisions
                   WHERE plugin_submission_id = ? AND correction_kind = 'faa_serial')"#,
        )
        .bind(source_url)
        .bind(source_url)
        .bind(failed.submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(counts, (1, 1, 1));
    }

    #[tokio::test]
    async fn signed_submit_bind_failure_removes_unbound_corrected_listing() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let endpoint = extraction_endpoint(serial_correction_extraction()).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let source_url = "https://example.test/signed-submit-bind-conflict";
        let html = "<html><body><p>N482TW serial 1823006</p></body></html>";
        let request = signed_submission_request(&db, &user, source_url, html).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"CREATE TRIGGER force_signed_submission_bind_failure
               BEFORE INSERT ON plugin_submissions
               BEGIN SELECT RAISE(ABORT, 'forced signed bind conflict'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();

        submit_plugin_html(&db, &user, &request, Some(&extractor))
            .await
            .expect_err("forced bind conflict must fail the signed submission");
        let retained: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT count(*) FROM aircraft_sale_listings WHERE source_url = ?),
                 (SELECT count(*) FROM plugin_submissions WHERE source_url = ?),
                 (SELECT count(*) FROM aircraft_listing_identity_correction_decisions)"#,
        )
        .bind(source_url)
        .bind(source_url)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained, (0, 0, 0));
    }

    #[tokio::test]
    async fn corrected_reprocess_receipt_failure_restores_exact_prior_submission() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        store_release(&db, &replay_release("N482TW", "18283006"))
            .await
            .unwrap();
        seed_replay_curated_aircraft(&db, user.id).await;
        let extraction = serial_correction_extraction();
        let endpoint = extraction_endpoint(extraction).await;
        let extractor = crate::extract::GeminiListingExtractor::with_test_endpoint(endpoint);
        let source_url = "https://example.test/reprocess-receipt-conflict";
        let html = "<html><body><p>N482TW serial 1823006</p></body></html>";
        let request = signed_submission_request(&db, &user, source_url, html).await;
        let variant = query_as_one!(
            &db,
            ListingIdRow,
            "SELECT aircraft_model_variant_id AS id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1"
        )
        .unwrap();
        let old_listing = query_as_one!(
            &db,
            ListingIdRow,
            r#"INSERT INTO aircraft_sale_listings
               (aircraft_model_variant_id, created_by_user_id, source_url, model_year,
                asking_price_usd, airframe_hours, registration_number, serial_number,
                ingestion_state, ingestion_error)
               VALUES (?, ?, ?, 2019, 123456, 456, 'N482TW', 'LEGACY-WRONG',
                       'quarantined', 'retain exact prior listing') RETURNING id"#,
            variant.id,
            user.id,
            source_url
        )
        .unwrap();
        let prior_extraction = json!({"prior_checkpoint": true}).to_string();
        let submission = query_as_one!(
            &db,
            ListingIdRow,
            r#"INSERT INTO plugin_submissions
               (user_id, plugin_install_id, source_url, rendered_html,
                rendered_html_sha256, signature_base64, extracted_listing_json,
                extraction_error, canonical_listing_id)
               VALUES (?, ?, ?, ?, ?, ?, ?, 'prior extraction warning', ?) RETURNING id"#,
            user.id,
            request.plugin_install_id,
            source_url,
            html,
            sha256_hex(html.as_bytes()),
            request.signature.as_str(),
            prior_extraction.as_str(),
            old_listing.id
        )
        .unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"CREATE TRIGGER force_reprocess_receipt_failure
               BEFORE INSERT ON aircraft_listing_identity_correction_decisions
               BEGIN SELECT RAISE(ABORT, 'forced reprocess receipt conflict'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();

        reprocess_plugin_submission(&db, &user, submission.id, Some(&extractor))
            .await
            .expect_err("forced receipt conflict must fail reprocessing");
        let retained: (Option<String>, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT extracted_listing_json, extraction_error, canonical_listing_id FROM plugin_submissions WHERE id = ?",
        )
        .bind(submission.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            retained,
            (
                Some(prior_extraction),
                Some("prior extraction warning".into()),
                Some(old_listing.id)
            )
        );
        let listings: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, serial_number, ingestion_error FROM aircraft_sale_listings WHERE source_url = ? ORDER BY id",
        )
        .bind(source_url)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(
            listings,
            vec![(
                old_listing.id,
                "LEGACY-WRONG".into(),
                "retain exact prior listing".into()
            )]
        );
    }

    #[tokio::test]
    async fn replay_rejects_a_preexisting_same_source_before_mutating_it() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let variant = query_as_one!(
            &db,
            ListingIdRow,
            "SELECT aircraft_model_variant_id AS id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1"
        )
        .unwrap();
        let source = "https://example.test/already-present";
        let existing = query_as_one!(
            &db,
            ListingIdRow,
            r#"INSERT INTO aircraft_sale_listings
               (aircraft_model_variant_id, created_by_user_id, source_url, model_year,
                asking_price_usd, airframe_hours, ingestion_state, ingestion_error)
               VALUES (?, ?, ?, 2019, 123456, 654, 'quarantined', 'retain me') RETURNING id"#,
            variant.id,
            user.id,
            source
        )
        .unwrap();
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let keys = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let install = query_as_one!(
            &db,
            ListingIdRow,
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, ?) RETURNING id",
            user.id,
            BASE64_STANDARD.encode(keys.public_key().as_ref())
        )
        .unwrap();
        let html = "<html><body><p>listing capture</p></body></html>";
        let hash = sha256_hex(html.as_bytes());
        let signature = BASE64_STANDARD.encode(
            keys.sign(
                &rng,
                signature_message(install.id, source, &hash).as_bytes(),
            )
            .unwrap()
            .as_ref(),
        );
        let extraction = json!({
            "manufacturer":"Cessna","model":"182","variant":"182T","model_year":2020,
            "asking_price_usd":200000.0,"currency":"USD","airframe_hours":500.0,
            "engine_hours":null,"engine_time_basis":"unknown","engine_time_evidence":null,
            "engine_time_confidence":null,"propeller_hours":null,"propeller_time_basis":"unknown",
            "propeller_time_evidence":null,"propeller_time_confidence":null,"installed_engine":null,
            "installed_propeller":null,"registration_number":"N182PF","serial_number":"182TEST",
            "status":"active","avionics":[],"valuation_facts":[]
        })
        .to_string();
        let submission = query_as_one!(
            &db,
            ListingIdRow,
            r#"INSERT INTO plugin_submissions
               (user_id, plugin_install_id, source_url, rendered_html, rendered_html_sha256,
                signature_base64, extracted_listing_json)
               VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id"#,
            user.id,
            install.id,
            source,
            html,
            hash,
            signature,
            extraction
        )
        .unwrap();
        let extractor =
            crate::extract::GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let error = materialize_plugin_submission_checkpoint(&db, &user, submission.id, &extractor)
            .await
            .expect_err("same-source replay must fail before ordinary dedup mutation");
        assert!(
            error.to_string().contains("already claimed"),
            "unexpected rejection: {error}"
        );
        let retained: (f64, f64, String, Option<i64>) = match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query_as(
                r#"SELECT listing.asking_price_usd, listing.airframe_hours,
                          listing.ingestion_error, submission.canonical_listing_id
                   FROM aircraft_sale_listings listing
                   JOIN plugin_submissions submission ON submission.id = ?
                   WHERE listing.id = ?"#,
            )
            .bind(submission.id)
            .bind(existing.id)
            .fetch_one(pool)
            .await
            .unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(retained, (123456.0, 654.0, "retain me".into(), None));
    }

    #[test]
    fn checkpoint_round_trips_pinned_visual_recovery_without_a_second_model_call() {
        use crate::aircraft::curation::visual::{
            evaluate_visual_registration_consensus, VisibilityBasis, VisibleAircraftIdentifier,
            VisibleIdentifierKind, VisualEvidenceConfidence, VisualIdentifierImageEvidence,
            VisualIdentifierResolution, VisualIdentifierStatus, VisualPhotoAudit,
        };
        let candidate = VisibleAircraftIdentifier {
            kind: VisibleIdentifierKind::Registration,
            visible_text: "N182PF".into(),
            evidence_count: 1,
            evidence: vec![VisualIdentifierImageEvidence {
                image_id: "photo-1".into(),
                visible_text: "N182PF".into(),
                confidence: VisualEvidenceConfidence::VeryHigh,
                box_2d: [10, 20, 30, 40],
                visibility_basis: VisibilityBasis::ExteriorRegistrationMarking,
                location_description: "fuselage".into(),
            }],
        };
        let recovery = VisualIdentifierResolution {
            status: VisualIdentifierStatus::CandidatesVisible,
            candidates: vec![candidate.clone()],
            registration_consensus: evaluate_visual_registration_consensus(&[candidate]),
            refusal_reason: None,
            photos: vec![VisualPhotoAudit {
                image_id: "photo-1".into(),
                mime_type: "image/jpeg".into(),
                byte_count: 100,
                sha256: "a".repeat(64),
            }],
            interaction_id: Some("pinned-interaction".into()),
            model: "gemini-3-flash-preview".into(),
            prompt_version: "visual-aircraft-identifier-v1".into(),
            schema_version: "visual-aircraft-identifier-v1".into(),
            total_input_tokens: Some(10),
            total_output_tokens: Some(5),
        };
        let payload = json!({
            "manufacturer":"Cessna","model":"182","variant":"182T","model_year":2020,
            "asking_price_usd":200000.0,"currency":"USD","airframe_hours":500.0,
            "engine_hours":null,"engine_time_basis":"unknown","engine_time_evidence":null,
            "engine_time_confidence":null,"propeller_hours":null,"propeller_time_basis":"unknown",
            "propeller_time_evidence":null,"propeller_time_confidence":null,"installed_engine":null,
            "installed_propeller":null,"registration_number":"N182PF","serial_number":null,
            "status":"active","avionics":[],"valuation_facts":[],
            "visual_identity_recovery": recovery
        });
        let (listing, retained) = parse_current_checkpoint_payload(&payload.to_string()).unwrap();
        assert_eq!(listing.registration_number.as_deref(), Some("N182PF"));
        assert_eq!(retained, Some(recovery));
    }
}
