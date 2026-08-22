//! Evidence-backed correction of FAA-blocked existing listings.
//!
//! Repairs are explicit reviewer actions. Provider-free preflight discovers
//! only immutable retained-source choices; a visual action makes one bounded
//! call for one selected photo. Every applied correction is guarded by a
//! fingerprint of the current listing and capture, admitted against the
//! latest FAA projection, and retained as immutable observation, evidence,
//! and decision history.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, Postgres, Sqlite};
use url::Url;

use crate::aircraft::curation::visual::{
    evaluate_visual_registration_consensus, resolve_visible_aircraft_identifiers_with_accounting,
    ListingPhotoInput, VisibleIdentifierKind, VisualConsensusStatus, VisualIdentifierConfig,
    VisualIdentifierResolution,
};
use crate::aircraft::faa::{
    admit_aircraft_source_identity, block_reason_code, normalize_n_number,
    require_aircraft_admission, require_listing_faa_admission, AircraftAdmissionError,
    AircraftGrounding, BlockReason, FaaSerialCorrection,
};
use crate::aircraft::observations::operator_source_identity_evidence_matches;
use crate::aircraft::verification::{
    preflight_listing_aircraft_verification, AircraftVerificationOutcome,
    AircraftVerificationPendingReason,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::gemini::config::{GeminiRuntimeConfig, GeminiTask};
use crate::gemini::interactions::{GeminiInteractionsClient, InteractionAccountingContext};
use crate::html::listing::download::download_identity_image;
use crate::html::listing::media::{discover, MediaDiscoveryError};
use crate::listings::{
    PinnedSourceVisualCorrectionArtifact, SourceVisualRegistrationCorrection,
    SOURCE_IDENTITY_RECEIPT_PENDING,
};
use crate::models::ParsedListing;

const MAX_PUBLISHER_EVIDENCE_CHARACTERS: usize = 2_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AircraftRepairAction {
    VisualIdentifier,
    FaaSerial,
    PublisherHierarchy,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AircraftRepairVisualAsset {
    pub asset_id: String,
    pub media_url: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AircraftRepairPreflight {
    Available {
        listing_id: i64,
        expected_state_sha256: String,
        reason_code: String,
        actions: Vec<AircraftRepairAction>,
        visual_assets: Vec<AircraftRepairVisualAsset>,
    },
    Unavailable {
        listing_id: i64,
        reason_code: &'static str,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualAircraftRepairRequest {
    pub expected_state_sha256: String,
    pub asset_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherAircraftRepairRequest {
    pub expected_state_sha256: String,
    pub exact_evidence_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaaSerialAircraftRepairRequest {
    pub expected_state_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AircraftRepairOutcome {
    Applied {
        listing_id: i64,
        correction_decision_id: i64,
        registration_number: Option<String>,
        serial_number: Option<String>,
        faa_snapshot_id: Option<i64>,
    },
    ImportRequired {
        listing_id: i64,
        candidate_n_number: String,
        current_snapshot_id: Option<i64>,
        reason_code: &'static str,
    },
    Inconclusive {
        listing_id: i64,
        reason_code: &'static str,
    },
    Blocked {
        listing_id: i64,
        reason_code: &'static str,
    },
}

#[derive(Debug)]
pub enum AircraftRepairError {
    NotFound(i64),
    Permission,
    Stale,
    Validation(&'static str),
    Service(String),
    Database(String),
}

impl fmt::Display for AircraftRepairError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(listing_id) => write!(formatter, "listing {listing_id} was not found"),
            Self::Permission => write!(formatter, "reviewer does not own this listing"),
            Self::Stale => write!(formatter, "listing identity changed after repair preflight"),
            Self::Validation(message) => write!(formatter, "aircraft repair rejected: {message}"),
            Self::Service(message) => {
                write!(formatter, "aircraft recovery service failed: {message}")
            }
            Self::Database(message) => {
                write!(formatter, "aircraft repair database failed: {message}")
            }
        }
    }
}

impl std::error::Error for AircraftRepairError {}

#[derive(Clone, Debug, FromRow)]
struct RepairState {
    listing_id: i64,
    owner_user_id: i64,
    listing_source_url: Option<String>,
    manufacturer: String,
    model: String,
    variant: String,
    model_year: i64,
    registration_number: Option<String>,
    serial_number: Option<String>,
    submission_id: Option<i64>,
    submission_source_url: Option<String>,
    rendered_html: Option<String>,
    rendered_html_sha256: Option<String>,
    extracted_listing_json: Option<String>,
}

#[derive(Debug, FromRow)]
struct BoundSubmissionState {
    user_id: i64,
    source_url: String,
    rendered_html: String,
    rendered_html_sha256: String,
    extracted_listing_json: Option<String>,
    extraction_error: Option<String>,
    canonical_listing_id: Option<i64>,
}

impl RepairState {
    fn source_url(&self) -> Option<&str> {
        self.submission_source_url
            .as_deref()
            .or(self.listing_source_url.as_deref())
    }

    fn fingerprint(&self) -> String {
        let extracted_listing_sha256 = self
            .extracted_listing_json
            .as_deref()
            .map(|value| format!("{:x}", Sha256::digest(value.as_bytes())));
        let value = serde_json::json!({
            "schema": "aircraft_listing_repair_state_v1",
            "listing_id": self.listing_id,
            "owner_user_id": self.owner_user_id,
            "source_url": self.source_url(),
            "manufacturer": self.manufacturer,
            "model": self.model,
            "variant": self.variant,
            "model_year": self.model_year,
            "registration_number": self.registration_number,
            "serial_number": self.serial_number,
            "submission_id": self.submission_id,
            "rendered_html_sha256": self.rendered_html_sha256,
            "extracted_listing_sha256": extracted_listing_sha256,
        });
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&value).expect("repair state serializes"))
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepairBlocker {
    Visual(&'static str),
    FaaSerial,
    ManualSerial,
    PublisherHierarchy,
}

pub async fn preflight_aircraft_repair(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
) -> Result<AircraftRepairPreflight, AircraftRepairError> {
    let state = load_state(db, listing_id).await?;
    require_owner(&state, owner_user_id)?;
    let Some(blocker) = repair_blocker(db, listing_id).await? else {
        return Ok(AircraftRepairPreflight::Unavailable {
            listing_id,
            reason_code: "aircraft_repair_not_required",
        });
    };
    let (reason_code, actions, visual_assets) = match blocker {
        RepairBlocker::PublisherHierarchy => (
            "source_evidence_missing".to_string(),
            vec![AircraftRepairAction::PublisherHierarchy],
            Vec::new(),
        ),
        RepairBlocker::FaaSerial => (
            "serial_conflict".to_string(),
            vec![AircraftRepairAction::FaaSerial],
            Vec::new(),
        ),
        RepairBlocker::ManualSerial => {
            return Ok(AircraftRepairPreflight::Unavailable {
                listing_id,
                reason_code: "serial_conflict_requires_explicit_evidence",
            });
        }
        RepairBlocker::Visual(reason_code) => {
            let assets = discover_assets(&state);
            (
                reason_code.to_string(),
                vec![AircraftRepairAction::VisualIdentifier],
                assets,
            )
        }
    };
    Ok(AircraftRepairPreflight::Available {
        listing_id,
        expected_state_sha256: state.fingerprint(),
        reason_code,
        actions,
        visual_assets,
    })
}

/// Replace a conflicting listing serial with the exact manufacturer serial in
/// the already-matched current FAA record. This path is provider-free.
pub async fn correct_serial_from_current_faa(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    request: &FaaSerialAircraftRepairRequest,
) -> Result<AircraftRepairOutcome, AircraftRepairError> {
    let state = guarded_state(
        db,
        owner_user_id,
        listing_id,
        &request.expected_state_sha256,
    )
    .await?;
    if repair_blocker(db, listing_id).await? != Some(RepairBlocker::FaaSerial) {
        return Ok(AircraftRepairOutcome::Blocked {
            listing_id,
            reason_code: "faa_serial_repair_not_allowed",
        });
    }
    let admission = strict_retained_source_serial_admission(db, &state).await?;
    let correction =
        admission
            .serial_correction
            .as_ref()
            .ok_or(AircraftRepairError::Validation(
                "retained source has no narrow FAA serial correction",
            ))?;
    let corrected_serial = correction.corrected_serial_number.as_str();
    let exact = admission.grounding;
    let decision_id = persist_correction(
        db,
        &state,
        owner_user_id,
        &request.expected_state_sha256,
        PersistCorrection::FaaSerial {
            registration: &exact.n_number,
            serial: corrected_serial,
            grounding: &exact,
        },
    )
    .await?;
    Ok(AircraftRepairOutcome::Applied {
        listing_id,
        correction_decision_id: decision_id,
        registration_number: Some(exact.n_number),
        serial_number: Some(corrected_serial.to_string()),
        faa_snapshot_id: Some(exact.snapshot.id),
    })
}

/// Record a serial correction used while materializing one previously
/// unbound extraction checkpoint. The stored extraction remains unchanged;
/// this function verifies its literal observed serial against the retained
/// capture, re-admits the same correction from current FAA data, and writes
/// immutable correction history after the capture has been bound.
pub async fn record_bound_source_serial_correction(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    submission_id: i64,
    correction: &FaaSerialCorrection,
) -> Result<AircraftRepairOutcome, AircraftRepairError> {
    if submission_id <= 0 {
        return Err(AircraftRepairError::Validation(
            "source correction submission is invalid",
        ));
    }
    let state = load_state(db, listing_id).await?;
    require_owner(&state, owner_user_id)?;
    if state.submission_id != Some(submission_id) {
        return Err(AircraftRepairError::Stale);
    }
    let extracted_json =
        load_bound_extraction(db, owner_user_id, listing_id, submission_id).await?;
    let parsed: ParsedListing = serde_json::from_str(&extracted_json).map_err(|_| {
        AircraftRepairError::Validation("bound source extraction is not a current listing object")
    })?;
    let observed_registration =
        parsed
            .registration_number
            .as_deref()
            .ok_or(AircraftRepairError::Validation(
                "bound source extraction has no registration",
            ))?;
    let observed_serial = parsed
        .serial_number
        .as_deref()
        .map(str::trim)
        .filter(|serial| !serial.is_empty())
        .ok_or(AircraftRepairError::Validation(
            "bound source extraction has no serial",
        ))?;
    if observed_serial != correction.observed_serial_number.trim()
        || state.serial_number.as_deref().map(str::trim)
            != Some(correction.corrected_serial_number.trim())
        || normalize_n_number(observed_registration)
            != state
                .registration_number
                .as_deref()
                .and_then(normalize_n_number)
    {
        return Err(AircraftRepairError::Stale);
    }
    let rendered_html = state
        .rendered_html
        .as_deref()
        .ok_or(AircraftRepairError::Stale)?;
    if !crate::html::clean::listing_body_contains_exact_structurally_visible_text_span(
        rendered_html,
        observed_serial,
    ) {
        return Err(AircraftRepairError::Validation(
            "observed source serial is not an exact visible retained-source span",
        ));
    }
    let admission = admit_aircraft_source_identity(
        db,
        Some(observed_registration),
        Some(observed_serial),
        Some(rendered_html),
    )
    .await
    .map_err(admission_error)?;
    if admission.serial_correction.as_ref() != Some(correction)
        || admission.effective_serial_number() != Some(correction.corrected_serial_number.as_str())
    {
        return Err(AircraftRepairError::Stale);
    }
    let expected_state_sha256 = state.fingerprint();
    let decision_id = persist_correction(
        db,
        &state,
        owner_user_id,
        &expected_state_sha256,
        PersistCorrection::SourceFaaSerial {
            observed_serial,
            parsed: &parsed,
            grounding: &admission.grounding,
        },
    )
    .await?;
    Ok(AircraftRepairOutcome::Applied {
        listing_id,
        correction_decision_id: decision_id,
        registration_number: Some(admission.grounding.n_number),
        serial_number: Some(correction.corrected_serial_number.clone()),
        faa_snapshot_id: Some(admission.grounding.snapshot.id),
    })
}

/// Record a one-photo registration correction used while materializing an
/// exact signed source checkpoint. The checkpoint retains the publisher's raw
/// registration; the listing and this immutable decision retain the separately
/// FAA-admitted visual correction.
pub(crate) async fn record_bound_source_visual_correction(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    submission_id: i64,
    correction: &SourceVisualRegistrationCorrection,
) -> Result<AircraftRepairOutcome, AircraftRepairError> {
    if submission_id <= 0 {
        return Err(AircraftRepairError::Validation(
            "source correction submission is invalid",
        ));
    }
    let state = load_state(db, listing_id).await?;
    require_owner(&state, owner_user_id)?;
    if state.submission_id != Some(submission_id) {
        return Err(AircraftRepairError::Stale);
    }
    let extracted_json =
        load_bound_extraction(db, owner_user_id, listing_id, submission_id).await?;
    let artifact = load_pinned_source_visual_artifact(db, submission_id).await?;
    let parsed: ParsedListing = serde_json::from_str(&extracted_json).map_err(|_| {
        AircraftRepairError::Validation("bound source extraction is not a current listing object")
    })?;
    let observed_registration = parsed
        .registration_number
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(AircraftRepairError::Validation(
            "bound source extraction has no registration",
        ))?;
    if observed_registration != correction.observed_registration_number.trim()
        || normalize_n_number(observed_registration)
            == normalize_n_number(&correction.corrected_registration_number)
        || state.registration_number.as_deref()
            != Some(correction.corrected_registration_number.as_str())
        || state.serial_number.as_deref() != correction.corrected_serial_number.as_deref()
    {
        return Err(AircraftRepairError::Stale);
    }
    let rendered_html = state
        .rendered_html
        .as_deref()
        .ok_or(AircraftRepairError::Stale)?;
    if !crate::html::clean::listing_body_contains_exact_structurally_visible_text_span(
        rendered_html,
        observed_registration,
    ) {
        return Err(AircraftRepairError::Validation(
            "observed source registration is not an exact visible retained-source span",
        ));
    }
    validate_source_visual_correction(&state, correction)?;
    validate_pinned_source_visual_artifact(&state, correction, &artifact)?;
    let exact_grounding = require_aircraft_admission(
        db,
        Some(&correction.corrected_registration_number),
        correction.corrected_serial_number.as_deref(),
    )
    .await
    .map_err(admission_error)?;
    if exact_grounding != correction.grounding {
        return Err(AircraftRepairError::Stale);
    }
    let expected_state_sha256 = state.fingerprint();
    let decision_id = persist_correction(
        db,
        &state,
        owner_user_id,
        &expected_state_sha256,
        PersistCorrection::SourceVisual {
            parsed: &parsed,
            correction,
            artifact: &artifact,
        },
    )
    .await?;
    Ok(AircraftRepairOutcome::Applied {
        listing_id,
        correction_decision_id: decision_id,
        registration_number: Some(correction.corrected_registration_number.clone()),
        serial_number: correction.corrected_serial_number.clone(),
        faa_snapshot_id: Some(exact_grounding.snapshot.id),
    })
}

async fn load_pinned_source_visual_artifact(
    db: &AppDb,
    submission_id: i64,
) -> Result<PinnedSourceVisualCorrectionArtifact, AircraftRepairError> {
    let sql = db.sql(
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
    );
    let artifact = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, PinnedSourceVisualCorrectionArtifact>(&sql)
                .bind(submission_id)
                .fetch_optional(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, PinnedSourceVisualCorrectionArtifact>(&sql)
                .bind(submission_id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(db_error)?;
    artifact.ok_or(AircraftRepairError::Stale)
}

fn validate_pinned_source_visual_artifact(
    state: &RepairState,
    correction: &SourceVisualRegistrationCorrection,
    artifact: &PinnedSourceVisualCorrectionArtifact,
) -> Result<(), AircraftRepairError> {
    if artifact.plugin_submission_id != state.submission_id.ok_or(AircraftRepairError::Stale)?
        || Some(artifact.rendered_html_sha256.as_str()) != state.rendered_html_sha256.as_deref()
        || artifact.observed_registration_number != correction.observed_registration_number
        || artifact.corrected_registration_number != correction.corrected_registration_number
        || artifact.corrected_serial_number != correction.corrected_serial_number
        || artifact.faa_registry_snapshot_id != correction.grounding.snapshot.id
        || artifact.faa_snapshot_archive_sha256 != correction.grounding.snapshot.archive_sha256
        || artifact.faa_source_record_sha256 != correction.grounding.source_record_sha256
        || artifact.primary_photo_url != correction.media_url
        || format!(
            "{:x}",
            Sha256::digest(artifact.visual_resolution_json.as_bytes())
        ) != artifact.visual_resolution_sha256
    {
        return Err(AircraftRepairError::Stale);
    }
    let pinned: VisualIdentifierResolution = serde_json::from_str(&artifact.visual_resolution_json)
        .map_err(|_| AircraftRepairError::Stale)?;
    if pinned.status != correction.resolution.status
        || pinned.candidates != correction.resolution.candidates
        || pinned.registration_consensus != correction.resolution.registration_consensus
        || pinned.refusal_reason != correction.resolution.refusal_reason
        || pinned.photos != correction.resolution.photos
        || pinned.photos.len() != 1
        || pinned.photos[0].image_id != format!("asset-{}", artifact.primary_photo_asset_id)
        || pinned.photos[0].sha256 != artifact.primary_photo_sha256
    {
        return Err(AircraftRepairError::Stale);
    }
    Ok(())
}

fn validate_source_visual_correction(
    state: &RepairState,
    correction: &SourceVisualRegistrationCorrection,
) -> Result<(), AircraftRepairError> {
    let resolution = &correction.resolution;
    if resolution.photos.len() != 1
        || resolution.registration_consensus.status != VisualConsensusStatus::AutoAccept
        || evaluate_visual_registration_consensus(&resolution.candidates)
            != resolution.registration_consensus
        || resolution
            .registration_consensus
            .normalized_n_number
            .as_deref()
            .and_then(normalize_n_number)
            .as_deref()
            != Some(correction.corrected_registration_number.as_str())
    {
        return Err(AircraftRepairError::Validation(
            "source visual correction is not one complete registration observation",
        ));
    }
    let photo_id = resolution.photos[0].image_id.as_str();
    if resolution.candidates.iter().any(|candidate| {
        candidate.evidence_count != candidate.evidence.len()
            || candidate.evidence.is_empty()
            || candidate
                .evidence
                .iter()
                .any(|evidence| evidence.image_id != photo_id)
    }) {
        return Err(AircraftRepairError::Validation(
            "source visual correction has invalid photo evidence bindings",
        ));
    }
    let source_url = state.source_url().ok_or(AircraftRepairError::Stale)?;
    let rendered_html = state
        .rendered_html
        .as_deref()
        .ok_or(AircraftRepairError::Stale)?;
    let discovery = discover(source_url, rendered_html).map_err(|_| {
        AircraftRepairError::Validation("retained source has no supported visual assets")
    })?;
    let primary = discovery
        .aircraft_photos
        .first()
        .ok_or(AircraftRepairError::Validation(
            "retained source has no primary aircraft photo",
        ))?;
    if primary.media_url != correction.media_url
        || photo_id != format!("asset-{}", primary.asset_id)
    {
        return Err(AircraftRepairError::Stale);
    }
    Ok(())
}

pub async fn recover_aircraft_from_visual_asset(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    request: &VisualAircraftRepairRequest,
    client: &GeminiInteractionsClient,
    runtime: &GeminiRuntimeConfig,
) -> Result<AircraftRepairOutcome, AircraftRepairError> {
    let state = guarded_state(
        db,
        owner_user_id,
        listing_id,
        &request.expected_state_sha256,
    )
    .await?;
    if !matches!(
        repair_blocker(db, listing_id).await?,
        Some(RepairBlocker::Visual(_))
    ) {
        return Ok(AircraftRepairOutcome::Blocked {
            listing_id,
            reason_code: "visual_recovery_not_allowed",
        });
    }
    let source_url = state.source_url().ok_or(AircraftRepairError::Validation(
        "retained source is unavailable",
    ))?;
    let rendered_html = state
        .rendered_html
        .as_deref()
        .ok_or(AircraftRepairError::Validation(
            "retained source is unavailable",
        ))?;
    let discovery = discover(source_url, rendered_html).map_err(|_| {
        AircraftRepairError::Validation("retained source has no supported visual assets")
    })?;
    let image = download_identity_image(&discovery, &request.asset_id)
        .await
        .map_err(|error| AircraftRepairError::Service(format!("{error:#}")))?;
    let image_id = format!("asset-{}", image.reference.asset_id);
    let photo = ListingPhotoInput::new(image_id, image.mime_type, image.bytes);
    let config = VisualIdentifierConfig::from_runtime_config(runtime)
        .map_err(|error| AircraftRepairError::Service(error.to_string()))?;
    let accounting = InteractionAccountingContext::new(
        GeminiTask::AircraftVisualIdentity,
        "existing_listing_identity_repair",
    )
    .with_listing_id(listing_id)
    .with_correlation_id(format!(
        "aircraft-repair:{listing_id}:{}",
        state.fingerprint()
    ))
    .with_source(
        "plugin_submission",
        state.submission_id.unwrap_or_default().to_string(),
    );
    let resolution =
        resolve_visible_aircraft_identifiers_with_accounting(client, &[photo], &config, accounting)
            .await
            .map_err(|error| AircraftRepairError::Service(format!("{error:#}")))?;
    apply_visual_resolution(
        db,
        owner_user_id,
        listing_id,
        &request.expected_state_sha256,
        &resolution,
        &image.reference.media_url,
    )
    .await
}

pub async fn corroborate_publisher_hierarchy(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    request: &PublisherAircraftRepairRequest,
) -> Result<AircraftRepairOutcome, AircraftRepairError> {
    let state = guarded_state(
        db,
        owner_user_id,
        listing_id,
        &request.expected_state_sha256,
    )
    .await?;
    if repair_blocker(db, listing_id).await? != Some(RepairBlocker::PublisherHierarchy) {
        return Ok(AircraftRepairOutcome::Blocked {
            listing_id,
            reason_code: "publisher_hierarchy_recovery_not_allowed",
        });
    }
    let evidence = request.exact_evidence_text.trim();
    if evidence.is_empty() || evidence.chars().count() > MAX_PUBLISHER_EVIDENCE_CHARACTERS {
        return Err(AircraftRepairError::Validation(
            "publisher evidence must contain 1 to 2000 characters",
        ));
    }
    let rendered_html = state
        .rendered_html
        .as_deref()
        .ok_or(AircraftRepairError::Validation(
            "retained source is unavailable",
        ))?;
    if !operator_source_identity_evidence_matches(
        rendered_html,
        evidence,
        &state.manufacturer,
        &state.model,
        &state.variant,
    ) {
        return Ok(AircraftRepairOutcome::Blocked {
            listing_id,
            reason_code: "publisher_evidence_not_exact",
        });
    }
    let decision_id = persist_correction(
        db,
        &state,
        owner_user_id,
        &request.expected_state_sha256,
        PersistCorrection::Publisher { evidence },
    )
    .await?;
    Ok(AircraftRepairOutcome::Applied {
        listing_id,
        correction_decision_id: decision_id,
        registration_number: state.registration_number,
        serial_number: state.serial_number,
        faa_snapshot_id: None,
    })
}

async fn apply_visual_resolution(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    expected_state_sha256: &str,
    resolution: &VisualIdentifierResolution,
    media_url: &str,
) -> Result<AircraftRepairOutcome, AircraftRepairError> {
    let state = guarded_state(db, owner_user_id, listing_id, expected_state_sha256).await?;
    if resolution.registration_consensus.status != VisualConsensusStatus::AutoAccept {
        return Ok(AircraftRepairOutcome::Inconclusive {
            listing_id,
            reason_code: "visual_identifier_inconclusive",
        });
    }
    let Some(n_number) = resolution
        .registration_consensus
        .normalized_n_number
        .as_deref()
    else {
        return Ok(AircraftRepairOutcome::Inconclusive {
            listing_id,
            reason_code: "visual_registration_missing",
        });
    };
    let grounding = match require_aircraft_admission(db, Some(n_number), None).await {
        Ok(grounding) => grounding,
        Err(AircraftAdmissionError::Rejected {
            reason: BlockReason::RegistrationNotCovered,
            snapshot_id,
            ..
        }) => {
            return Ok(AircraftRepairOutcome::ImportRequired {
                listing_id,
                candidate_n_number: n_number.to_string(),
                current_snapshot_id: snapshot_id,
                reason_code: "faa_target_import_required",
            });
        }
        Err(AircraftAdmissionError::Rejected { reason, .. }) => {
            return Ok(AircraftRepairOutcome::Blocked {
                listing_id,
                reason_code: safe_visual_faa_block(&reason),
            });
        }
        Err(error) => return Err(admission_error(error)),
    };
    let visible_serials = resolution
        .candidates
        .iter()
        .filter(|candidate| candidate.kind == VisibleIdentifierKind::ManufacturerSerial)
        .map(|candidate| candidate.visible_text.trim())
        .filter(|serial| !serial.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    if visible_serials.len() > 1 {
        return Ok(AircraftRepairOutcome::Inconclusive {
            listing_id,
            reason_code: "visual_serial_conflict",
        });
    }
    let corrected_serial = visible_serials
        .iter()
        .next()
        .map(|serial| (*serial).to_string())
        .or_else(|| grounding.manufacturer_serial_raw.clone())
        .or_else(|| state.serial_number.clone());
    let exact_grounding =
        match require_aircraft_admission(db, Some(n_number), corrected_serial.as_deref()).await {
            Ok(grounding) => grounding,
            Err(AircraftAdmissionError::Rejected { reason, .. }) => {
                return Ok(AircraftRepairOutcome::Blocked {
                    listing_id,
                    reason_code: safe_visual_faa_block(&reason),
                });
            }
            Err(error) => return Err(admission_error(error)),
        };
    let decision_id = persist_correction(
        db,
        &state,
        owner_user_id,
        expected_state_sha256,
        PersistCorrection::Visual {
            registration: n_number,
            serial: corrected_serial.as_deref(),
            grounding: &exact_grounding,
            resolution,
            media_url,
        },
    )
    .await?;
    Ok(AircraftRepairOutcome::Applied {
        listing_id,
        correction_decision_id: decision_id,
        registration_number: Some(n_number.to_string()),
        serial_number: corrected_serial,
        faa_snapshot_id: Some(exact_grounding.snapshot.id),
    })
}

fn safe_visual_faa_block(reason: &BlockReason) -> &'static str {
    match reason {
        BlockReason::RegistrationNotCovered => "faa_target_import_required",
        BlockReason::RegistrationNotFound => "recovered_registration_not_found",
        BlockReason::SerialConflict => "recovered_serial_conflict",
        BlockReason::AmbiguousRegistration => "recovered_registration_ambiguous",
        _ => "recovered_identity_not_admitted",
    }
}

async fn repair_blocker(
    db: &AppDb,
    listing_id: i64,
) -> Result<Option<RepairBlocker>, AircraftRepairError> {
    match require_listing_faa_admission(db, listing_id).await {
        Err(AircraftAdmissionError::Rejected { reason, .. }) => match reason {
            BlockReason::SerialConflict => match strict_retained_source_serial_admission(
                db,
                &load_state(db, listing_id).await?,
            )
            .await
            {
                Ok(admission) if admission.serial_correction.is_some() => {
                    Ok(Some(RepairBlocker::FaaSerial))
                }
                Ok(_) => Ok(Some(RepairBlocker::ManualSerial)),
                Err(AircraftRepairError::Validation(_)) => Ok(Some(RepairBlocker::ManualSerial)),
                Err(error) => Err(error),
            },
            BlockReason::RegistrationNotFound => {
                Ok(Some(RepairBlocker::Visual("registration_not_found")))
            }
            BlockReason::MissingRegistration => {
                Ok(Some(RepairBlocker::Visual("missing_registration")))
            }
            BlockReason::InvalidNNumber => Ok(Some(RepairBlocker::Visual("invalid_n_number"))),
            _ => Ok(None),
        },
        Err(error) => Err(admission_error(error)),
        Ok(_) => match preflight_listing_aircraft_verification(db, listing_id)
            .await
            .map_err(|error| AircraftRepairError::Database(error.to_string()))?
        {
            AircraftVerificationOutcome::Pending {
                reason: AircraftVerificationPendingReason::SourceEvidenceMissing,
                ..
            } => Ok(Some(RepairBlocker::PublisherHierarchy)),
            _ => Ok(None),
        },
    }
}

async fn strict_retained_source_serial_admission(
    db: &AppDb,
    state: &RepairState,
) -> Result<crate::aircraft::faa::SourceAircraftAdmission, AircraftRepairError> {
    let registration =
        state
            .registration_number
            .as_deref()
            .ok_or(AircraftRepairError::Validation(
                "listing registration is unavailable",
            ))?;
    let serial = state
        .serial_number
        .as_deref()
        .ok_or(AircraftRepairError::Validation(
            "listing serial is unavailable",
        ))?;
    let retained_source = state
        .rendered_html
        .as_deref()
        .ok_or(AircraftRepairError::Validation(
            "retained source is unavailable",
        ))?;
    admit_aircraft_source_identity(db, Some(registration), Some(serial), Some(retained_source))
        .await
        .map_err(|error| match error {
            AircraftAdmissionError::Rejected {
                reason: BlockReason::SerialConflict,
                ..
            } => AircraftRepairError::Validation(
                "serial conflict requires explicit retained-source evidence and adjudication",
            ),
            other => admission_error(other),
        })
}

fn discover_assets(state: &RepairState) -> Vec<AircraftRepairVisualAsset> {
    let (Some(source_url), Some(rendered_html)) =
        (state.source_url(), state.rendered_html.as_deref())
    else {
        return Vec::new();
    };
    let discovery = match discover(source_url, rendered_html) {
        Ok(discovery) => discovery,
        Err(
            MediaDiscoveryError::UnsupportedSourceHost
            | MediaDiscoveryError::UnsupportedSourcePath
            | MediaDiscoveryError::InvalidSourceUrl
            | MediaDiscoveryError::UnsafeSourceUrl
            | MediaDiscoveryError::SourceUrlTooLong { .. }
            | MediaDiscoveryError::RetainedHtmlTooLarge { .. },
        ) => return Vec::new(),
    };
    discovery
        .aircraft_photos
        .into_iter()
        .chain(discovery.logbook_attachments)
        .filter(|asset| asset.is_visual_image())
        .map(|asset| AircraftRepairVisualAsset {
            asset_id: asset.asset_id,
            media_url: asset.media_url,
            label: asset.label.or(asset.section_label),
        })
        .collect()
}

async fn guarded_state(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    expected_state_sha256: &str,
) -> Result<RepairState, AircraftRepairError> {
    if !valid_sha256(expected_state_sha256) {
        return Err(AircraftRepairError::Validation(
            "repair state hash is invalid",
        ));
    }
    let state = load_state(db, listing_id).await?;
    require_owner(&state, owner_user_id)?;
    if state.fingerprint() != expected_state_sha256 {
        return Err(AircraftRepairError::Stale);
    }
    Ok(state)
}

fn require_owner(state: &RepairState, owner_user_id: i64) -> Result<(), AircraftRepairError> {
    if state.owner_user_id != owner_user_id {
        return Err(AircraftRepairError::Permission);
    }
    Ok(())
}

async fn load_state(db: &AppDb, listing_id: i64) -> Result<RepairState, AircraftRepairError> {
    let sql = db.sql(
        r#"
        SELECT listing.id AS listing_id,
               listing.created_by_user_id AS owner_user_id,
               listing.source_url AS listing_source_url,
               manufacturer.name AS manufacturer,
               model.name AS model,
               variant.name AS variant,
               listing.model_year,
               listing.registration_number,
               listing.serial_number,
               submission.id AS submission_id,
               submission.source_url AS submission_source_url,
               submission.rendered_html,
               submission.rendered_html_sha256,
               submission.extracted_listing_json
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer ON manufacturer.id = model.aircraft_manufacturer_id
        LEFT JOIN plugin_submissions submission ON submission.id = (
          SELECT candidate.id FROM plugin_submissions candidate
          WHERE candidate.canonical_listing_id = listing.id
          ORDER BY candidate.submitted_at DESC, candidate.id DESC LIMIT 1
        )
        WHERE listing.id = ?
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, RepairState>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, RepairState>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|error| AircraftRepairError::Database(error.to_string()))?;
    row.ok_or(AircraftRepairError::NotFound(listing_id))
}

async fn load_bound_extraction(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    submission_id: i64,
) -> Result<String, AircraftRepairError> {
    let sql = db.sql(
        "SELECT extracted_listing_json FROM plugin_submissions WHERE id = ? AND user_id = ? AND canonical_listing_id = ? AND extraction_error IS NULL",
    );
    let extracted = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, Option<String>>(&sql)
                .bind(submission_id)
                .bind(owner_user_id)
                .bind(listing_id)
                .fetch_optional(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, Option<String>>(&sql)
                .bind(submission_id)
                .bind(owner_user_id)
                .bind(listing_id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(db_error)?
    .flatten();
    extracted.ok_or(AircraftRepairError::Stale)
}

enum PersistCorrection<'a> {
    Publisher {
        evidence: &'a str,
    },
    Visual {
        registration: &'a str,
        serial: Option<&'a str>,
        grounding: &'a AircraftGrounding,
        resolution: &'a VisualIdentifierResolution,
        media_url: &'a str,
    },
    FaaSerial {
        registration: &'a str,
        serial: &'a str,
        grounding: &'a AircraftGrounding,
    },
    SourceFaaSerial {
        observed_serial: &'a str,
        parsed: &'a ParsedListing,
        grounding: &'a AircraftGrounding,
    },
    SourceVisual {
        parsed: &'a ParsedListing,
        correction: &'a SourceVisualRegistrationCorrection,
        artifact: &'a PinnedSourceVisualCorrectionArtifact,
    },
}

async fn persist_correction(
    db: &AppDb,
    state: &RepairState,
    reviewer_id: i64,
    expected_state_sha256: &str,
    correction: PersistCorrection<'_>,
) -> Result<i64, AircraftRepairError> {
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool
                .begin()
                .await
                .map_err(|error| AircraftRepairError::Database(error.to_string()))?;
            let decision = persist_correction_sqlite(
                &mut transaction,
                state,
                reviewer_id,
                expected_state_sha256,
                correction,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(|error| AircraftRepairError::Database(error.to_string()))?;
            Ok(decision)
        }
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool
                .begin()
                .await
                .map_err(|error| AircraftRepairError::Database(error.to_string()))?;
            sqlx::query("LOCK TABLE faa_registry_snapshots, faa_registry_coverage, faa_registry_aircraft IN SHARE MODE")
                .execute(&mut *transaction)
                .await
                .map_err(|error| AircraftRepairError::Database(error.to_string()))?;
            let decision = persist_correction_postgres(
                &mut transaction,
                state,
                reviewer_id,
                expected_state_sha256,
                correction,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(|error| AircraftRepairError::Database(error.to_string()))?;
            Ok(decision)
        }
    }
}

async fn persist_correction_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    expected: &RepairState,
    reviewer_id: i64,
    expected_state_sha256: &str,
    correction: PersistCorrection<'_>,
) -> Result<i64, AircraftRepairError> {
    let current = load_locked_state_sqlite(transaction, expected.listing_id).await?;
    validate_locked_state(&current, expected, expected_state_sha256)?;
    lock_and_validate_bound_submission_sqlite(transaction, &current).await?;
    persist_locked_sqlite(
        transaction,
        &current,
        reviewer_id,
        expected_state_sha256,
        correction,
    )
    .await
}

async fn persist_correction_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    expected: &RepairState,
    reviewer_id: i64,
    expected_state_sha256: &str,
    correction: PersistCorrection<'_>,
) -> Result<i64, AircraftRepairError> {
    let current = load_locked_state_postgres(transaction, expected.listing_id).await?;
    validate_locked_state(&current, expected, expected_state_sha256)?;
    lock_and_validate_bound_submission_postgres(transaction, &current).await?;
    persist_locked_postgres(
        transaction,
        &current,
        reviewer_id,
        expected_state_sha256,
        correction,
    )
    .await
}

fn bound_submission_sql(parameter: &str, suffix: &str) -> String {
    format!(
        r#"SELECT user_id, source_url, rendered_html, rendered_html_sha256,
                  extracted_listing_json, extraction_error, canonical_listing_id
           FROM plugin_submissions WHERE id = {parameter}{suffix}"#
    )
}

fn validate_bound_submission_state(
    submission: &BoundSubmissionState,
    state: &RepairState,
) -> Result<(), AircraftRepairError> {
    if submission.user_id != state.owner_user_id
        || submission.canonical_listing_id != Some(state.listing_id)
        || Some(submission.source_url.as_str()) != state.submission_source_url.as_deref()
        || Some(submission.rendered_html.as_str()) != state.rendered_html.as_deref()
        || Some(submission.rendered_html_sha256.as_str()) != state.rendered_html_sha256.as_deref()
        || submission.extracted_listing_json != state.extracted_listing_json
        || submission.extraction_error.is_some()
    {
        return Err(AircraftRepairError::Stale);
    }
    Ok(())
}

async fn lock_and_validate_bound_submission_sqlite(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    state: &RepairState,
) -> Result<(), AircraftRepairError> {
    let submission_id = state.submission_id.ok_or(AircraftRepairError::Stale)?;
    let submission = sqlx::query_as::<_, BoundSubmissionState>(&bound_submission_sql("?", ""))
        .bind(submission_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?
        .ok_or(AircraftRepairError::Stale)?;
    validate_bound_submission_state(&submission, state)
}

async fn lock_and_validate_bound_submission_postgres(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    state: &RepairState,
) -> Result<(), AircraftRepairError> {
    let submission_id = state.submission_id.ok_or(AircraftRepairError::Stale)?;
    let submission =
        sqlx::query_as::<_, BoundSubmissionState>(&bound_submission_sql("$1", " FOR UPDATE"))
            .bind(submission_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?
            .ok_or(AircraftRepairError::Stale)?;
    validate_bound_submission_state(&submission, state)
}

fn validate_locked_state(
    current: &RepairState,
    expected: &RepairState,
    expected_state_sha256: &str,
) -> Result<(), AircraftRepairError> {
    if current.owner_user_id != expected.owner_user_id
        || current.fingerprint() != expected_state_sha256
        || current.fingerprint() != expected.fingerprint()
    {
        return Err(AircraftRepairError::Stale);
    }
    let html = current
        .rendered_html
        .as_deref()
        .ok_or(AircraftRepairError::Validation(
            "retained source is unavailable",
        ))?;
    let digest = format!("{:x}", Sha256::digest(html.as_bytes()));
    if current.rendered_html_sha256.as_deref() != Some(digest.as_str()) {
        return Err(AircraftRepairError::Stale);
    }
    Ok(())
}

async fn load_locked_state_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    listing_id: i64,
) -> Result<RepairState, AircraftRepairError> {
    sqlx::query_as::<_, RepairState>(&locked_state_sql("?", ""))
        .bind(listing_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| AircraftRepairError::Database(error.to_string()))?
        .ok_or(AircraftRepairError::NotFound(listing_id))
}

async fn load_locked_state_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    listing_id: i64,
) -> Result<RepairState, AircraftRepairError> {
    sqlx::query_as::<_, RepairState>(&locked_state_sql("$1", " FOR UPDATE OF listing"))
        .bind(listing_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| AircraftRepairError::Database(error.to_string()))?
        .ok_or(AircraftRepairError::NotFound(listing_id))
}

fn locked_state_sql(listing_id_parameter: &str, suffix: &str) -> String {
    format!(
        r#"SELECT listing.id AS listing_id, listing.created_by_user_id AS owner_user_id,
                  listing.source_url AS listing_source_url, manufacturer.name AS manufacturer,
                  model.name AS model, variant.name AS variant, listing.model_year,
                  listing.registration_number, listing.serial_number,
                  submission.id AS submission_id, submission.source_url AS submission_source_url,
                  submission.rendered_html, submission.rendered_html_sha256,
                  submission.extracted_listing_json
           FROM aircraft_sale_listings listing
           JOIN aircraft_model_variants variant ON variant.id = listing.aircraft_model_variant_id
           JOIN aircraft_models model ON model.id = variant.aircraft_model_id
           JOIN aircraft_manufacturers manufacturer ON manufacturer.id = model.aircraft_manufacturer_id
           LEFT JOIN plugin_submissions submission ON submission.id = (
             SELECT candidate.id FROM plugin_submissions candidate
             WHERE candidate.canonical_listing_id = listing.id
             ORDER BY candidate.submitted_at DESC, candidate.id DESC LIMIT 1
           )
           WHERE listing.id = {listing_id_parameter}{suffix}"#
    )
}

async fn persist_locked_sqlite(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    state: &RepairState,
    reviewer_id: i64,
    expected_hash: &str,
    correction: PersistCorrection<'_>,
) -> Result<i64, AircraftRepairError> {
    let update_listing = matches!(
        &correction,
        PersistCorrection::Visual { .. } | PersistCorrection::FaaSerial { .. }
    );
    let material = correction_material(state, &correction)?;
    if let PersistCorrection::Visual { grounding, .. }
    | PersistCorrection::FaaSerial { grounding, .. }
    | PersistCorrection::SourceFaaSerial { grounding, .. } = &correction
    {
        ensure_current_faa_sqlite(tx, grounding).await?;
    }
    if let PersistCorrection::SourceVisual {
        correction,
        artifact,
        ..
    } = &correction
    {
        ensure_source_visual_faa_pair_sqlite(tx, correction, artifact).await?;
    }
    let source_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO curation_evidence_sources (source_url, resolved_url, source_title, publisher, source_domain, source_tier, content_sha256, retrieved_at) VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP) ON CONFLICT (source_url, content_sha256) DO NOTHING RETURNING id",
    )
    .bind(&material.source_url).bind(&material.source_url).bind(&material.source_title)
    .bind(&material.publisher).bind(&material.source_domain).bind(material.source_tier).bind(&material.content_sha256)
    .fetch_optional(&mut **tx).await.map_err(db_error)?;
    let source_id = match source_id {
        Some(source_id) => source_id,
        None => sqlx::query_scalar::<_, i64>(
            "SELECT id FROM curation_evidence_sources WHERE source_url = ? AND content_sha256 = ?",
        )
        .bind(&material.source_url)
        .bind(&material.content_sha256)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?,
    };
    let claim_id = insert_claim_sqlite(tx, source_id, state, &material).await?;
    let observation_id = insert_observation_sqlite(tx, state, &material).await?;
    if update_listing {
        sqlx::query("UPDATE aircraft_sale_listings SET registration_number = ?, serial_number = ?, ingestion_state = 'pending_review', ingestion_error = NULL, ingestion_completed_at = NULL, is_verified = 0, updated_at = CURRENT_TIMESTAMP WHERE id = ? AND created_by_user_id = ?")
            .bind(material.corrected_registration.as_deref()).bind(material.corrected_serial.as_deref())
            .bind(state.listing_id).bind(state.owner_user_id).execute(&mut **tx).await.map_err(db_error)?;
    }
    let decision_id = insert_decision_sqlite(
        tx,
        state,
        reviewer_id,
        expected_hash,
        observation_id,
        claim_id,
        &material,
    )
    .await?;
    if matches!(
        correction,
        PersistCorrection::SourceFaaSerial { .. } | PersistCorrection::SourceVisual { .. }
    ) {
        release_source_receipt_gate_sqlite(tx, state).await?;
    }
    Ok(decision_id)
}

async fn persist_locked_postgres(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    state: &RepairState,
    reviewer_id: i64,
    expected_hash: &str,
    correction: PersistCorrection<'_>,
) -> Result<i64, AircraftRepairError> {
    let update_listing = matches!(
        &correction,
        PersistCorrection::Visual { .. } | PersistCorrection::FaaSerial { .. }
    );
    let material = correction_material(state, &correction)?;
    if let PersistCorrection::Visual { grounding, .. }
    | PersistCorrection::FaaSerial { grounding, .. }
    | PersistCorrection::SourceFaaSerial { grounding, .. } = &correction
    {
        ensure_current_faa_postgres(tx, grounding).await?;
    }
    if let PersistCorrection::SourceVisual {
        correction,
        artifact,
        ..
    } = &correction
    {
        ensure_source_visual_faa_pair_postgres(tx, correction, artifact).await?;
    }
    let source_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO curation_evidence_sources (source_url, resolved_url, source_title, publisher, source_domain, source_tier, content_sha256, retrieved_at) VALUES ($1, $2, $3, $4, $5, $6, $7, CURRENT_TIMESTAMP) ON CONFLICT (source_url, content_sha256) DO NOTHING RETURNING id",
    )
    .bind(&material.source_url).bind(&material.source_url).bind(&material.source_title)
    .bind(&material.publisher).bind(&material.source_domain).bind(material.source_tier).bind(&material.content_sha256)
    .fetch_optional(&mut **tx).await.map_err(db_error)?;
    let source_id = match source_id {
        Some(source_id) => source_id,
        None => sqlx::query_scalar::<_, i64>("SELECT id FROM curation_evidence_sources WHERE source_url = $1 AND content_sha256 = $2")
            .bind(&material.source_url).bind(&material.content_sha256).fetch_one(&mut **tx).await.map_err(db_error)?,
    };
    let claim_id = insert_claim_postgres(tx, source_id, state, &material).await?;
    let observation_id = insert_observation_postgres(tx, state, &material).await?;
    if update_listing {
        sqlx::query("UPDATE aircraft_sale_listings SET registration_number = $1, serial_number = $2, ingestion_state = 'pending_review', ingestion_error = NULL, ingestion_completed_at = NULL, is_verified = FALSE, updated_at = CURRENT_TIMESTAMP WHERE id = $3 AND created_by_user_id = $4")
            .bind(material.corrected_registration.as_deref()).bind(material.corrected_serial.as_deref())
            .bind(state.listing_id).bind(state.owner_user_id).execute(&mut **tx).await.map_err(db_error)?;
    }
    let decision_id = insert_decision_postgres(
        tx,
        state,
        reviewer_id,
        expected_hash,
        observation_id,
        claim_id,
        &material,
    )
    .await?;
    if matches!(
        correction,
        PersistCorrection::SourceFaaSerial { .. } | PersistCorrection::SourceVisual { .. }
    ) {
        release_source_receipt_gate_postgres(tx, state).await?;
    }
    Ok(decision_id)
}

async fn release_source_receipt_gate_sqlite(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    state: &RepairState,
) -> Result<(), AircraftRepairError> {
    let changed = sqlx::query(
        r#"UPDATE aircraft_sale_listings
           SET ingestion_state = CASE WHEN EXISTS (
                 SELECT 1 FROM aircraft_sale_listing_pending_reviews review
                 WHERE review.listing_id = aircraft_sale_listings.id
               ) THEN 'pending_review' ELSE 'incomplete' END,
               ingestion_error = NULL, ingestion_completed_at = NULL,
               is_verified = 0, updated_at = CURRENT_TIMESTAMP
           WHERE id = ? AND created_by_user_id = ?
             AND ingestion_state = 'quarantined' AND ingestion_error = ?"#,
    )
    .bind(state.listing_id)
    .bind(state.owner_user_id)
    .bind(SOURCE_IDENTITY_RECEIPT_PENDING)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?
    .rows_affected();
    if changed != 1 {
        return Err(AircraftRepairError::Stale);
    }
    Ok(())
}

async fn release_source_receipt_gate_postgres(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    state: &RepairState,
) -> Result<(), AircraftRepairError> {
    let changed = sqlx::query(
        r#"UPDATE aircraft_sale_listings
           SET ingestion_state = CASE WHEN EXISTS (
                 SELECT 1 FROM aircraft_sale_listing_pending_reviews review
                 WHERE review.listing_id = aircraft_sale_listings.id
               ) THEN 'pending_review' ELSE 'incomplete' END,
               ingestion_error = NULL, ingestion_completed_at = NULL,
               is_verified = FALSE, updated_at = CURRENT_TIMESTAMP
           WHERE id = $1 AND created_by_user_id = $2
             AND ingestion_state = 'quarantined' AND ingestion_error = $3"#,
    )
    .bind(state.listing_id)
    .bind(state.owner_user_id)
    .bind(SOURCE_IDENTITY_RECEIPT_PENDING)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?
    .rows_affected();
    if changed != 1 {
        return Err(AircraftRepairError::Stale);
    }
    Ok(())
}

struct CorrectionMaterial {
    kind: &'static str,
    source_url: String,
    source_title: String,
    publisher: String,
    source_domain: String,
    source_tier: &'static str,
    content_sha256: String,
    evidence: String,
    claim_subject: String,
    claim_predicate: &'static str,
    claim_object: String,
    observed_make: Option<String>,
    observed_family: Option<String>,
    observed_designation: Option<String>,
    observed_model_year: Option<i64>,
    observed_registration: Option<String>,
    observed_serial: Option<String>,
    prior_registration: Option<String>,
    prior_serial: Option<String>,
    corrected_registration: Option<String>,
    corrected_serial: Option<String>,
    faa_snapshot_id: Option<i64>,
    faa_source_record_sha256: Option<String>,
    visual_json: Option<String>,
    payload_json: String,
    observation_sha256: String,
}

fn correction_material(
    state: &RepairState,
    correction: &PersistCorrection<'_>,
) -> Result<CorrectionMaterial, AircraftRepairError> {
    let submission_id = state.submission_id.ok_or(AircraftRepairError::Stale)?;
    let rendered_sha = state
        .rendered_html_sha256
        .as_deref()
        .ok_or(AircraftRepairError::Stale)?;
    let (
        kind,
        source_url,
        evidence,
        corrected_registration,
        corrected_serial,
        faa_snapshot_id,
        faa_record,
        visual_json,
        source_tier,
        content_sha256,
    ) = match correction {
        PersistCorrection::Publisher { evidence } => (
            "publisher_hierarchy",
            state
                .source_url()
                .ok_or(AircraftRepairError::Stale)?
                .to_string(),
            (*evidence).to_string(),
            state.registration_number.clone(),
            state.serial_number.clone(),
            None,
            None,
            None,
            "marketplace_observation",
            rendered_sha.to_string(),
        ),
        PersistCorrection::Visual {
            registration,
            serial,
            grounding,
            resolution,
            media_url,
        } => {
            let evidence = resolution
                .candidates
                .iter()
                .flat_map(|candidate| {
                    candidate
                        .evidence
                        .iter()
                        .map(|item| item.visible_text.as_str())
                })
                .collect::<Vec<_>>()
                .join(" | ");
            let photo_sha256 = resolution
                .photos
                .first()
                .map(|photo| photo.sha256.clone())
                .ok_or(AircraftRepairError::Validation(
                    "visual report has no photo audit",
                ))?;
            (
                "visual_identifier",
                (*media_url).to_string(),
                evidence,
                Some((*registration).to_string()),
                serial.map(str::to_string),
                Some(grounding.snapshot.id),
                Some(grounding.source_record_sha256.clone()),
                Some(
                    serde_json::to_string(resolution)
                        .map_err(|e| AircraftRepairError::Database(e.to_string()))?,
                ),
                "marketplace_observation",
                photo_sha256,
            )
        }
        PersistCorrection::FaaSerial {
            registration,
            serial,
            grounding,
        } => (
            "faa_serial",
            grounding.snapshot.source_url.clone(),
            format!("FAA MASTER registration {registration} manufacturer serial {serial}"),
            Some((*registration).to_string()),
            Some((*serial).to_string()),
            Some(grounding.snapshot.id),
            Some(grounding.source_record_sha256.clone()),
            None,
            "regulator_primary",
            grounding.snapshot.archive_sha256.clone(),
        ),
        PersistCorrection::SourceFaaSerial { grounding, .. } => (
            "faa_serial",
            grounding.snapshot.source_url.clone(),
            format!(
                "FAA MASTER registration {} manufacturer serial {}",
                grounding.n_number,
                grounding
                    .manufacturer_serial_raw
                    .as_deref()
                    .unwrap_or_default()
            ),
            Some(grounding.n_number.clone()),
            grounding.manufacturer_serial_raw.clone(),
            Some(grounding.snapshot.id),
            Some(grounding.source_record_sha256.clone()),
            None,
            "regulator_primary",
            grounding.snapshot.archive_sha256.clone(),
        ),
        PersistCorrection::SourceVisual {
            correction,
            artifact,
            ..
        } => {
            let resolution = &correction.resolution;
            let evidence = resolution
                .candidates
                .iter()
                .flat_map(|candidate| {
                    candidate
                        .evidence
                        .iter()
                        .map(|item| item.visible_text.as_str())
                })
                .collect::<Vec<_>>()
                .join(" | ");
            (
                "visual_identifier",
                correction.media_url.clone(),
                evidence,
                Some(correction.corrected_registration_number.clone()),
                correction.corrected_serial_number.clone(),
                Some(correction.grounding.snapshot.id),
                Some(correction.grounding.source_record_sha256.clone()),
                Some(artifact.visual_resolution_json.clone()),
                "marketplace_observation",
                artifact.primary_photo_sha256.clone(),
            )
        }
    };
    let (prior_registration, prior_serial, decision_origin) = match correction {
        PersistCorrection::SourceFaaSerial {
            observed_serial,
            parsed,
            ..
        } => (
            parsed.registration_number.clone(),
            Some((*observed_serial).to_string()),
            "source_materialization",
        ),
        PersistCorrection::SourceVisual { parsed, .. } => (
            parsed.registration_number.clone(),
            parsed.serial_number.clone(),
            "source_materialization",
        ),
        _ => (
            state.registration_number.clone(),
            state.serial_number.clone(),
            "existing_listing_repair",
        ),
    };
    let payload = serde_json::json!({"version":1,"kind":kind,"origin":decision_origin,"listing_id":state.listing_id,"submission_id":submission_id,"prior_registration_number":prior_registration,"prior_serial_number":prior_serial,"corrected_registration_number":corrected_registration,"corrected_serial_number":corrected_serial,"faa_registry_snapshot_id":faa_snapshot_id,"faa_source_record_sha256":faa_record});
    let payload_json = serde_json::to_string(&payload).expect("decision payload serializes");
    let observation_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&serde_json::json!({"schema":"aircraft_listing_identity_correction_observation_v1","listing_id":state.listing_id,"submission_id":submission_id,"rendered_html_sha256":rendered_sha,"evidence":evidence,"payload":payload})).expect("observation serializes")));
    let url = Url::parse(&source_url)
        .map_err(|_| AircraftRepairError::Validation("evidence source URL is invalid"))?;
    let source_domain = url
        .host_str()
        .ok_or(AircraftRepairError::Validation(
            "evidence source URL has no host",
        ))?
        .to_string();
    let source_title = match kind {
        "visual_identifier" => "Listing photo aircraft identity",
        "faa_serial" => "FAA releasable aircraft registry",
        _ => "Retained listing aircraft hierarchy",
    };
    let (
        claim_subject,
        claim_predicate,
        claim_object,
        observed_make,
        observed_family,
        observed_designation,
        observed_model_year,
        observed_registration,
        observed_serial,
    ) = match kind {
        "publisher_hierarchy" => (
            format!("aircraft listing {}", state.listing_id),
            "states exact aircraft hierarchy",
            format!("{} {} {}", state.manufacturer, state.model, state.variant),
            Some(state.manufacturer.clone()),
            Some(state.model.clone()),
            Some(state.variant.clone()),
            None,
            None,
            None,
        ),
        "visual_identifier" => (
            format!("listing photo for aircraft listing {}", state.listing_id),
            "shows exact aircraft identifier",
            corrected_registration.clone().unwrap_or_default(),
            None,
            None,
            None,
            None,
            corrected_registration.clone(),
            None,
        ),
        "faa_serial" => (
            corrected_registration.clone().unwrap_or_default(),
            "FAA assigns exact manufacturer serial",
            corrected_serial.clone().unwrap_or_default(),
            None,
            None,
            None,
            None,
            corrected_registration.clone(),
            corrected_serial.clone(),
        ),
        _ => return Err(AircraftRepairError::Validation("unknown correction kind")),
    };
    Ok(CorrectionMaterial {
        kind,
        source_url,
        source_title: source_title.to_string(),
        publisher: source_domain.clone(),
        source_domain,
        source_tier,
        content_sha256,
        evidence,
        claim_subject,
        claim_predicate,
        claim_object,
        observed_make,
        observed_family,
        observed_designation,
        observed_model_year,
        observed_registration,
        observed_serial,
        prior_registration,
        prior_serial,
        corrected_registration,
        corrected_serial,
        faa_snapshot_id,
        faa_source_record_sha256: faa_record,
        visual_json,
        payload_json,
        observation_sha256,
    })
}

