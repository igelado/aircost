use std::collections::HashSet;
use std::fmt;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use ring::digest;
use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{parse_listing_html, validate_source_url, GeminiListingExtractor};
use crate::html::clean::clean_listing_html;
use crate::listing::avionics::disposition::{
    record_automatic_occurrence_dispositions, AutomaticOccurrenceDisposition,
};
use crate::listing::avionics::extraction::validate_unbound_current_avionics_extraction;
use crate::listing::review::attach_pending_review_submission;
use crate::listings::{
    create_listing_with_progress_and_occurrence_dispositions, get_listing, ListingCreationMode,
    ListingStoreError,
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

#[derive(Debug)]
pub enum PluginStoreError {
    Validation(String),
    Permission(String),
    NotFound(String),
    Database(String),
}

impl fmt::Display for PluginStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginStoreError::Validation(message)
            | PluginStoreError::Permission(message)
            | PluginStoreError::NotFound(message)
            | PluginStoreError::Database(message) => write!(formatter, "{message}"),
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

type StoreResult<T> = Result<T, PluginStoreError>;
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
        stage: &'static str,
        reason: String,
    },
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
struct PluginSubmissionHtmlRow {
    id: i64,
    source_url: String,
    rendered_html: String,
    canonical_listing_id: Option<i64>,
}

#[derive(Debug, FromRow)]
struct PluginCheckpointRow {
    id: i64,
    user_id: i64,
    plugin_install_id: i64,
    public_key_base64: String,
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