macro_rules! insert_helpers {
    ($claim:ident, $observation:ident, $decision:ident, $db:ty, $p1:literal, $p2:literal, $p3:literal, $p4:literal, $p5:literal, $p6:literal, $p7:literal, $p8:literal, $p9:literal, $p10:literal, $p11:literal, $p12:literal, $p13:literal, $p14:literal, $p15:literal, $p16:literal, $p17:literal, $p18:literal) => {
        async fn $claim(tx: &mut sqlx::Transaction<'_, $db>, source_id: i64, _state: &RepairState, material: &CorrectionMaterial) -> Result<i64, AircraftRepairError> {
            sqlx::query_scalar::<_, i64>(concat!("INSERT INTO curation_evidence_claims (evidence_source_id, claim_kind, subject_text, predicate_text, object_text, quoted_evidence, validation_status, validated_at) VALUES (",$p1,", 'identity', ",$p2,", ",$p3,", ",$p4,", ",$p5,", 'validated', CURRENT_TIMESTAMP) RETURNING id"))
                .bind(source_id).bind(&material.claim_subject).bind(material.claim_predicate).bind(&material.claim_object).bind(&material.evidence).fetch_one(&mut **tx).await.map_err(db_error)
        }
        async fn $observation(tx: &mut sqlx::Transaction<'_, $db>, state: &RepairState, material: &CorrectionMaterial) -> Result<i64, AircraftRepairError> {
            sqlx::query_scalar::<_, i64>(concat!("INSERT INTO aircraft_identity_observations (aircraft_sale_listing_id, source_url, observed_make, observed_family, observed_designation, model_year, serial_number, registration_number, market_code, exact_source_evidence, observation_sha256) VALUES (",$p1,", ",$p2,", ",$p3,", ",$p4,", ",$p5,", ",$p6,", ",$p7,", ",$p8,", 'US', ",$p9,", ",$p10,") RETURNING id"))
                .bind(state.listing_id).bind(&material.source_url).bind(material.observed_make.as_deref()).bind(material.observed_family.as_deref()).bind(material.observed_designation.as_deref()).bind(material.observed_model_year).bind(material.observed_serial.as_deref()).bind(material.observed_registration.as_deref()).bind(&material.evidence).bind(&material.observation_sha256).fetch_one(&mut **tx).await.map_err(db_error)
        }
        async fn $decision(tx: &mut sqlx::Transaction<'_, $db>, state: &RepairState, reviewer_id: i64, expected_hash: &str, observation_id: i64, claim_id: i64, material: &CorrectionMaterial) -> Result<i64, AircraftRepairError> {
            sqlx::query_scalar::<_, i64>(concat!("INSERT INTO aircraft_listing_identity_correction_decisions (aircraft_sale_listing_id, observation_id, evidence_claim_id, correction_kind, expected_state_sha256, plugin_submission_id, rendered_html_sha256, prior_registration_number, prior_serial_number, corrected_registration_number, corrected_serial_number, faa_registry_snapshot_id, faa_source_record_sha256, visual_resolution_json, decision_payload_json, decided_by_user_id, decided_at) VALUES (",$p1,", ",$p2,", ",$p3,", ",$p4,", ",$p5,", ",$p6,", ",$p7,", ",$p8,", ",$p9,", ",$p10,", ",$p11,", ",$p12,", ",$p13,", ",$p14,", ",$p15,", ",$p16,", CURRENT_TIMESTAMP) RETURNING id"))
                .bind(state.listing_id).bind(observation_id).bind(claim_id).bind(material.kind).bind(expected_hash).bind(state.submission_id).bind(state.rendered_html_sha256.as_deref()).bind(material.prior_registration.as_deref()).bind(material.prior_serial.as_deref()).bind(material.corrected_registration.as_deref()).bind(material.corrected_serial.as_deref()).bind(material.faa_snapshot_id).bind(material.faa_source_record_sha256.as_deref()).bind(material.visual_json.as_deref()).bind(&material.payload_json).bind(reviewer_id).fetch_one(&mut **tx).await.map_err(db_error)
        }
    }
}