    let mut preview = None;
    let mut listing = None;
    let mut extracted_listing_json = None;
    let mut extraction_error = None;
    let mut canonical_listing_id = None;
    let mut occurrence_dispositions: Vec<AutomaticOccurrenceDisposition> = Vec::new();

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
                match create_listing_with_progress_and_occurrence_dispositions(
                    db,
                    user.id,
                    &parsed_preview,
                    None,
                    Some(extractor),
                    progress,
                    ListingCreationMode::Ordinary,
                )
                .await
                {
                    Ok(created) => {
                        canonical_listing_id = Some(created.listing.id);
                        occurrence_dispositions = created.occurrence_dispositions;
                        listing = Some(created.listing);
                    }
                    Err(ListingStoreError::Ingestion {
                        listing_id,
                        message,
                    }) => {
                        canonical_listing_id = Some(listing_id);
                        extraction_error =
                            Some(format!("listing {listing_id} was quarantined: {message}"));
                    }
                    Err(error) => extraction_error = Some(error.to_string()),
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
    attach_submission_to_pending_review_if_needed(db, user, listing.as_ref(), &submission).await?;
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
    let stored = plugin_submission_html_for_user(db, user.id, submission_id).await?;
    let mut preview = None;
    let mut listing = None;
    let mut extracted_listing_json = None;
    let mut extraction_error = None;
    let mut canonical_listing_id = stored.canonical_listing_id;
    let mut occurrence_dispositions: Vec<AutomaticOccurrenceDisposition> = Vec::new();

    if let Some(extractor) = extractor {
        match extract_capture_to_current_checkpoint(
            &stored.source_url,
            &stored.rendered_html,
            extractor,
        )
        .await
        {
            Ok((parsed_preview, checkpoint_payload)) => {
                extracted_listing_json = Some(checkpoint_payload);
                match create_listing_with_progress_and_occurrence_dispositions(
                    db,
                    user.id,
                    &parsed_preview,
                    None,
                    Some(extractor),
                    None,
                    ListingCreationMode::Ordinary,
                )
                .await
                {
                    Ok(created) => {
                        canonical_listing_id = Some(created.listing.id);
                        occurrence_dispositions = created.occurrence_dispositions;
                        listing = Some(created.listing);
                    }
                    Err(ListingStoreError::Ingestion {
                        listing_id,
                        message,
                    }) => {
                        canonical_listing_id = Some(listing_id);
                        extraction_error =
                            Some(format!("listing {listing_id} was quarantined: {message}"));
                    }
                    Err(error) => extraction_error = Some(error.to_string()),
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

    let submission = update_plugin_submission_result(
        db,
        user.id,
        stored.id,
        extracted_listing_json.as_ref(),
        extraction_error.as_deref(),
        canonical_listing_id,
    )
    .await?;
    attach_submission_to_pending_review_if_needed(db, user, listing.as_ref(), &submission).await?;
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
    let Some(listing) = listing.filter(|listing| listing.ingestion_state == "pending_review")
    else {
        return Ok(());
    };
    attach_pending_review_submission(db, listing.id, submission.id, user.id)
        .await
        .map_err(|error| {
            PluginStoreError::Database(format!(
                "listing review was staged but its plugin submission could not be attached: {error}"
            ))
        })
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
    let mut payload = json!(preview.parsed_listing);
    if let (Some(object), Some(identity_recovery)) =
        (payload.as_object_mut(), preview.identity_recovery.as_ref())
    {
        object.insert(
            "visual_identity_recovery".to_string(),
            json!(identity_recovery),
        );
    }
    payload
}

fn parse_current_checkpoint_payload(
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
    let preview = parse_listing_html(source_url, rendered_html, extractor).await?;
    let payload = extracted_listing_payload(&preview);
    validate_unbound_current_avionics_extraction(&payload.to_string(), rendered_html)
        .map_err(PluginStoreError::Validation)?;
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
    validate_stored_checkpoint_capture(&stored)?;
    let (_preview, payload) =
        extract_capture_to_current_checkpoint(&stored.source_url, &stored.rendered_html, extractor)
            .await?;
    let payload_json = payload.to_string();
    let occurrences =
        validate_unbound_current_avionics_extraction(&payload_json, &stored.rendered_html)
            .map_err(PluginStoreError::Validation)?;
    let payload_sha256 = sha256_hex(payload_json.as_bytes());
    let sql = db.sql(
        r#"
        UPDATE plugin_submissions
        SET extracted_listing_json = ?,
            extraction_error = NULL,
            canonical_listing_id = NULL
        WHERE id = ?
          AND user_id = ?
          AND plugin_install_id = ?
          AND source_url = ?
          AND rendered_html_sha256 = ?
          AND signature_base64 = ?
          AND canonical_listing_id IS NULL
        "#,
    );
    let changed = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query(&sql)
            .bind(payload_json)
            .bind(stored.id)
            .bind(stored.user_id)
            .bind(stored.plugin_install_id)
            .bind(&stored.source_url)
            .bind(&stored.rendered_html_sha256)
            .bind(&stored.signature_base64)
            .execute(pool)
            .await?
            .rows_affected(),
        DatabaseBackend::Postgres(pool) => sqlx::query(&sql)
            .bind(payload_json)
            .bind(stored.id)
            .bind(stored.user_id)
            .bind(stored.plugin_install_id)
            .bind(&stored.source_url)
            .bind(&stored.rendered_html_sha256)
            .bind(&stored.signature_base64)
            .execute(pool)
            .await?
            .rows_affected(),
    };
    if changed != 1 {
        return Err(PluginStoreError::Database(
            "signed capture changed while its extraction checkpoint was being stored".to_string(),
        ));
    }
    Ok(PluginExtractionCheckpoint {
        submission_id,
        rendered_html_sha256: stored.rendered_html_sha256,
        extracted_listing_sha256: payload_sha256,
        avionics_occurrence_count: occurrences.len(),
    })
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
    validate_stored_checkpoint_capture(&stored)?;
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
    let occurrences =
        validate_unbound_current_avionics_extraction(extracted, &stored.rendered_html)
            .map_err(PluginStoreError::Validation)?;
    Ok(PluginExtractionCheckpoint {
        submission_id,
        rendered_html_sha256: stored.rendered_html_sha256,
        extracted_listing_sha256: sha256_hex(extracted.as_bytes()),
        avionics_occurrence_count: occurrences.len(),
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
    validate_stored_checkpoint_capture(&stored)?;
    if stored.extraction_error.is_some() {
        return Err(PluginStoreError::Validation(
            "capture has an extraction error instead of a replayable checkpoint".to_string(),
        ));
    }
    let current_checkpoint = match stored.extracted_listing_json.as_deref() {
        Some(extracted) => {
            let occurrences =
                validate_unbound_current_avionics_extraction(extracted, &stored.rendered_html)
                    .map_err(PluginStoreError::Validation)?;
            parse_current_checkpoint_payload(extracted)?;
            Some(PluginExtractionCheckpoint {
                submission_id,
                rendered_html_sha256: stored.rendered_html_sha256,
                extracted_listing_sha256: sha256_hex(extracted.as_bytes()),
                avionics_occurrence_count: occurrences.len(),
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

/// Materialize one exact extraction checkpoint through the ordinary listing
/// admission, aircraft, avionics, review, and finalization workflow. Listing
/// extraction is never called. The final capture binding and historical
/// observation timestamp update are one guarded transaction.
pub async fn materialize_plugin_submission_checkpoint(
    db: &AppDb,
    user: &User,
    submission_id: i64,
    extractor: &GeminiListingExtractor,
) -> StoreResult<PluginListingReplayOutcome> {
    let stored = load_checkpoint_capture(db, user.id, submission_id).await?;
    if stored.canonical_listing_id.is_some() {
        return Err(PluginStoreError::Validation(
            "replay capture is already bound to a canonical listing".to_string(),
        ));
    }
    validate_stored_checkpoint_capture(&stored)?;
    if stored.extraction_error.is_some() {
        return Err(PluginStoreError::Validation(
            "replay capture has an extraction error".to_string(),
        ));
    }
    let extracted_listing_json = stored.extracted_listing_json.as_deref().ok_or_else(|| {
        PluginStoreError::Validation(
            "replay capture has not reached the extraction checkpoint".to_string(),
        )
    })?;
    validate_unbound_current_avionics_extraction(extracted_listing_json, &stored.rendered_html)
        .map_err(PluginStoreError::Validation)?;
    let (parsed_listing, identity_recovery) =
        parse_current_checkpoint_payload(extracted_listing_json)?;
    let preview = ListingPreview {
        source_url: Some(stored.source_url.clone()),
        parsed_listing,
        warnings: Vec::new(),
        identity_recovery,
        context_text: Some(clean_listing_html(&stored.rendered_html)),
    };
    let existing_listing_ids =
        ensure_replay_source_is_unclaimed_and_snapshot(db, &stored.source_url).await?;
    let created = match create_listing_with_progress_and_occurrence_dispositions(
        db,
        user.id,
        &preview,
        None,
        Some(extractor),
        None,
        ListingCreationMode::CreateOnly,
    )
    .await
    {
        Ok(created) => created,
        Err(ListingStoreError::Validation(reason) | ListingStoreError::State(reason)) => {
            cleanup_new_replay_listings(db, &stored, &existing_listing_ids).await?;
            return Ok(PluginListingReplayOutcome::Rejected {
                submission_id,
                stage: "listing_admission",
                reason,
            });
        }
        Err(error) => {
            cleanup_new_replay_listings(db, &stored, &existing_listing_ids).await?;
            return Err(error.into());
        }
    };
    if existing_listing_ids.contains(&created.listing.id) {
        return Err(PluginStoreError::Database(
            "replay listing admission reused a preexisting listing instead of creating the exact capture observation"
                .to_string(),
        ));
    }
    let listing_id = created.listing.id;
    let materialized = async {
        bind_replay_capture_and_timestamp(db, &stored, listing_id).await?;
        let listing = get_listing(db, user.id, listing_id).await?;
        attach_submission_to_pending_review_if_needed(
            db,
            user,
            Some(&listing),
            &PluginSubmission {
                id: stored.id,
                user_id: stored.user_id,
                plugin_install_id: stored.plugin_install_id,
                source_url: stored.source_url.clone(),
                submitted_at: stored.submitted_at.clone(),
                rendered_html_sha256: stored.rendered_html_sha256.clone(),
                signature_base64: stored.signature_base64.clone(),
                extracted_listing_json: Some(
                    serde_json::from_str(extracted_listing_json).map_err(|error| {
                        PluginStoreError::Database(format!(
                            "stored extraction became invalid while binding replay: {error}"
                        ))
                    })?,
                ),
                extraction_error: None,
                canonical_listing_id: Some(listing.id),
            },
        )
        .await?;
        record_automatic_occurrence_dispositions(
            db,
            listing.id,
            stored.id,
            user.id,
            &created.occurrence_dispositions,
        )
        .await
        .map_err(PluginStoreError::Database)?;
        Ok::<SaleListing, PluginStoreError>(listing)
    }
    .await;
    let listing = match materialized {
        Ok(listing) => listing,
        Err(error) => {
            compensate_replay_listing(db, &stored, listing_id)
                .await
                .map_err(|cleanup| {
                    PluginStoreError::Database(format!(
                        "replay failed ({error}) and its new listing could not be compensated: {cleanup}"
                    ))
                })?;
            return Err(error);
        }
    };
    Ok(PluginListingReplayOutcome::Materialized {
        submission_id,
        listing,
    })
}

async fn bind_replay_capture_and_timestamp(
    db: &AppDb,
    stored: &PluginCheckpointRow,
    listing_id: i64,
) -> StoreResult<()> {
    let bind_submission = db.sql(
        r#"
        UPDATE plugin_submissions
        SET canonical_listing_id = ?, extraction_error = NULL
        WHERE id = ?
          AND user_id = ?
          AND plugin_install_id = ?
          AND source_url = ?
          AND rendered_html_sha256 = ?
          AND signature_base64 = ?
          AND extracted_listing_json = ?
          AND extraction_error IS NULL
          AND canonical_listing_id IS NULL
        "#,
    );
    let set_observed_at = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET added_at = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND created_by_user_id = ?
          AND source_url = ?
        "#,
    );
    macro_rules! bind_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let bound = sqlx::query(&bind_submission)
                .bind(listing_id)
                .bind(stored.id)
                .bind(stored.user_id)
                .bind(stored.plugin_install_id)
                .bind(&stored.source_url)
                .bind(&stored.rendered_html_sha256)
                .bind(&stored.signature_base64)
                .bind(stored.extracted_listing_json.as_deref())
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if bound != 1 {
                return Err(PluginStoreError::Database(
                    "replay capture changed while its listing was being bound".to_string(),
                ));
            }
            let timestamped = sqlx::query(&set_observed_at)
                .bind(&stored.submitted_at)
                .bind(listing_id)
                .bind(stored.user_id)
                .bind(&stored.source_url)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if timestamped != 1 {
                return Err(PluginStoreError::Database(
                    "replayed listing changed before its observation timestamp was restored"
                        .to_string(),
                ));
            }
            transaction.commit().await?;
            Ok::<(), PluginStoreError>(())
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => bind_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => bind_in_transaction!(pool),
    }
}

async fn ensure_replay_source_is_unclaimed_and_snapshot(
    db: &AppDb,
    source_url: &str,
) -> StoreResult<HashSet<i64>> {
    let rows = query_as_all!(
        db,
        ListingIdRow,
        "SELECT id FROM aircraft_sale_listings ORDER BY id"
    )?;
    let source_owner = query_as_optional!(
        db,
        ListingIdRow,
        r#"
        SELECT id
        FROM aircraft_sale_listings
        WHERE source_url = ?
        ORDER BY id
        LIMIT 1
        "#,
        source_url
    )?;
    if let Some(row) = source_owner {
        return Err(PluginStoreError::Validation(format!(
            "replay target already contains listing {} for this exact capture source",
            row.id
        )));
    }
    Ok(rows.into_iter().map(|row| row.id).collect())
}

async fn cleanup_new_replay_listings(
    db: &AppDb,
    stored: &PluginCheckpointRow,
    existing_listing_ids: &HashSet<i64>,
) -> StoreResult<()> {
    let candidates = query_as_all!(
        db,
        ListingIdRow,
        r#"
        SELECT id
        FROM aircraft_sale_listings
        WHERE created_by_user_id = ? AND source_url = ?
        ORDER BY id
        "#,
        stored.user_id,
        stored.source_url.as_str()
    )?;
    for candidate in candidates {
        if !existing_listing_ids.contains(&candidate.id) {
            compensate_replay_listing(db, stored, candidate.id).await?;
        }
    }
    Ok(())
}

async fn compensate_replay_listing(
    db: &AppDb,
    stored: &PluginCheckpointRow,
    listing_id: i64,
) -> StoreResult<()> {
    let detach = db.sql(
        r#"
        UPDATE plugin_submissions
        SET canonical_listing_id = NULL
        WHERE id = ? AND canonical_listing_id = ?
        "#,
    );
    let delete = db.sql(
        r#"
        DELETE FROM aircraft_sale_listings
        WHERE id = ? AND created_by_user_id = ? AND source_url = ?
        "#,
    );
    macro_rules! compensate_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&detach)
                .bind(stored.id)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?;
            let deleted = sqlx::query(&delete)
                .bind(listing_id)
                .bind(stored.user_id)
                .bind(&stored.source_url)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if deleted != 1 {
                return Err(PluginStoreError::Database(
                    "new replay listing changed before compensation".to_string(),
                ));
            }
            transaction.commit().await?;
            Ok::<(), PluginStoreError>(())
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => compensate_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => compensate_in_transaction!(pool),
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

fn validate_stored_checkpoint_capture(stored: &PluginCheckpointRow) -> StoreResult<()> {
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
    )
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

async fn plugin_submission_html_for_user(
    db: &AppDb,
    user_id: i64,
    submission_id: i64,
) -> StoreResult<PluginSubmissionHtmlRow> {
    let row = query_as_optional!(
        db,
        PluginSubmissionHtmlRow,
        r#"
        SELECT
          id,
          source_url,
          rendered_html,
          canonical_listing_id
        FROM plugin_submissions
        WHERE id = ? AND user_id = ?
        "#,
        submission_id,
        user_id
    )?;
    row.ok_or_else(|| PluginStoreError::NotFound("plugin submission not found".to_string()))
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
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine as _;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use serde_json::json;

    use super::{
        bind_replay_capture_and_timestamp, compensate_replay_listing,
        materialize_plugin_submission_checkpoint, parse_current_checkpoint_payload, sha256_hex,
        signature_message, update_plugin_submission_result, verify_submission_signature,
        ListingIdRow, PluginCheckpointRow, PluginListingReplayOutcome,
    };
    use crate::db::{AppDb, DatabaseBackend};

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
    async fn replay_faa_rejection_is_typed_and_leaves_checkpoint_unbound() {
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
                .expect("FAA rejection should be an inspectable replay outcome");
        assert!(matches!(
            outcome,
            PluginListingReplayOutcome::Rejected { stage: "listing_admission", ref reason, .. }
                if reason.contains("FAA aircraft admission rejected")
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
    async fn replay_compensation_unbinds_capture_and_deletes_only_new_listing() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let variant = query_as_one!(
            &db,
            ListingIdRow,
            "SELECT aircraft_model_variant_id AS id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1"
        )
        .unwrap();
        let source = "https://example.test/compensation";
        let listing = query_as_one!(
            &db,
            ListingIdRow,
            r#"INSERT INTO aircraft_sale_listings
               (aircraft_model_variant_id, created_by_user_id, source_url, model_year,
                asking_price_usd, airframe_hours)
               VALUES (?, ?, ?, 2020, 100000, 500) RETURNING id"#,
            variant.id,
            user.id,
            source
        )
        .unwrap();
        let install = query_as_one!(
            &db,
            ListingIdRow,
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'key') RETURNING id",
            user.id
        )
        .unwrap();
        let submission = query_as_one!(
            &db,
            ListingIdRow,
            r#"INSERT INTO plugin_submissions
               (user_id, plugin_install_id, source_url, submitted_at, rendered_html,
                rendered_html_sha256, signature_base64, extracted_listing_json)
               VALUES (?, ?, ?, '2026-07-20 12:34:56', '<html>x</html>', 'hash', 'sig', '{}')
               RETURNING id"#,
            user.id,
            install.id,
            source
        )
        .unwrap();
        let stored = PluginCheckpointRow {
            id: submission.id,
            user_id: user.id,
            plugin_install_id: install.id,
            public_key_base64: "key".into(),
            source_url: source.into(),
            submitted_at: "2026-07-20 12:34:56".into(),
            rendered_html: "<html>x</html>".into(),
            rendered_html_sha256: "hash".into(),
            signature_base64: "sig".into(),
            extracted_listing_json: Some("{}".into()),
            extraction_error: None,
            canonical_listing_id: None,
        };
        bind_replay_capture_and_timestamp(&db, &stored, listing.id)
            .await
            .unwrap();
        compensate_replay_listing(&db, &stored, listing.id)
            .await
            .unwrap();
        let state: (i64, Option<i64>, Option<String>) = match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query_as(
                "SELECT (SELECT COUNT(*) FROM aircraft_sale_listings WHERE id = ?), canonical_listing_id, extracted_listing_json FROM plugin_submissions WHERE id = ?",
            )
            .bind(listing.id)
            .bind(submission.id)
            .fetch_one(pool)
            .await
            .unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(state, (0, None, Some("{}".into())));
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
        assert!(error.to_string().contains("already contains listing"));
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