insert_helpers!(
    insert_claim_sqlite,
    insert_observation_sqlite,
    insert_decision_sqlite,
    Sqlite,
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?",
    "?"
);
insert_helpers!(
    insert_claim_postgres,
    insert_observation_postgres,
    insert_decision_postgres,
    Postgres,
    "$1",
    "$2",
    "$3",
    "$4",
    "$5",
    "$6",
    "$7",
    "$8",
    "$9",
    "$10",
    "$11",
    "$12",
    "$13",
    "$14",
    "$15",
    "$16",
    "$17",
    "$18"
);

async fn ensure_source_visual_faa_pair_sqlite(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    correction: &SourceVisualRegistrationCorrection,
    artifact: &PinnedSourceVisualCorrectionArtifact,
) -> Result<(), AircraftRepairError> {
    let count = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM aircraft_source_visual_correction_artifacts pinned
        JOIN plugin_submissions submission ON submission.id = pinned.plugin_submission_id
        JOIN faa_registry_snapshots snapshot ON snapshot.id = pinned.faa_registry_snapshot_id
        JOIN faa_registry_coverage observed
          ON observed.snapshot_id = snapshot.id
         AND observed.n_number = pinned.observed_registration_number
         AND observed.lookup_status = 'absent'
        JOIN faa_registry_coverage corrected
          ON corrected.snapshot_id = snapshot.id
         AND corrected.n_number = pinned.corrected_registration_number
         AND corrected.lookup_status = 'matched'
        JOIN faa_registry_aircraft aircraft
          ON aircraft.snapshot_id = snapshot.id
         AND aircraft.n_number = corrected.n_number
        WHERE pinned.plugin_submission_id = ?
          AND pinned.rendered_html_sha256 = submission.rendered_html_sha256
          AND pinned.observed_registration_number = ?
          AND pinned.corrected_registration_number = ?
          AND pinned.corrected_serial_number IS ?
          AND snapshot.id = (SELECT id FROM faa_registry_snapshots ORDER BY snapshot_date DESC, id DESC LIMIT 1)
          AND snapshot.archive_sha256 = ?
          AND aircraft.source_record_sha256 = ?
          AND aircraft.manufacturer_serial_raw IS pinned.corrected_serial_number
        "#,
    )
    .bind(artifact.plugin_submission_id)
    .bind(correction.observed_registration_number.as_str())
    .bind(correction.corrected_registration_number.as_str())
    .bind(correction.corrected_serial_number.as_deref())
    .bind(correction.grounding.snapshot.archive_sha256.as_str())
    .bind(correction.grounding.source_record_sha256.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?;
    if count != 1 {
        return Err(AircraftRepairError::Stale);
    }
    Ok(())
}

async fn ensure_source_visual_faa_pair_postgres(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    correction: &SourceVisualRegistrationCorrection,
    artifact: &PinnedSourceVisualCorrectionArtifact,
) -> Result<(), AircraftRepairError> {
    let count = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM public.aircraft_source_visual_correction_artifacts pinned
        JOIN public.plugin_submissions submission ON submission.id = pinned.plugin_submission_id
        JOIN public.faa_registry_snapshots snapshot ON snapshot.id = pinned.faa_registry_snapshot_id
        JOIN public.faa_registry_coverage observed
          ON observed.snapshot_id = snapshot.id
         AND observed.n_number = pinned.observed_registration_number
         AND observed.lookup_status = 'absent'
        JOIN public.faa_registry_coverage corrected
          ON corrected.snapshot_id = snapshot.id
         AND corrected.n_number = pinned.corrected_registration_number
         AND corrected.lookup_status = 'matched'
        JOIN public.faa_registry_aircraft aircraft
          ON aircraft.snapshot_id = snapshot.id
         AND aircraft.n_number = corrected.n_number
        WHERE pinned.plugin_submission_id = $1
          AND pinned.rendered_html_sha256 = submission.rendered_html_sha256
          AND pinned.observed_registration_number = $2
          AND pinned.corrected_registration_number = $3
          AND pinned.corrected_serial_number IS NOT DISTINCT FROM $4
          AND snapshot.id = (SELECT id FROM public.faa_registry_snapshots ORDER BY snapshot_date DESC, id DESC LIMIT 1)
          AND snapshot.archive_sha256 = $5
          AND aircraft.source_record_sha256 = $6
          AND aircraft.manufacturer_serial_raw IS NOT DISTINCT FROM pinned.corrected_serial_number
        "#,
    )
    .bind(artifact.plugin_submission_id)
    .bind(correction.observed_registration_number.as_str())
    .bind(correction.corrected_registration_number.as_str())
    .bind(correction.corrected_serial_number.as_deref())
    .bind(correction.grounding.snapshot.archive_sha256.as_str())
    .bind(correction.grounding.source_record_sha256.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?;
    if count != 1 {
        return Err(AircraftRepairError::Stale);
    }
    Ok(())
}

async fn ensure_current_faa_sqlite(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    grounding: &AircraftGrounding,
) -> Result<(), AircraftRepairError> {
    ensure_current_faa_query(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM faa_registry_snapshots snapshot JOIN faa_registry_coverage coverage ON coverage.snapshot_id = snapshot.id AND coverage.n_number = ? AND coverage.lookup_status = 'matched' JOIN faa_registry_aircraft aircraft ON aircraft.snapshot_id = snapshot.id AND aircraft.n_number = coverage.n_number WHERE snapshot.snapshot_date = (SELECT snapshot_date FROM faa_registry_snapshots ORDER BY snapshot_date DESC, id DESC LIMIT 1) AND snapshot.archive_sha256 = (SELECT archive_sha256 FROM faa_registry_snapshots ORDER BY snapshot_date DESC, id DESC LIMIT 1) AND snapshot.id = ? AND aircraft.source_record_sha256 = ?").bind(&grounding.n_number).bind(grounding.snapshot.id).bind(&grounding.source_record_sha256).fetch_one(&mut **tx).await, grounding).await
}
async fn ensure_current_faa_postgres(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    grounding: &AircraftGrounding,
) -> Result<(), AircraftRepairError> {
    ensure_current_faa_query(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM faa_registry_snapshots snapshot JOIN faa_registry_coverage coverage ON coverage.snapshot_id = snapshot.id AND coverage.n_number = $1 AND coverage.lookup_status = 'matched' JOIN faa_registry_aircraft aircraft ON aircraft.snapshot_id = snapshot.id AND aircraft.n_number = coverage.n_number WHERE snapshot.snapshot_date = (SELECT snapshot_date FROM faa_registry_snapshots ORDER BY snapshot_date DESC, id DESC LIMIT 1) AND snapshot.archive_sha256 = (SELECT archive_sha256 FROM faa_registry_snapshots ORDER BY snapshot_date DESC, id DESC LIMIT 1) AND snapshot.id = $2 AND aircraft.source_record_sha256 = $3").bind(&grounding.n_number).bind(grounding.snapshot.id).bind(&grounding.source_record_sha256).fetch_one(&mut **tx).await, grounding).await
}
async fn ensure_current_faa_query(
    result: Result<i64, sqlx::Error>,
    _grounding: &AircraftGrounding,
) -> Result<(), AircraftRepairError> {
    if result.map_err(db_error)? != 1 {
        return Err(AircraftRepairError::Stale);
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn db_error(error: sqlx::Error) -> AircraftRepairError {
    AircraftRepairError::Database(error.to_string())
}
fn admission_error(error: AircraftAdmissionError) -> AircraftRepairError {
    match error {
        AircraftAdmissionError::ListingNotFound { listing_id } => {
            AircraftRepairError::NotFound(listing_id)
        }
        AircraftAdmissionError::LookupFailed { message, .. } => {
            AircraftRepairError::Database(message)
        }
        AircraftAdmissionError::Rejected { reason, .. } => {
            AircraftRepairError::Validation(block_reason_code(&reason))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::faa::{
        store_release, AircraftRecord, AircraftReference, MemberProvenance, Release,
        ReleaseFixtureBuilder, ReleaseMetadata, TargetCoverage,
    };

    fn release(n_number: &str, serial: &str, matched: bool) -> Release {
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
            matched,
        }])
        .aircraft(
            matched
                .then(|| AircraftRecord {
                    n_number: n_number.to_string(),
                    manufacturer_serial_raw: Some(serial.to_string()),
                    manufacturer_serial_key: crate::aircraft::faa::normalize_serial_key(serial),
                    aircraft_code: "2072738".to_string(),
                    engine_code: None,
                    year_manufactured: Some(2020),
                    source_record_sha256: "1".repeat(64),
                })
                .into_iter()
                .collect(),
        )
        .aircraft_references(
            matched
                .then(|| AircraftReference {
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
                })
                .into_iter()
                .collect(),
        )
        .build()
    }

    async fn repair_fixture(registration: Option<&str>, serial: Option<&str>) -> (AppDb, i64, i64) {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let user_id: i64 = sqlx::query_scalar("SELECT id FROM users ORDER BY id LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap();
        let variant_id: i64 = sqlx::query_scalar("SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1")
            .fetch_one(pool).await.unwrap();
        let source_url = "https://www.controller.com/listing/for-sale/123/test-aircraft";
        let listing_id: i64 = sqlx::query_scalar("INSERT INTO aircraft_sale_listings (aircraft_model_variant_id, created_by_user_id, source_url, model_year, asking_price_usd, airframe_hours, registration_number, serial_number, ingestion_state) VALUES (?, ?, ?, 2020, 400000, 900, ?, ?, 'pending_review') RETURNING id")
            .bind(variant_id).bind(user_id).bind(source_url).bind(registration).bind(serial)
            .fetch_one(pool).await.unwrap();
        let install_id: i64 = sqlx::query_scalar("INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'test-key') RETURNING id")
            .bind(user_id).fetch_one(pool).await.unwrap();
        let rendered_html = format!(
            "<main>2020 Cessna 182T Skylane Registration {} Serial {}</main>",
            registration.unwrap_or("unavailable"),
            serial.unwrap_or("unavailable")
        );
        let rendered_sha = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        sqlx::query("INSERT INTO plugin_submissions (user_id, plugin_install_id, source_url, rendered_html, rendered_html_sha256, signature_base64, canonical_listing_id, extracted_listing_json) VALUES (?, ?, ?, ?, ?, 'test-signature', ?, '{}')")
            .bind(user_id).bind(install_id).bind(source_url).bind(&rendered_html).bind(rendered_sha).bind(listing_id)
            .execute(pool).await.unwrap();
        (db, user_id, listing_id)
    }

    #[test]
    fn repair_state_hash_changes_with_identity_and_capture() {
        let state = RepairState {
            listing_id: 1,
            owner_user_id: 2,
            listing_source_url: Some("https://www.controller.com/listing/for-sale/1".into()),
            manufacturer: "Cessna".into(),
            model: "182".into(),
            variant: "182T".into(),
            model_year: 2020,
            registration_number: Some("N1AB".into()),
            serial_number: Some("1".into()),
            submission_id: Some(3),
            submission_source_url: None,
            rendered_html: Some("body".into()),
            rendered_html_sha256: Some("a".repeat(64)),
            extracted_listing_json: Some("{}".into()),
        };
        let original = state.fingerprint();
        let mut changed = state.clone();
        changed.serial_number = Some("2".into());
        assert_ne!(original, changed.fingerprint());
        changed = state.clone();
        changed.rendered_html_sha256 = Some("b".repeat(64));
        assert_ne!(original, changed.fingerprint());
    }

    #[test]
    fn only_target_coverage_has_the_import_required_code() {
        assert_eq!(
            safe_visual_faa_block(&BlockReason::RegistrationNotCovered),
            "faa_target_import_required"
        );
        assert_eq!(
            safe_visual_faa_block(&BlockReason::RegistrationNotFound),
            "recovered_registration_not_found"
        );
        assert_eq!(
            safe_visual_faa_block(&BlockReason::SerialConflict),
            "recovered_serial_conflict"
        );
    }

    #[test]
    fn missing_registration_is_an_explicit_visual_repair_reason() {
        let blocker = match BlockReason::MissingRegistration {
            BlockReason::MissingRegistration => Some(RepairBlocker::Visual("missing_registration")),
            _ => None,
        };
        assert_eq!(blocker, Some(RepairBlocker::Visual("missing_registration")));
    }

    #[test]
    fn locked_state_queries_use_backend_specific_parameters() {
        assert!(locked_state_sql("?", "").contains("WHERE listing.id = ?"));
        assert!(locked_state_sql("$1", " FOR UPDATE OF listing")
            .contains("WHERE listing.id = $1 FOR UPDATE OF listing"));
        let submission_lock = bound_submission_sql("$1", " FOR UPDATE");
        assert!(submission_lock.contains("WHERE id = $1 FOR UPDATE"));
        for guarded_field in [
            "user_id",
            "source_url",
            "rendered_html",
            "rendered_html_sha256",
            "extracted_listing_json",
            "extraction_error",
            "canonical_listing_id",
        ] {
            assert!(submission_lock.contains(guarded_field));
        }
    }

    #[tokio::test]
    async fn faa_serial_repair_is_provider_free_cas_guarded_and_immutable() {
        let (db, user_id, listing_id) = repair_fixture(Some("N482TW"), Some("1823006")).await;
        store_release(&db, &release("N482TW", "18283006", true))
            .await
            .unwrap();
        let AircraftRepairPreflight::Available {
            expected_state_sha256,
            actions,
            ..
        } = preflight_aircraft_repair(&db, user_id, listing_id)
            .await
            .unwrap()
        else {
            panic!("serial conflict must be repairable");
        };
        assert_eq!(actions, vec![AircraftRepairAction::FaaSerial]);
        let before_usage: i64 = match db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
                    .fetch_one(pool)
                    .await
                    .unwrap()
            }
            _ => unreachable!(),
        };
        let outcome = correct_serial_from_current_faa(
            &db,
            user_id,
            listing_id,
            &FaaSerialAircraftRepairRequest {
                expected_state_sha256: expected_state_sha256.clone(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            AircraftRepairOutcome::Applied {
                serial_number: Some(ref serial),
                ..
            } if serial == "18283006"
        ));
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let stored: (Option<String>, i64, i64) = sqlx::query_as(
            "SELECT listing.serial_number, (SELECT COUNT(*) FROM aircraft_listing_identity_correction_decisions WHERE aircraft_sale_listing_id = listing.id AND correction_kind = 'faa_serial'), (SELECT COUNT(*) FROM gemini_api_usage) FROM aircraft_sale_listings listing WHERE listing.id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored, (Some("18283006".to_string()), 1, before_usage));

        assert!(matches!(
            correct_serial_from_current_faa(
                &db,
                user_id,
                listing_id,
                &FaaSerialAircraftRepairRequest {
                    expected_state_sha256,
                },
            )
            .await,
            Err(AircraftRepairError::Stale)
        ));
        let update = sqlx::query(
            "UPDATE aircraft_listing_identity_correction_decisions SET prior_serial_number = 'erased' WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .execute(pool)
        .await;
        assert!(update.is_err(), "correction decisions must be immutable");
    }

    #[tokio::test]
    async fn non_narrow_faa_serial_conflict_is_never_offered_or_auto_cemented() {
        let (db, user_id, listing_id) = repair_fixture(Some("N482TW"), Some("UNRELATED99")).await;
        store_release(&db, &release("N482TW", "18283006", true))
            .await
            .unwrap();
        assert_eq!(
            preflight_aircraft_repair(&db, user_id, listing_id)
                .await
                .unwrap(),
            AircraftRepairPreflight::Unavailable {
                listing_id,
                reason_code: "serial_conflict_requires_explicit_evidence",
            }
        );
        let state = load_state(&db, listing_id).await.unwrap();
        let outcome = correct_serial_from_current_faa(
            &db,
            user_id,
            listing_id,
            &FaaSerialAircraftRepairRequest {
                expected_state_sha256: state.fingerprint(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            AircraftRepairOutcome::Blocked {
                listing_id,
                reason_code: "faa_serial_repair_not_allowed",
            }
        );
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let unchanged: (Option<String>, i64) = sqlx::query_as(
            "SELECT serial_number, (SELECT count(*) FROM aircraft_listing_identity_correction_decisions WHERE aircraft_sale_listing_id = ?) FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(unchanged, (Some("UNRELATED99".to_string()), 0));
    }

    #[tokio::test]
    async fn visually_unchanged_registration_absent_from_current_master_never_mutates() {
        let (db, user_id, listing_id) = repair_fixture(Some("N182PF"), None).await;
        store_release(&db, &release("N182PF", "18258918", false))
            .await
            .unwrap();
        let AircraftRepairPreflight::Available {
            expected_state_sha256,
            actions,
            ..
        } = preflight_aircraft_repair(&db, user_id, listing_id)
            .await
            .unwrap()
        else {
            panic!("missing current MASTER assignment must permit visual confirmation only");
        };
        assert_eq!(actions, vec![AircraftRepairAction::VisualIdentifier]);
        let resolution = VisualIdentifierResolution {
            status: crate::aircraft::curation::visual::VisualIdentifierStatus::CandidatesVisible,
            candidates: Vec::new(),
            registration_consensus:
                crate::aircraft::curation::visual::VisualRegistrationConsensus {
                    status: VisualConsensusStatus::AutoAccept,
                    basis: crate::aircraft::curation::visual::VisualConsensusBasis::SingleRegistrationImage,
                    normalized_n_number: Some("N182PF".to_string()),
                    literal_registrations: vec!["N182PF".to_string()],
                    literal_serials: Vec::new(),
                    registration_evidence_count: 1,
                    serial_evidence_count: 0,
                    supporting_image_ids: vec!["asset-current".to_string()],
                    reason: "one complete visible N-number".to_string(),
                },
            refusal_reason: None,
            photos: vec![crate::aircraft::curation::visual::VisualPhotoAudit {
                image_id: "asset-current".to_string(),
                mime_type: "image/jpeg".to_string(),
                byte_count: 10,
                sha256: "9".repeat(64),
            }],
            interaction_id: Some("interaction-1".to_string()),
            model: "gemini-3.1-flash-lite".to_string(),
            prompt_version: "aircraft-visible-identifier-v1".to_string(),
            schema_version: "aircraft-visible-identifier-schema-v1".to_string(),
            total_input_tokens: Some(100),
            total_output_tokens: Some(20),
        };
        let outcome = apply_visual_resolution(
            &db,
            user_id,
            listing_id,
            &expected_state_sha256,
            &resolution,
            "https://media.sandhills.com/current.jpg",
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            AircraftRepairOutcome::Blocked {
                listing_id,
                reason_code: "recovered_registration_not_found",
            }
        );
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let unchanged: (Option<String>, Option<String>, i64) = sqlx::query_as(
            "SELECT registration_number, serial_number, (SELECT COUNT(*) FROM aircraft_listing_identity_correction_decisions WHERE aircraft_sale_listing_id = ?) FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(unchanged, (Some("N182PF".to_string()), None, 0));
    }

    #[tokio::test]
    async fn visual_receipt_attributes_only_the_registration_to_the_photo() {
        let (db, user_id, listing_id) = repair_fixture(None, None).await;
        store_release(&db, &release("N182PF", "18258918", true))
            .await
            .unwrap();
        let state = load_state(&db, listing_id).await.unwrap();
        let resolution = VisualIdentifierResolution {
            status: crate::aircraft::curation::visual::VisualIdentifierStatus::CandidatesVisible,
            candidates: vec![
                crate::aircraft::curation::visual::VisibleAircraftIdentifier {
                    kind: VisibleIdentifierKind::Registration,
                    visible_text: "N182PF".to_string(),
                    evidence_count: 1,
                    evidence: vec![
                        crate::aircraft::curation::visual::VisualIdentifierImageEvidence {
                            image_id: "asset-registration".to_string(),
                            visible_text: "N182PF".to_string(),
                            confidence:
                                crate::aircraft::curation::visual::VisualEvidenceConfidence::VeryHigh,
                            box_2d: [10, 20, 30, 40],
                            visibility_basis:
                                crate::aircraft::curation::visual::VisibilityBasis::ExteriorRegistrationMarking,
                            location_description: "fuselage".to_string(),
                        },
                    ],
                },
            ],
            registration_consensus:
                crate::aircraft::curation::visual::VisualRegistrationConsensus {
                    status: VisualConsensusStatus::AutoAccept,
                    basis:
                        crate::aircraft::curation::visual::VisualConsensusBasis::SingleRegistrationImage,
                    normalized_n_number: Some("N182PF".to_string()),
                    literal_registrations: vec!["N182PF".to_string()],
                    literal_serials: Vec::new(),
                    registration_evidence_count: 1,
                    serial_evidence_count: 0,
                    supporting_image_ids: vec!["asset-registration".to_string()],
                    reason: "one complete visible N-number".to_string(),
                },
            refusal_reason: None,
            photos: vec![crate::aircraft::curation::visual::VisualPhotoAudit {
                image_id: "asset-registration".to_string(),
                mime_type: "image/jpeg".to_string(),
                byte_count: 10,
                sha256: "9".repeat(64),
            }],
            interaction_id: Some("interaction-registration".to_string()),
            model: "gemini-3.1-flash-lite".to_string(),
            prompt_version: "aircraft-visible-identifier-v1".to_string(),
            schema_version: "aircraft-visible-identifier-schema-v1".to_string(),
            total_input_tokens: Some(100),
            total_output_tokens: Some(20),
        };

        let outcome = apply_visual_resolution(
            &db,
            user_id,
            listing_id,
            &state.fingerprint(),
            &resolution,
            "https://media.sandhills.com/registration.jpg",
        )
        .await
        .unwrap();
        assert!(matches!(outcome, AircraftRepairOutcome::Applied { .. }));
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let attributed: (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        ) = sqlx::query_as(
            r#"SELECT observation.observed_make, observation.observed_family,
                      observation.observed_designation, observation.model_year,
                      observation.serial_number, observation.registration_number,
                      claim.object_text, decision.corrected_serial_number
               FROM aircraft_listing_identity_correction_decisions decision
               JOIN aircraft_identity_observations observation
                 ON observation.id = decision.observation_id
               JOIN curation_evidence_claims claim
                 ON claim.id = decision.evidence_claim_id
               WHERE decision.aircraft_sale_listing_id = ?
                 AND decision.correction_kind = 'visual_identifier'"#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            attributed,
            (
                None,
                None,
                None,
                None,
                None,
                Some("N182PF".to_string()),
                "N182PF".to_string(),
                Some("18258918".to_string()),
            )
        );
    }

    #[tokio::test]
    async fn publisher_corroboration_is_consumed_only_for_the_bound_current_capture() {
        let (db, user_id, listing_id) = repair_fixture(Some("N482TW"), Some("18283006")).await;
        store_release(&db, &release("N482TW", "18283006", true))
            .await
            .unwrap();
        let state = load_state(&db, listing_id).await.unwrap();
        let exact_evidence = format!("{} {} {}", state.manufacturer, state.model, state.variant);
        let repeated_make = std::iter::repeat_n(state.manufacturer.as_str(), 130)
            .collect::<Vec<_>>()
            .join(" ");
        let rendered_html = format!("<main><p>{repeated_make}</p><h1>{exact_evidence}</h1></main>");
        let rendered_sha = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE plugin_submissions SET rendered_html = ?, rendered_html_sha256 = ? WHERE canonical_listing_id = ?",
        )
        .bind(&rendered_html)
        .bind(&rendered_sha)
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();

        let AircraftRepairPreflight::Available {
            expected_state_sha256,
            actions,
            ..
        } = preflight_aircraft_repair(&db, user_id, listing_id)
            .await
            .unwrap()
        else {
            panic!("work-limited automatic evidence must offer publisher corroboration");
        };
        assert_eq!(actions, vec![AircraftRepairAction::PublisherHierarchy]);
        let outcome = corroborate_publisher_hierarchy(
            &db,
            user_id,
            listing_id,
            &PublisherAircraftRepairRequest {
                expected_state_sha256: expected_state_sha256.clone(),
                exact_evidence_text: exact_evidence.clone(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(outcome, AircraftRepairOutcome::Applied { .. }));

        let observations = crate::aircraft::observations::load_aircraft_identity_observations(
            &db,
            1,
            Some(listing_id),
        )
        .await
        .unwrap();
        let observation = observations.observations.first().unwrap();
        assert_eq!(
            observation.source_kind,
            "reviewer_corroborated_retained_submission"
        );
        assert!(observation.source_excerpt_is_exact);
        assert_eq!(
            observation.source_excerpt.as_deref(),
            Some(exact_evidence.as_str())
        );
        let persisted_model_year: Option<i64> = sqlx::query_scalar(
            "SELECT observation.model_year FROM aircraft_identity_observations observation JOIN aircraft_listing_identity_correction_decisions decision ON decision.observation_id = observation.id WHERE decision.aircraft_sale_listing_id = ? AND decision.correction_kind = 'publisher_hierarchy'",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(persisted_model_year, None);
        let next = preflight_listing_aircraft_verification(&db, listing_id)
            .await
            .unwrap();
        assert!(!matches!(
            next,
            AircraftVerificationOutcome::Pending {
                reason: AircraftVerificationPendingReason::SourceEvidenceMissing,
                ..
            }
        ));

        sqlx::query(
            "UPDATE plugin_submissions SET rendered_html = '<main>changed</main>', rendered_html_sha256 = ? WHERE canonical_listing_id = ?",
        )
        .bind(format!("{:x}", Sha256::digest(b"<main>changed</main>")))
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();
        let stale_observation = crate::aircraft::observations::load_aircraft_identity_observations(
            &db,
            1,
            Some(listing_id),
        )
        .await
        .unwrap();
        assert_ne!(
            stale_observation.observations[0].source_kind,
            "reviewer_corroborated_retained_submission"
        );
        assert!(matches!(
            corroborate_publisher_hierarchy(
                &db,
                user_id,
                listing_id,
                &PublisherAircraftRepairRequest {
                    expected_state_sha256,
                    exact_evidence_text: exact_evidence,
                },
            )
            .await,
            Err(AircraftRepairError::Stale)
        ));
    }

    #[tokio::test]
    async fn bound_source_serial_correction_preserves_raw_checkpoint_and_records_faa_value() {
        let (db, user_id, listing_id) = repair_fixture(Some("N482TW"), Some("18283006")).await;
        store_release(&db, &release("N482TW", "18283006", true))
            .await
            .unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let submission_id: i64 =
            sqlx::query_scalar("SELECT id FROM plugin_submissions WHERE canonical_listing_id = ?")
                .bind(listing_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let rendered_html = "<main>Registration N482TW, Serial Number 1823006</main>";
        let rendered_sha = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        let raw_checkpoint = serde_json::json!({
            "manufacturer":"Cessna","model":"182","variant":"182T","model_year":2020,
            "asking_price_usd":400000.0,"currency":"USD","airframe_hours":900.0,
            "engine_hours":null,"engine_time_basis":"unknown","engine_time_evidence":null,
            "engine_time_confidence":null,"propeller_hours":null,
            "propeller_time_basis":"unknown","propeller_time_evidence":null,
            "propeller_time_confidence":null,"installed_engine":null,"installed_propeller":null,
            "registration_number":"N482TW","serial_number":"1823006","status":"for_sale",
            "avionics":[],"valuation_facts":[]
        })
        .to_string();
        sqlx::query("UPDATE plugin_submissions SET rendered_html = ?, rendered_html_sha256 = ?, extracted_listing_json = ?, extraction_error = NULL WHERE id = ?")
            .bind(rendered_html).bind(&rendered_sha).bind(&raw_checkpoint).bind(submission_id)
            .execute(pool).await.unwrap();
        sqlx::query(
            "UPDATE aircraft_sale_listings SET ingestion_state = 'quarantined', ingestion_error = ?, ingestion_completed_at = NULL, is_verified = 0 WHERE id = ?",
        )
        .bind(SOURCE_IDENTITY_RECEIPT_PENDING)
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();

        let admission = admit_aircraft_source_identity(
            &db,
            Some("N482TW"),
            Some("1823006"),
            Some("Registration N482TW, serial 1823006"),
        )
        .await
        .unwrap();
        let correction = admission.serial_correction.as_ref().unwrap();
        assert_eq!(admission.effective_serial_number(), Some("18283006"));
        let outcome = record_bound_source_serial_correction(
            &db,
            user_id,
            listing_id,
            submission_id,
            correction,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, AircraftRepairOutcome::Applied { .. }));

        let stored: (String, String, String, String, i64) = sqlx::query_as(
            "SELECT submission.extracted_listing_json, listing.serial_number, decision.prior_serial_number, decision.corrected_serial_number, (SELECT COUNT(*) FROM gemini_api_usage) FROM plugin_submissions submission JOIN aircraft_sale_listings listing ON listing.id = submission.canonical_listing_id JOIN aircraft_listing_identity_correction_decisions decision ON decision.plugin_submission_id = submission.id AND decision.correction_kind = 'faa_serial' WHERE submission.id = ?",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored.0, raw_checkpoint);
        assert_eq!(stored.1, "18283006");
        assert_eq!(stored.2, "1823006");
        assert_eq!(stored.3, "18283006");
        assert_eq!(stored.4, 0);

        let observation: (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
        ) = sqlx::query_as(
            "SELECT observed_make, observed_family, observed_designation, model_year FROM aircraft_identity_observations WHERE id = (SELECT observation_id FROM aircraft_listing_identity_correction_decisions WHERE plugin_submission_id = ? AND correction_kind = 'faa_serial')",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(observation, (None, None, None, None));

        let claim: (String, String, String, String, String) = sqlx::query_as(
            "SELECT claim.subject_text, claim.predicate_text, claim.object_text, claim.quoted_evidence, source.source_tier FROM curation_evidence_claims claim JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id WHERE claim.id = (SELECT evidence_claim_id FROM aircraft_listing_identity_correction_decisions WHERE plugin_submission_id = ? AND correction_kind = 'faa_serial')",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(claim.0, "N482TW");
        assert_eq!(claim.1, "FAA assigns exact manufacturer serial");
        assert_eq!(claim.2, "18283006");
        assert_eq!(
            claim.3,
            "FAA MASTER registration N482TW manufacturer serial 18283006"
        );
        assert!(!claim.3.to_ascii_lowercase().contains("publisher"));
        assert_eq!(claim.4, "regulator_primary");

        let correction_observation_id: i64 = sqlx::query_scalar(
            "SELECT observation_id FROM aircraft_listing_identity_correction_decisions WHERE plugin_submission_id = ? AND correction_kind = 'faa_serial'",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(sqlx::query(
            "UPDATE aircraft_identity_observations SET source_url = 'https://example.test/mutated' WHERE id = ?",
        )
        .bind(correction_observation_id)
        .execute(pool)
        .await
        .is_err());
        assert!(
            sqlx::query("DELETE FROM aircraft_identity_observations WHERE id = ?")
                .bind(correction_observation_id)
                .execute(pool)
                .await
                .is_err()
        );

        let unrelated_observation_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_identity_observations (source_url, exact_source_evidence, observation_sha256) VALUES ('https://example.test/unrelated', 'unrelated detached observation', ?) RETURNING id",
        )
        .bind("7".repeat(64))
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE aircraft_identity_observations SET source_url = 'https://example.test/reattached' WHERE id = ?",
        )
        .bind(unrelated_observation_id)
        .execute(pool)
        .await
        .expect("an observation outside correction history must remain mutable");
        assert_eq!(
            sqlx::query("DELETE FROM aircraft_identity_observations WHERE id = ?")
                .bind(unrelated_observation_id)
                .execute(pool)
                .await
                .expect("an unrelated detached observation must remain deletable")
                .rows_affected(),
            1
        );
    }
}
