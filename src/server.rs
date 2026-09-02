use anyhow::{Context, Result};
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio_stream::{wrappers::UnboundedReceiverStream, StreamExt};
use tower_http::cors::CorsLayer;

use crate::aircraft::repair::{
    correct_serial_from_current_faa, corroborate_publisher_hierarchy,
    recover_aircraft_from_visual_asset, AircraftRepairError, AircraftRepairOutcome,
    FaaSerialAircraftRepairRequest, PublisherAircraftRepairRequest, VisualAircraftRepairRequest,
};
use crate::aircraft::{
    aircraft_listing_value_with_model, aircraft_options, aircraft_variant_detail_with_model,
    AircraftStoreError,
};
use crate::aircraft::{faa::drs::DrsClient, verification::AircraftVerificationServices};
use crate::avionics::catalog::{
    attest_grounded_existing_avionics_identity, attest_pending_review_product_identity,
    exact_product_identity_signal_is_present, resolve_avionics_identity_for_review_preflight,
    verify_approved_avionics_product_source_without_gemini, ApprovedAvionicsIdentity,
    ApprovedAvionicsProductSourceRequest, ApprovedProductSourceVerificationOutcome,
    AvionicsIdentityOutcome, AvionicsIdentityRequest, CatalogError, ReviewDirectSourceVerification,
    ReviewPreflightAvionicsIdentityOutcome,
};
use crate::avionics::consolidation::{
    consolidate_avionics_models_with_human_review,
    preview_human_reviewed_avionics_model_consolidation, ConsolidationError,
    HumanReviewedAvionicsConsolidationRequest, HumanReviewedConsolidationProvenance,
};
use crate::avionics::deletion::{delete_avionics_product, AvionicsProductDeletionError};
use crate::avionics::fingerprint::active_collision_closure_revision_sha256;
use crate::avionics::inspection::{
    avionics_catalog_options, get_avionics_catalog_detail, list_avionics_catalog,
    AvionicsCatalogQuery, AvionicsInspectionError,
};
use crate::db::AppDb;
use crate::extract::{preview_listing_url, preview_manual_listing, GeminiListingExtractor};
use crate::gemini::config::GeminiRuntimeConfig;
use crate::gemini::curation::workflow::MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS;
use crate::gemini::interactions::GeminiInteractionsClient;
use crate::gemini::source::ProductIdentityTarget;
use crate::gemini::usage::Store as GeminiUsageStore;
use crate::listing::review::replacement::{
    approve_replacement_products_and_restage, ApproveReplacementProductsRequest,
};
use crate::listing::review::{
    approve_locally_verified_ordinary_aspect_and_restage,
    corroborate_existing_product_association_and_restage, discard_raw_avionics_aspect_and_restage,
    evaluate_existing_product_association, get_listing_review, list_listing_reviews,
    list_pending_product_associations, list_pending_product_reviews,
    preflight_listing_review_resolution, preflight_pending_product_attestation,
    prepare_pending_product_reviews, rebuild_pending_avionics_review_if_current,
    resolve_listing_review, resolved_review_response, restage_unattested_preserved_products,
    revise_avionics_observation_and_restage, use_existing_product_for_aspect_and_restage,
    ExistingProductAssociationCommit, ExistingProductAssociationEvaluation, ListingReview,
    ListingReviewDetail, ListingReviewQueue, PendingProductAssociationPage,
    PendingProductReviewPage, ProductReviewPageQuery, RebuildPendingAvionicsReview,
    RebuildPendingAvionicsReviewBlockReason, ResolveReviewRequest, ResolveReviewResponse,
    ReviewAspectId, ReviewDecision, ReviewError, ReviewQueueQuery,
    ReviseAvionicsObservationRequest, StagedPendingReview,
};
use crate::listing::run::{
    cancel_verification_run, claim_next_verification_run_item, complete_verification_run_item,
    create_verification_run, fail_verification_run_item, get_verification_run,
    list_verification_run_items, CreateVerificationRunRequest, VerificationRun,
    VerificationRunError, VerificationRunItem, VerificationRunItemsQuery,
};
use crate::listing::verification::{
    preflight_reviewer_listing_verifications, verify_listing, ListingVerificationError,
    ListingVerificationMode, ListingVerificationServices, ReviewerListingPreflightScope,
    REVIEWER_PREFLIGHT_DEFAULT_LIMIT,
};
use crate::listings::{
    create_listing, delete_listing, ensure_listing_canonical_aircraft_identity,
    finalize_reviewed_listing_ingestion, get_listing, list_listings, update_listing,
    ListingStoreError,
};
use crate::models::{
    ListingPreview, ListingUpdateRequest, PluginRegisterRequest, PluginSubmissionRequest,
    PreviewRequest, User,
};
use crate::normalize::{
    normalize_avionics_identifier, normalize_avionics_manufacturer_name,
    normalize_avionics_model_name,
};
use crate::plugin::{
    plugin_url_status, register_plugin_install, reprocess_plugin_submission, submit_plugin_html,
    submit_plugin_html_with_progress, PluginStoreError, PluginSubmissionOutcome,
};
use crate::valuation::store::{load_serving_valuation, ServingValuationStatus};
use crate::valuation::ValuationModel;

pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub database_url: String,
}

#[derive(Clone)]
struct AppState {
    db: AppDb,
    extractor: Option<GeminiListingExtractor>,
    automatic_aircraft_gemini: Option<GeminiInteractionsClient>,
    automatic_aircraft_drs: Option<DrsClient>,
    automatic_runtime_config: Option<GeminiRuntimeConfig>,
    verification_run_wake: Arc<Notify>,
    valuation_model: Option<Arc<dyn ValuationModel>>,
    valuation_status: ServingValuationStatus,
}

const VERIFICATION_RUN_ITEM_DEFAULT_LIMIT: i64 = 100;
const VERIFICATION_RUN_ITEM_MAX_LIMIT: i64 = 100;
const VERIFICATION_RUN_LEASE_DURATION: Duration = Duration::from_secs(30 * 60);
const VERIFICATION_RUN_IDLE_POLL: Duration = Duration::from_secs(10);
static VERIFICATION_RUN_LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize)]
struct PluginSubmissionStatusQuery {
    source_url: String,
}

#[derive(Debug, Deserialize)]
struct HumanAvionicsConsolidationApiRequest {
    #[serde(rename = "review_payload_sha256")]
    expected_review_payload_sha256: String,
    #[serde(rename = "catalog_revision_sha256")]
    expected_catalog_revision_sha256: String,
    aspect_id: crate::listing::review::ReviewAspectId,
    survivor_id: i64,
    duplicate_ids: Vec<i64>,
    authoritative_source_url: String,
    authoritative_source_title: String,
    exact_evidence_text: String,
    #[serde(default)]
    apply: bool,
}

fn proposed_identity_matches_consolidation_members<'a>(
    proposed_manufacturer: &str,
    proposed_model: &str,
    members: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> bool {
    let proposed_manufacturer_key = normalize_avionics_manufacturer_name(proposed_manufacturer);
    let proposed_model_key = normalize_avionics_model_name(proposed_model);
    let mut manufacturer_matches = false;
    let mut model_matches = false;
    for (manufacturer, model_key) in members {
        manufacturer_matches |=
            normalize_avionics_manufacturer_name(manufacturer) == proposed_manufacturer_key;
        model_matches |= model_key == proposed_model_key;
    }
    manufacturer_matches && model_matches
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyExistingReviewAvionicsRequest {
    review_payload_sha256: String,
    catalog_revision_sha256: String,
    aspect_id: ReviewAspectId,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateVerificationRunHttpRequest {
    listing_ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationRunItemsHttpQuery {
    limit: Option<i64>,
    after_item_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewerListingPreflightQuery {
    limit: Option<i64>,
    after_listing_id: Option<i64>,
    listing_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttestReviewAvionicsProductRequest {
    listing_id: i64,
    review_payload_sha256: String,
    aspect_id: ReviewAspectId,
    catalog_revision_sha256: String,
    identity_source_url: String,
    identity_source_title: String,
    identity_evidence_text: String,
}

#[derive(Debug, Deserialize)]
struct UseExistingReviewAvionicsRequest {
    #[serde(rename = "review_payload_sha256")]
    expected_review_payload_sha256: String,
    #[serde(rename = "catalog_revision_sha256")]
    expected_catalog_revision_sha256: String,
    aspect_id: ReviewAspectId,
    avionics_model_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscardReviewAvionicsRequest {
    #[serde(rename = "review_payload_sha256")]
    expected_review_payload_sha256: String,
    aspect_id: ReviewAspectId,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RebuildPendingAvionicsReviewRequest {
    review_payload_sha256: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum RebuildPendingAvionicsReviewResponse {
    Rebuilt {
        listing_id: i64,
        review: Option<Box<ListingReview>>,
        review_complete: bool,
    },
    Blocked {
        listing_id: i64,
        reason_code: RebuildPendingAvionicsReviewBlockReason,
        message: &'static str,
        review_complete: bool,
    },
}

pub async fn run_server(config: ServerConfig) -> Result<()> {
    let db = AppDb::connect(&config.database_url).await?;
    let serving_valuation = load_serving_valuation(&db).await?;
    for warning in &serving_valuation.status.warnings {
        eprintln!("valuation warning: {warning}");
    }
    let extractor = GeminiListingExtractor::from_environment_with_usage(&db).ok();
    let automatic_runtime_config = GeminiRuntimeConfig::from_environment().ok();
    let automatic_aircraft_gemini = std::env::var("GEMINI_API_KEY")
        .ok()
        .and_then(|api_key| GeminiInteractionsClient::new(api_key).ok())
        .map(|client| client.with_usage_store(GeminiUsageStore::new(&db)));
    let automatic_aircraft_drs = std::env::var("FAA_DRS_API_KEY")
        .ok()
        .and_then(|api_key| DrsClient::new(api_key).ok());
    let state = AppState {
        db,
        extractor,
        automatic_aircraft_gemini,
        automatic_aircraft_drs,
        automatic_runtime_config,
        verification_run_wake: Arc::new(Notify::new()),
        valuation_model: serving_valuation.model,
        valuation_status: serving_valuation.status,
    };
    let app = router(state.clone());
    let address = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("could not bind {address}"))?;
    start_verification_run_worker(state);

    println!("Serving aircost web app on http://{address}");
    axum::serve(listener, app)
        .await
        .context("aircost web server failed")
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.css", get(stylesheet))
        .route("/app.js", get(javascript))
        .route("/avionics.js", get(avionics_javascript))
        .route("/review.js", get(review_javascript))
        .route("/review/domain.mjs", get(review_domain_javascript))
        .route("/review/automation.mjs", get(review_automation_javascript))
        .route("/health", get(health))
        .route("/api/valuation/status", get(valuation_status_handler))
        .route("/api/users/current", get(current_user_handler))
        .route("/api/plugin/register", post(register_plugin_handler))
        .route("/api/plugin/submissions", post(plugin_submission_handler))
        .route(
            "/api/plugin/submissions/status",
            get(plugin_submission_status_handler),
        )
        .route(
            "/api/plugin/submissions/stream",
            post(plugin_submission_stream_handler),
        )
        .route(
            "/api/plugin/submissions/{id}/reprocess",
            post(reprocess_plugin_submission_handler),
        )
        .route(
            "/api/listings",
            get(list_listings_handler).post(create_listing_handler),
        )
        .route("/api/listings/preview", post(preview_listing_handler))
        .route("/api/aircraft/options", get(aircraft_options_handler))
        .route("/api/avionics", get(list_avionics_handler))
        .route("/api/avionics/options", get(avionics_options_handler))
        .route(
            "/api/avionics/{id}",
            get(avionics_detail_handler).delete(delete_avionics_handler),
        )
        .route("/api/review/listings", get(list_listing_reviews_handler))
        .route(
            "/api/review/verification/preflight",
            get(reviewer_listing_preflight_handler),
        )
        .route(
            "/api/review/verification-runs",
            post(create_verification_run_handler),
        )
        .route(
            "/api/review/verification-runs/{run_id}",
            get(get_verification_run_handler),
        )
        .route(
            "/api/review/verification-runs/{run_id}/items",
            get(list_verification_run_items_handler),
        )
        .route(
            "/api/review/verification-runs/{run_id}/cancel",
            post(cancel_verification_run_handler),
        )
        .route(
            "/api/review/avionics/products",
            get(list_pending_product_reviews_handler),
        )
        .route(
            "/api/review/avionics/products/prepare",
            post(prepare_pending_product_reviews_handler),
        )
        .route(
            "/api/review/avionics/products/{id}/associations",
            get(list_pending_product_associations_handler),
        )
        .route(
            "/api/review/avionics/products/{id}/attest",
            post(attest_review_avionics_product_handler),
        )
        .route("/api/review/listings/{id}", get(get_listing_review_handler))
        .route(
            "/api/review/listings/{id}/restage",
            post(restage_listing_review_handler),
        )
        .route(
            "/api/review/listings/{id}/avionics/rebuild",
            post(rebuild_listing_avionics_review_handler),
        )
        .route(
            "/api/review/listings/{id}/aircraft/visual-recovery",
            post(visual_aircraft_repair_handler),
        )
        .route(
            "/api/review/listings/{id}/aircraft/faa-serial",
            post(faa_serial_aircraft_repair_handler),
        )
        .route(
            "/api/review/listings/{id}/aircraft/publisher-hierarchy",
            post(publisher_aircraft_repair_handler),
        )
        .route(
            "/api/review/listings/{id}/avionics/verify-existing",
            post(verify_existing_review_avionics_handler),
        )
        .route(
            "/api/review/listings/{id}/avionics/use-existing",
            post(use_existing_review_avionics_handler),
        )
        .route(
            "/api/review/listings/{id}/avionics/discard",
            post(discard_review_avionics_handler),
        )
        .route(
            "/api/review/listings/{id}/avionics/revise",
            post(revise_review_avionics_handler),
        )
        .route(
            "/api/review/listings/{id}/avionics/approve-replacement",
            post(approve_replacement_products_handler),
        )
        .route(
            "/api/review/listings/{id}/resolve",
            post(resolve_listing_review_handler),
        )
        .route(
            "/api/review/listings/{id}/avionics/consolidate",
            post(consolidate_review_avionics_handler),
        )
        .route(
            "/api/aircraft/variants/{id}",
            get(aircraft_variant_detail_handler),
        )
        .route(
            "/api/listings/{id}",
            get(get_listing_handler)
                .patch(update_listing_handler)
                .delete(delete_listing_handler),
        )
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn stylesheet() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS)
}

async fn javascript() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
}

async fn avionics_javascript() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        AVIONICS_JS,
    )
}

async fn review_javascript() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        REVIEW_JS,
    )
}

async fn review_domain_javascript() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        REVIEW_DOMAIN_JS,
    )
}

async fn review_automation_javascript() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        REVIEW_AUTOMATION_JS,
    )
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"ok": true, "valuation": state.valuation_status}))
}

async fn valuation_status_handler(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"valuation": state.valuation_status}))
}

async fn current_user_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    Ok(Json(json!({"user": user})))
}

async fn list_listings_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let user_for_response = user.clone();
    let listings = list_listings(&state.db, user.id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(
        json!({"current_user": user_for_response, "listings": listings}),
    ))
}

async fn preview_listing_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let preview = preview_listing_payload(payload, &state).await?;
    Ok(Json(json!({"current_user": user, "preview": preview})))
}

async fn register_plugin_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<PluginRegisterRequest>,
) -> Result<Response, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let plugin_install = register_plugin_install(&state.db, &user, &payload.public_key_base64)
        .await
        .map_err(ApiError::from)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"current_user": user, "plugin_install": plugin_install})),
    )
        .into_response())
}

async fn plugin_submission_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<PluginSubmissionRequest>,
) -> Result<Response, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let outcome = submit_plugin_html(&state.db, &user, &payload, state.extractor.as_ref())
        .await
        .map_err(ApiError::from)?;
    let response =
        plugin_submission_response(&state.db, state.valuation_model.as_ref(), user, outcome).await;
    Ok((StatusCode::CREATED, Json(response)).into_response())
}

async fn plugin_submission_status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PluginSubmissionStatusQuery>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let user_for_response = user.clone();
    let status = plugin_url_status(&state.db, &user, &query.source_url)
        .await
        .map_err(ApiError::from)?;
    let listing = match status.listing_id {
        Some(listing_id) => get_listing(&state.db, user.id, listing_id).await.ok(),
        None => None,
    };
    let listing_estimate = match listing.as_ref() {
        Some(listing) => aircraft_listing_value_with_model(
            &state.db,
            user.id,
            listing.id,
            state.valuation_model.as_ref(),
        )
        .await
        .ok(),
        None => None,
    };
    Ok(Json(json!({
        "current_user": user_for_response,
        "submitted": status.submitted,
        "submission": status.submission,
        "listing": listing,
        "listing_estimate": listing_estimate,
    })))
}

async fn plugin_submission_stream_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<PluginSubmissionRequest>,
) -> Result<Response, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let progress_receiver = start_plugin_submission_job(state, user, payload);

    let stream = UnboundedReceiverStream::new(progress_receiver).map(|event| {
        let line = match serde_json::to_string(&event) {
            Ok(serialized) => format!("{serialized}\n"),
            Err(error) => format!(
                "{}\n",
                json!({
                    "stage": "error",
                    "status": "error",
                    "message": format!("could not serialize progress event: {error}"),
                })
            ),
        };
        Ok::<Bytes, Infallible>(Bytes::from(line))
    });

    Ok(Response::builder()
        .status(StatusCode::CREATED)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(stream))
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?)
}

/// Starts server-owned processing after Axum has received and authenticated the
/// complete upload. The returned receiver only observes progress: dropping it
/// (for example, when the extension popup closes) does not own or cancel the
/// spawned job.
fn start_plugin_submission_job(
    state: AppState,
    user: User,
    payload: PluginSubmissionRequest,
) -> tokio::sync::mpsc::UnboundedReceiver<Value> {
    let (progress_sender, progress_receiver) = tokio::sync::mpsc::unbounded_channel::<Value>();

    tokio::spawn(async move {
        run_plugin_submission_job(state, user, payload, progress_sender).await;
    });

    progress_receiver
}

async fn run_plugin_submission_job(
    state: AppState,
    user: User,
    payload: PluginSubmissionRequest,
    progress_sender: tokio::sync::mpsc::UnboundedSender<Value>,
) {
    let _ = progress_sender.send(json!({
        "stage": "received_upload",
        "status": "running",
        "message": "Received upload",
    }));
    let result = submit_plugin_html_with_progress(
        &state.db,
        &user,
        &payload,
        state.extractor.as_ref(),
        Some(&progress_sender),
    )
    .await;
    match result {
        Ok(outcome) => {
            let mut response = plugin_submission_response(
                &state.db,
                state.valuation_model.as_ref(),
                user,
                outcome,
            )
            .await;
            if let Some(object) = response.as_object_mut() {
                object.insert("stage".to_string(), json!("complete"));
                object.insert("status".to_string(), json!("complete"));
                object.insert("message".to_string(), json!("Upload complete"));
            }
            let _ = progress_sender.send(response);
        }
        Err(error) => {
            let _ = progress_sender.send(json!({
                "stage": "error",
                "status": "error",
                "message": error.to_string(),
            }));
        }
    }
}

async fn reprocess_plugin_submission_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(submission_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let outcome =
        reprocess_plugin_submission(&state.db, &user, submission_id, state.extractor.as_ref())
            .await
            .map_err(ApiError::from)?;
    Ok(Json(
        plugin_submission_response(&state.db, state.valuation_model.as_ref(), user, outcome).await,
    ))
}

async fn create_listing_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Response, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let preview = preview_listing_payload(payload.clone(), &state).await?;
    let original_listing = payload.get("listing").cloned();
    let user_for_response = user.clone();
    let listing = create_listing(
        &state.db,
        user.id,
        &preview,
        original_listing.as_ref(),
        state.extractor.as_ref(),
    )
    .await
    .map_err(ApiError::from)?;
    let listing_estimate = aircraft_listing_value_with_model(
        &state.db,
        user_for_response.id,
        listing.id,
        state.valuation_model.as_ref(),
    )
    .await
    .ok();

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "current_user": user_for_response,
            "listing": listing,
            "listing_estimate": listing_estimate
        })),
    )
        .into_response())
}

async fn get_listing_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let user_for_response = user.clone();
    let listing = get_listing(&state.db, user.id, listing_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(
        json!({"current_user": user_for_response, "listing": listing}),
    ))
}

async fn update_listing_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<ListingUpdateRequest>,
) -> Result<Json<Value>, ApiError> {
    if !payload.listing.is_object() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "listing must be a JSON object",
        ));
    }
    let user = load_current_user(&state.db, &headers).await?;
    let user_for_response = user.clone();
    let listing = update_listing(
        &state.db,
        user.id,
        listing_id,
        &payload.listing,
        state.extractor.as_ref(),
    )
    .await
    .map_err(ApiError::from)?;
    let listing_estimate = aircraft_listing_value_with_model(
        &state.db,
        user_for_response.id,
        listing.id,
        state.valuation_model.as_ref(),
    )
    .await
    .ok();
    Ok(Json(json!({
        "current_user": user_for_response,
        "listing": listing,
        "listing_estimate": listing_estimate
    })))
}

async fn delete_listing_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    delete_listing(&state.db, user.id, listing_id)
        .await
        .map_err(ApiError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn aircraft_options_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let options = aircraft_options(&state.db, user.id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(json!({"current_user": user, "options": options})))
}

async fn list_avionics_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AvionicsCatalogQuery>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let catalog = list_avionics_catalog(&state.db, user.id, query).await?;
    Ok(Json(json!({"current_user": user, "catalog": catalog})))
}

async fn avionics_options_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let options = avionics_catalog_options(&state.db, user.id).await?;
    Ok(Json(json!({"current_user": user, "options": options})))
}

async fn avionics_detail_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(avionics_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let avionics = get_avionics_catalog_detail(&state.db, user.id, avionics_id).await?;
    Ok(Json(json!({"current_user": user, "avionics": avionics})))
}

async fn delete_avionics_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(avionics_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let deletion = delete_avionics_product(&state.db, user.id, avionics_id).await?;
    Ok(Json(json!({
        "current_user": user,
        "deleted_product_id": deletion.deleted_product_id,
        "deleted_product_name": deletion.deleted_product_name,
        "affected_listing_count": deletion.affected_listing_count,
        "affected_listing_ids": deletion.affected_listing_ids,
        "deleted_listing_association_count": deletion.deleted_listing_association_count,
        "discarded_occurrence_count": deletion.discarded_occurrence_count,
        "removed_pending_aspect_count": deletion.removed_pending_aspect_count,
        "deleted_suite_membership_count": deletion.deleted_suite_membership_count,
    })))
}

async fn list_listing_reviews_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ReviewQueueQuery>,
) -> Result<Json<ListingReviewQueue>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let queue = list_listing_reviews(&state.db, user.id, query).await?;
    Ok(Json(queue))
}

async fn list_pending_product_reviews_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ProductReviewPageQuery>,
) -> Result<Json<PendingProductReviewPage>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    Ok(Json(
        list_pending_product_reviews(&state.db, user.id, query).await?,
    ))
}

async fn prepare_pending_product_reviews_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    Ok(Json(json!(
        prepare_pending_product_reviews(&state.db, user.id).await?
    )))
}

async fn list_pending_product_associations_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(product_id): Path<i64>,
    Query(query): Query<ProductReviewPageQuery>,
) -> Result<Json<PendingProductAssociationPage>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    Ok(Json(
        list_pending_product_associations(&state.db, user.id, product_id, query).await?,
    ))
}

async fn get_listing_review_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
) -> Result<Json<ListingReviewDetail>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let review = get_listing_review(&state.db, user.id, listing_id).await?;
    Ok(Json(review))
}

/// Re-attest one global, graph-approved avionics product from one freshly
/// fetched OEM document. The endpoint never resolves a listing association and
/// never calls Gemini. A current attestation is an idempotent zero-fetch
/// success.
async fn attest_review_avionics_product_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(product_id): Path<i64>,
    Json(payload): Json<AttestReviewAvionicsProductRequest>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let target = preflight_pending_product_attestation(
        &state.db,
        user.id,
        product_id,
        payload.listing_id,
        &payload.review_payload_sha256,
        &payload.aspect_id,
        &payload.catalog_revision_sha256,
        &payload.identity_source_url,
        &payload.identity_source_title,
        &payload.identity_evidence_text,
    )
    .await?;
    if target.already_reuse_attested {
        let review = get_listing_review(&state.db, user.id, payload.listing_id)
            .await?
            .review;
        return Ok(Json(json!({
            "product_id": product_id,
            "attestation_status": "current",
            "reused": true,
            "review": review
        })));
    }

    let stable_identifier = target.product.stable_identifier.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "approved catalog id {product_id} has no stable manufacturer identifier and must be curated before reuse"
            ),
        )
        .with_code("avionics_identity_verification_failed")
    })?;
    let request = ApprovedAvionicsProductSourceRequest {
        source_url: payload.identity_source_url.clone(),
        manufacturer: target.product.manufacturer,
        model: target.product.model,
        avionics_types: target.product.capabilities,
        manufacturer_identifier_kind: stable_identifier.kind.clone(),
        manufacturer_identifier: stable_identifier.value.clone(),
    };
    let source_target =
        ProductIdentityTarget::new(&request.model, &request.manufacturer_identifier)
            .map_err(|error| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    format!(
                        "approved catalog id {product_id} has an invalid deterministic source target: {error}"
                    ),
                )
                .with_code("avionics_identity_verification_failed")
            })?;
    let extractor = state.extractor.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "guarded publisher-source fetching is unavailable",
        )
        .with_code("avionics_grounding_unavailable")
    })?;
    let fetched = extractor
        .fetch_public_same_origin_product_document(&payload.identity_source_url, source_target)
        .await
        .map_err(|error| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("could not fetch authoritative avionics source: {error}"),
            )
            .with_code("avionics_source_fetch_failed")
        })?;
    let outcome = verify_approved_avionics_product_source_without_gemini(
        &state.db,
        &request,
        product_id,
        &payload.identity_source_title,
        &fetched,
    )
    .await
    .map_err(|error| {
        let status = match &error {
            CatalogError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
            CatalogError::Validation(_) | CatalogError::Gemini(_) => StatusCode::CONFLICT,
        };
        ApiError::new(
            status,
            format!("could not deterministically verify existing avionics identity: {error}"),
        )
        .with_code("avionics_identity_verification_failed")
    })?;
    let verification = match outcome {
        ApprovedProductSourceVerificationOutcome::Verified(verification)
            if verification.approved.id == product_id =>
        {
            verification
        }
        ApprovedProductSourceVerificationOutcome::Verified(verification) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!(
                    "source verification matched catalog id {} instead of hash-bound catalog id {product_id}",
                    verification.approved.id
                ),
            )
            .with_code("avionics_identity_mismatch"));
        }
        ApprovedProductSourceVerificationOutcome::Unresolved { reason } => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!(
                    "existing avionics identity could not be deterministically re-attested: {reason}"
                ),
            )
            .with_code("avionics_identity_unresolved"));
        }
    };
    let attested =
        attest_pending_review_product_identity(&state.db, &verification, &target.commit_guard)
            .await
            .map_err(|error| {
            ApiError::new(
                StatusCode::CONFLICT,
                format!(
                    "grounded catalog id {product_id} could not receive a current-policy reuse attestation: {error}"
                ),
            )
            .with_code("avionics_reuse_attestation_failed")
            })?;
    if !attested {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "grounded catalog id {product_id} has no active exact manufacturer source origin"
            ),
        )
        .with_code("avionics_reuse_attestation_failed"));
    }
    let review = get_listing_review(&state.db, user.id, payload.listing_id)
        .await?
        .review;
    Ok(Json(json!({
        "product_id": product_id,
        "attestation_status": "current",
        "reused": false,
        "review": review
    })))
}

async fn restage_listing_review_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let staged = restage_unattested_preserved_products(&state.db, user.id, listing_id).await?;
    review_maintenance_response(&state.db, user.id, listing_id, staged).await
}

async fn rebuild_listing_avionics_review_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<RebuildPendingAvionicsReviewRequest>,
) -> Result<Json<RebuildPendingAvionicsReviewResponse>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    match rebuild_pending_avionics_review_if_current(
        &state.db,
        user.id,
        listing_id,
        &payload.review_payload_sha256,
    )
    .await?
    {
        RebuildPendingAvionicsReview::Blocked { reason_code, .. } => {
            Ok(Json(RebuildPendingAvionicsReviewResponse::Blocked {
                listing_id,
                reason_code,
                message: reason_code.message(),
                review_complete: false,
            }))
        }
        RebuildPendingAvionicsReview::Rebuilt { review: Some(_) } => {
            let detail = get_listing_review(&state.db, user.id, listing_id).await?;
            Ok(Json(RebuildPendingAvionicsReviewResponse::Rebuilt {
                listing_id,
                review: Some(Box::new(detail.review)),
                review_complete: false,
            }))
        }
        RebuildPendingAvionicsReview::Rebuilt { review: None } => {
            Ok(Json(RebuildPendingAvionicsReviewResponse::Rebuilt {
                listing_id,
                review: None,
                review_complete: true,
            }))
        }
    }
}

async fn visual_aircraft_repair_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<VisualAircraftRepairRequest>,
) -> Result<Json<AircraftRepairOutcome>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let client = state.automatic_aircraft_gemini.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Aircraft visual recovery is not configured.",
        )
        .with_code("aircraft_visual_recovery_unavailable")
    })?;
    let runtime = state.automatic_runtime_config.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Aircraft visual recovery is not configured.",
        )
        .with_code("aircraft_visual_recovery_unavailable")
    })?;
    recover_aircraft_from_visual_asset(&state.db, user.id, listing_id, &payload, client, runtime)
        .await
        .map(Json)
        .map_err(aircraft_repair_api_error)
}

async fn faa_serial_aircraft_repair_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<FaaSerialAircraftRepairRequest>,
) -> Result<Json<AircraftRepairOutcome>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    correct_serial_from_current_faa(&state.db, user.id, listing_id, &payload)
        .await
        .map(Json)
        .map_err(aircraft_repair_api_error)
}

async fn publisher_aircraft_repair_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<PublisherAircraftRepairRequest>,
) -> Result<Json<AircraftRepairOutcome>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    corroborate_publisher_hierarchy(&state.db, user.id, listing_id, &payload)
        .await
        .map(Json)
        .map_err(aircraft_repair_api_error)
}

fn aircraft_repair_api_error(error: AircraftRepairError) -> ApiError {
    match error {
        AircraftRepairError::NotFound(_) => ApiError::new(
            StatusCode::NOT_FOUND,
            "The listing is no longer available for aircraft repair.",
        )
        .with_code("aircraft_repair_not_found"),
        AircraftRepairError::Permission => {
            ApiError::new(StatusCode::FORBIDDEN, "You cannot repair this listing.")
                .with_code("aircraft_repair_forbidden")
        }
        AircraftRepairError::Stale => ApiError::new(
            StatusCode::CONFLICT,
            "The aircraft or retained source changed. Reload the review before trying again.",
        )
        .with_code("aircraft_repair_stale"),
        AircraftRepairError::Validation(_) => ApiError::new(
            StatusCode::BAD_REQUEST,
            "The requested aircraft correction did not satisfy the evidence and FAA checks.",
        )
        .with_code("aircraft_repair_invalid"),
        AircraftRepairError::Service(message) => {
            eprintln!("aircraft repair service failed: {message}");
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "Aircraft visual recovery could not inspect the selected photo.",
            )
            .with_code("aircraft_repair_service_failed")
        }
        AircraftRepairError::Database(message) => {
            eprintln!("aircraft repair database failed: {message}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The aircraft correction could not be saved.",
            )
            .with_code("aircraft_repair_failed")
        }
    }
}

async fn reviewer_listing_preflight_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ReviewerListingPreflightQuery>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let report = preflight_reviewer_listing_verifications(
        &state.db,
        user.id,
        &ReviewerListingPreflightScope::new(
            query.limit.unwrap_or(REVIEWER_PREFLIGHT_DEFAULT_LIMIT),
            query.listing_id,
            query.after_listing_id,
        ),
    )
    .await
    .map_err(reviewer_preflight_api_error)?;
    Ok(Json(json!({
        "verification": report.verification,
        "listing_contexts": report.listing_contexts,
        "services": {
            "gemini_configured": state.extractor.is_some()
                && state.automatic_aircraft_gemini.is_some()
                && state.automatic_runtime_config.is_some(),
            "faa_drs_configured": state.automatic_aircraft_drs.is_some(),
        }
    })))
}

async fn create_verification_run_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateVerificationRunHttpRequest>,
) -> Result<Response, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let result = create_verification_run(
        &state.db,
        &CreateVerificationRunRequest {
            owner_user_id: user.id,
            idempotency_key,
            listing_ids: payload.listing_ids,
        },
    )
    .await
    .map_err(verification_run_api_error)?;
    if result.created {
        state.verification_run_wake.notify_one();
    }
    let status = if result.created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    let location = format!("/api/review/verification-runs/{}", result.run.id);
    Ok((
        status,
        [(header::LOCATION, location)],
        Json(json!({ "run": verification_run_json(&result.run) })),
    )
        .into_response())
}

async fn get_verification_run_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let run = get_verification_run(&state.db, user.id, run_id)
        .await
        .map_err(verification_run_api_error)?;
    Ok(Json(json!({ "run": verification_run_json(&run) })))
}

async fn list_verification_run_items_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<i64>,
    Query(query): Query<VerificationRunItemsHttpQuery>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let limit = query.limit.unwrap_or(VERIFICATION_RUN_ITEM_DEFAULT_LIMIT);
    if !(1..=VERIFICATION_RUN_ITEM_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("limit must be between 1 and {VERIFICATION_RUN_ITEM_MAX_LIMIT}"),
        )
        .with_code("verification_run_invalid"));
    }
    let page = list_verification_run_items(
        &state.db,
        user.id,
        run_id,
        &VerificationRunItemsQuery {
            limit: Some(limit),
            after_item_id: query.after_item_id,
        },
    )
    .await
    .map_err(verification_run_api_error)?;
    Ok(Json(json!({
        "items": page.items.iter().map(verification_run_item_json).collect::<Vec<_>>(),
        "checkpoint": {
            "has_more": page.checkpoint.has_more,
            "resume_after_item_id": page.checkpoint.resume_after_item_id,
        }
    })))
}

async fn cancel_verification_run_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let run = cancel_verification_run(&state.db, user.id, run_id)
        .await
        .map_err(verification_run_api_error)?;
    state.verification_run_wake.notify_one();
    Ok(Json(json!({ "run": verification_run_json(&run) })))
}

fn verification_run_json(run: &VerificationRun) -> Value {
    json!({
        "id": run.id,
        "status": run.status,
        "total_items": run.total_items,
        "queued_items": run.queued_items,
        "running_items": run.running_items,
        "verified_items": run.verified_items,
        "pending_review_items": run.pending_review_items,
        "blocked_items": run.blocked_items,
        "failed_items": run.failed_items,
        "cancelled_items": run.cancelled_items,
        "current_listing_id": run.current_listing_id,
    })
}

fn verification_run_item_json(item: &VerificationRunItem) -> Value {
    json!({
        "id": item.id,
        "listing_id": item.listing_id,
        "status": item.status,
        "outcome": item.outcome,
        "reason_code": item.reason_code,
        "reason": item.reason,
    })
}

fn required_idempotency_key(headers: &HeaderMap) -> Result<String, ApiError> {
    let mut values = headers.get_all("idempotency-key").iter();
    let value = values.next().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "Idempotency-Key header is required",
        )
        .with_code("verification_run_invalid")
    })?;
    if values.next().is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "exactly one Idempotency-Key header is required",
        )
        .with_code("verification_run_invalid"));
    }
    let value = value.to_str().map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "Idempotency-Key must be valid text",
        )
        .with_code("verification_run_invalid")
    })?;
    let value = value.trim();
    if value.is_empty() {
        return Err(
            ApiError::new(StatusCode::BAD_REQUEST, "Idempotency-Key must not be empty")
                .with_code("verification_run_invalid"),
        );
    }
    Ok(value.to_string())
}

fn verification_run_api_error(error: VerificationRunError) -> ApiError {
    match error {
        VerificationRunError::Validation(message) => {
            ApiError::new(StatusCode::BAD_REQUEST, message).with_code("verification_run_invalid")
        }
        VerificationRunError::NotFound(message) => {
            ApiError::new(StatusCode::NOT_FOUND, message).with_code("verification_run_not_found")
        }
        VerificationRunError::Conflict(message) => {
            ApiError::new(StatusCode::CONFLICT, message).with_code("verification_run_conflict")
        }
        VerificationRunError::IdempotencyConflict { run_id } => ApiError::new(
            StatusCode::CONFLICT,
            "Idempotency-Key was already used with a different request",
        )
        .with_code("verification_run_idempotency_conflict")
        .with_details(json!({ "active_run_id": run_id })),
        VerificationRunError::ActiveListingConflict { run_id, listing_id } => ApiError::new(
            StatusCode::CONFLICT,
            format!("listing {listing_id} already belongs to an active verification run"),
        )
        .with_code("verification_run_listing_active")
        .with_details(json!({
            "listing_id": listing_id,
            "active_run_id": run_id,
        })),
        VerificationRunError::LeaseConflict { item_id } => ApiError::new(
            StatusCode::CONFLICT,
            format!("verification run item {item_id} lease is no longer current"),
        )
        .with_code("verification_run_lease_conflict"),
        VerificationRunError::Database(message) => {
            eprintln!("verification run database operation failed: {message}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The verification run could not be processed.",
            )
            .with_code("verification_run_failed")
        }
    }
}

fn start_verification_run_worker(state: AppState) {
    state.verification_run_wake.notify_one();
    tokio::spawn(async move {
        verification_run_worker(state).await;
    });
}

async fn verification_run_worker(state: AppState) {
    if let Err(error) =
        crate::listing::run::reclaim_expired_verification_run_leases(&state.db).await
    {
        eprintln!("verification run worker could not reclaim startup leases: {error}");
    }
    loop {
        let lease_token = verification_run_lease_token();
        match claim_next_verification_run_item(
            &state.db,
            &lease_token,
            VERIFICATION_RUN_LEASE_DURATION,
        )
        .await
        {
            Ok(Some(item)) => {
                process_claimed_verification_run_item(&state, item, &lease_token).await;
            }
            Ok(None) => {
                tokio::select! {
                    _ = state.verification_run_wake.notified() => {}
                    _ = tokio::time::sleep(VERIFICATION_RUN_IDLE_POLL) => {}
                }
            }
            Err(error) => {
                eprintln!("verification run worker could not claim work: {error}");
                tokio::select! {
                    _ = state.verification_run_wake.notified() => {}
                    _ = tokio::time::sleep(VERIFICATION_RUN_IDLE_POLL) => {}
                }
            }
        }
    }
}

async fn process_claimed_verification_run_item(
    state: &AppState,
    item: crate::listing::run::ClaimedVerificationRunItem,
    lease_token: &str,
) {
    let aircraft = match (
        state.automatic_aircraft_gemini.as_ref(),
        state.automatic_aircraft_drs.as_ref(),
        state.automatic_runtime_config.as_ref(),
    ) {
        (Some(gemini), Some(drs), Some(config)) => Some(AircraftVerificationServices {
            gemini,
            drs,
            config,
        }),
        _ => None,
    };
    match verify_listing(
        &state.db,
        item.listing_id,
        ListingVerificationMode::Apply,
        ListingVerificationServices {
            extractor: state.extractor.as_ref(),
            aircraft,
        },
    )
    .await
    {
        Ok(outcome) => {
            let persistence = if outcome.status == "failed" {
                fail_verification_run_item(
                    &state.db,
                    item.item_id,
                    lease_token,
                    "automatic_verification_failed",
                    "Automatic verification could not complete this listing.",
                )
                .await
            } else {
                complete_verification_run_item(&state.db, item.item_id, lease_token, &outcome).await
            };
            if let Err(error) = persistence {
                eprintln!(
                    "verification run worker could not persist terminal item {}: {error}",
                    item.item_id
                );
            }
        }
        Err(error) => {
            let (reason_code, reason) = verification_run_failure_reason(&error);
            eprintln!(
                "verification run item {} failed before terminal persistence: {error}",
                item.item_id
            );
            if let Err(store_error) = fail_verification_run_item(
                &state.db,
                item.item_id,
                lease_token,
                reason_code,
                reason,
            )
            .await
            {
                eprintln!(
                    "verification run worker could not fail item {}: {store_error}",
                    item.item_id
                );
            }
        }
    }
}

fn verification_run_failure_reason(
    error: &ListingVerificationError,
) -> (&'static str, &'static str) {
    match error {
        ListingVerificationError::Validation(_) => (
            "automatic_verification_invalid",
            "The listing no longer satisfies the automatic verification input contract.",
        ),
        ListingVerificationError::NotFound(_) => (
            "listing_not_found",
            "The listing no longer exists or is no longer available to this verification run.",
        ),
        ListingVerificationError::Unavailable(_) => (
            "automatic_verification_unavailable",
            "A required automatic verification service is not configured.",
        ),
        ListingVerificationError::Database(_) => (
            "automatic_verification_failed",
            "A database operation failed while verifying this listing.",
        ),
        ListingVerificationError::Aircraft(_) => (
            "aircraft_verification_failed",
            "The FAA-backed aircraft verification step failed.",
        ),
        ListingVerificationError::Avionics(_) => (
            "avionics_verification_failed",
            "The avionics verification step failed.",
        ),
    }
}

fn verification_run_lease_token() -> String {
    let sequence = VERIFICATION_RUN_LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("aircost-web:{}:{timestamp}:{sequence}", std::process::id())
}

fn reviewer_preflight_api_error(error: ListingVerificationError) -> ApiError {
    match error {
        ListingVerificationError::Validation(message) => {
            ApiError::new(StatusCode::BAD_REQUEST, message)
                .with_code("verification_preflight_invalid")
        }
        ListingVerificationError::NotFound(listing_id) => ApiError::new(
            StatusCode::NOT_FOUND,
            format!("listing {listing_id} was not found"),
        )
        .with_code("verification_preflight_not_found"),
        ListingVerificationError::Unavailable(message) => {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, message)
                .with_code("verification_preflight_unavailable")
        }
        ListingVerificationError::Database(message) => {
            eprintln!("verification preflight database operation failed: {message}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The verification preflight could not be completed.",
            )
            .with_code("verification_preflight_failed")
        }
        ListingVerificationError::Aircraft(message)
        | ListingVerificationError::Avionics(message) => {
            eprintln!("verification preflight stage failed: {message}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The verification preflight could not inspect every listing stage.",
            )
            .with_code("verification_preflight_failed")
        }
    }
}

async fn review_maintenance_response(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    staged: Option<StagedPendingReview>,
) -> Result<Json<Value>, ApiError> {
    let review = match staged {
        Some(_) => {
            let detail = get_listing_review(db, owner_user_id, listing_id).await?;
            Some(detail.review)
        }
        None => None,
    };
    let review_complete = review.is_none();
    let finalization_error = if review_complete {
        finalize_reviewed_listing_ingestion(db, listing_id)
            .await
            .err()
            .map(|error| error.to_string())
    } else {
        None
    };
    let listing = get_listing(db, owner_user_id, listing_id).await?;
    let listing_ready = listing.ingestion_state == "ready";
    let listing_verified = listing.is_verified;
    Ok(Json(json!({
        "review": review,
        "review_complete": review_complete,
        "listing": listing,
        "listing_ready": listing_ready,
        "listing_verified": listing_verified,
        "finalization_attempted": review_complete,
        "finalization_error": finalization_error,
    })))
}

/// Apply one ordinary `use_verified_product` decision without resolving,
/// grounding, or finalizing any other review aspect.
async fn use_existing_review_avionics_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<UseExistingReviewAvionicsRequest>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;

    // Reject stale browser/API state early. The store repeats both checks
    // under the catalog/listing/review locks below.
    let detail = get_listing_review(&state.db, user.id, listing_id).await?;
    if payload.expected_review_payload_sha256 != detail.review.review_payload_sha256 {
        return Err(ApiError::from(ReviewError::Stale(
            "review payload is stale; reload the review".to_string(),
        )));
    }
    if payload.expected_catalog_revision_sha256 != detail.review.catalog_revision_sha256 {
        return Err(ApiError::from(ReviewError::Stale(
            "approved avionics catalog changed during review; reload and re-evaluate".to_string(),
        )));
    }
    let staged = use_existing_product_for_aspect_and_restage(
        &state.db,
        user.id,
        listing_id,
        &payload.aspect_id,
        &payload.expected_review_payload_sha256,
        &payload.expected_catalog_revision_sha256,
        payload.avionics_model_id,
    )
    .await?;
    review_maintenance_response(&state.db, user.id, listing_id, staged).await
}

/// Permanently discard one independent raw avionics occurrence without
/// resolving any other aspect or depending on a catalog revision.
async fn discard_review_avionics_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<DiscardReviewAvionicsRequest>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let staged = discard_raw_avionics_aspect_and_restage(
        &state.db,
        user.id,
        listing_id,
        &payload.aspect_id,
        &payload.expected_review_payload_sha256,
        &payload.reason,
    )
    .await?;
    review_maintenance_response(&state.db, user.id, listing_id, staged).await
}

/// Save reviewer-corrected occurrence values into the guarded pending bundle.
/// Product selection, product creation, evidence grounding, and final listing
/// mutation remain exclusively owned by the existing review resolution paths.
async fn revise_review_avionics_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<ReviseAvionicsObservationRequest>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let staged =
        revise_avionics_observation_and_restage(&state.db, user.id, listing_id, &payload).await?;
    review_maintenance_response(&state.db, user.id, listing_id, Some(staged)).await
}

/// Apply both identities of one staged replacement relationship atomically.
///
/// This source-free association path requires both products to be current
/// reusable catalog entries and never invokes Gemini or an OEM fetch.
async fn approve_replacement_products_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<ApproveReplacementProductsRequest>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let staged =
        approve_replacement_products_and_restage(&state.db, user.id, listing_id, &payload).await?;
    review_maintenance_response(&state.db, user.id, listing_id, staged).await
}

/// Verify exactly one hash-bound listing aspect against an already attested
/// product. This route is source-free, local-only, and zero-Gemini.
///
/// Preserved links are corroborated in place. Independent ordinary extraction
/// aspects use the normal aspect-scoped existing-product transaction.
async fn verify_existing_review_avionics_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<VerifyExistingReviewAvionicsRequest>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let evaluation = evaluate_existing_product_association(
        &state.db,
        user.id,
        listing_id,
        &payload.aspect_id,
        &payload.review_payload_sha256,
        &payload.catalog_revision_sha256,
    )
    .await?;
    let target = match evaluation {
        ExistingProductAssociationEvaluation::AutoVerifiable(target) => target,
        ExistingProductAssociationEvaluation::ProductAttestationRequired { eligibility } => {
            return Err(ReviewError::Conflict(eligibility.reason.unwrap_or_else(|| {
                "global product attestation is required before local verification".to_string()
            }))
            .into());
        }
        ExistingProductAssociationEvaluation::ManualReviewRequired { eligibility, error } => {
            match eligibility.reason_code.as_deref() {
                Some("different_product_detected") => {
                    return Err(ApiError::new(StatusCode::CONFLICT, error.to_string())
                        .with_code("avionics_identity_mismatch"));
                }
                Some("catalog_identity_ambiguous") => {
                    return Err(ApiError::new(StatusCode::CONFLICT, error.to_string())
                        .with_code("avionics_association_unresolved"));
                }
                _ => return Err(error.into()),
            }
        }
    };
    let target_id = target
        .product
        .id
        .expect("verification target always comes from an approved catalog row");

    // Capture the complete collision closure immediately before the local
    // association decision that consumes it.
    let expected_collision_closure_sha256 =
        active_collision_closure_revision_sha256(&state.db, target_id)
            .await
            .map_err(ReviewError::from)?;

    let staged = match target.commit {
        ExistingProductAssociationCommit::CorroboratePreserved { observation_sha256 } => {
            corroborate_existing_product_association_and_restage(
                &state.db,
                user.id,
                listing_id,
                &payload.aspect_id,
                &payload.review_payload_sha256,
                &payload.catalog_revision_sha256,
                &expected_collision_closure_sha256,
                target_id,
                &observation_sha256,
                &target.listing_evidence_provenance,
            )
            .await?
        }
        ExistingProductAssociationCommit::ApproveOrdinary => {
            approve_locally_verified_ordinary_aspect_and_restage(
                &state.db,
                user.id,
                listing_id,
                &payload.aspect_id,
                &payload.review_payload_sha256,
                &payload.catalog_revision_sha256,
                &expected_collision_closure_sha256,
                target_id,
                &target.listing_evidence_provenance,
            )
            .await?
        }
    };
    review_maintenance_response(&state.db, user.id, listing_id, staged).await
}

/// Preview or apply an explicit, evidence-backed consolidation of the complete
/// unreviewed model-equivalence set blocking one listing-review aspect.
///
/// This endpoint never calls Gemini. The core function re-snapshots the
/// pending-review digest and every selected catalog row under its write lock
/// before an apply, so the preview is informative rather than authorization.
async fn consolidate_review_avionics_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(payload): Json<HumanAvionicsConsolidationApiRequest>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    let detail = get_listing_review(&state.db, user.id, listing_id).await?;
    if payload.expected_review_payload_sha256 != detail.review.review_payload_sha256 {
        return Err(ApiError::from(ReviewError::Stale(
            "review payload is stale; reload the review".to_string(),
        )));
    }
    if payload.expected_catalog_revision_sha256 != detail.review.catalog_revision_sha256 {
        return Err(ApiError::from(ReviewError::Stale(
            "approved avionics catalog changed during review; reload and re-evaluate".to_string(),
        )));
    }

    let aspect = detail
        .review
        .aspects
        .iter()
        .find(|aspect| aspect.id == payload.aspect_id)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("unknown review aspect {}", payload.aspect_id),
            )
            .with_code("review_consolidation_invalid")
        })?;
    if aspect.kind != "avionics" {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "review aspect {} is not an avionics identity aspect",
                payload.aspect_id
            ),
        )
        .with_code("review_consolidation_invalid"));
    }
    let proposed = aspect.proposed_product.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "review aspect {} has no proposed avionics identity to consolidate",
                payload.aspect_id
            ),
        )
        .with_code("review_consolidation_invalid")
    })?;
    if proposed
        .id
        .is_some_and(|id| id != payload.survivor_id && !payload.duplicate_ids.contains(&id))
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "review aspect {} points to catalog id {:?}, outside the requested consolidation set",
                payload.aspect_id, proposed.id
            ),
        )
        .with_code("review_consolidation_mismatch"));
    }

    let request = HumanReviewedAvionicsConsolidationRequest {
        survivor_id: payload.survivor_id,
        duplicate_ids: payload.duplicate_ids,
        reviewer_user_id: user.id,
        authoritative_source_url: payload.authoritative_source_url,
        authoritative_source_title: payload.authoritative_source_title,
        exact_evidence_text: payload.exact_evidence_text,
        provenance: Some(HumanReviewedConsolidationProvenance {
            listing_id,
            review_aspect_id: payload.aspect_id.to_string(),
            expected_review_payload_sha256: payload.expected_review_payload_sha256,
        }),
        expected_authorization_sha256: None,
        expected_catalog_revision_sha256: Some(payload.expected_catalog_revision_sha256.clone()),
    };
    let preview = preview_human_reviewed_avionics_model_consolidation(&state.db, &request).await?;
    if !proposed_identity_matches_consolidation_members(
        &proposed.manufacturer,
        &proposed.model,
        preview.authorization.members.iter().map(|member| {
            (
                member.manufacturer.as_str(),
                member.canonical_model_key.as_str(),
            )
        }),
    ) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "requested catalog rows do not match the proposed identity {} {} for review aspect {}",
                proposed.manufacturer, proposed.model, payload.aspect_id
            ),
        )
        .with_code("review_consolidation_mismatch"));
    }
    if let Some(staged_identifier) = &proposed.stable_identifier {
        let staged_kind = staged_identifier.kind.trim();
        let staged_value = normalize_avionics_identifier(&staged_identifier.value);
        let has_conflicting_member_identifier =
            preview.authorization.members.iter().any(|member| {
                member
                    .manufacturer_identifier_kind
                    .as_deref()
                    .zip(member.normalized_manufacturer_identifier.as_deref())
                    .is_some_and(|(kind, value)| {
                        kind.trim() != staged_kind
                            || normalize_avionics_identifier(value) != staged_value
                    })
            });
        if has_conflicting_member_identifier {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!(
                    "requested catalog rows conflict with the staged stable identifier for review aspect {}",
                    payload.aspect_id
                ),
            )
            .with_code("review_consolidation_mismatch"));
        }
    }

    if !payload.apply {
        return Ok(Json(json!({
            "applied": false,
            "report": preview,
            "review": detail.review,
        })));
    }

    let mut apply_request = request;
    apply_request.expected_authorization_sha256 =
        Some(preview.authorization.authorization_sha256.clone());
    let report = consolidate_avionics_models_with_human_review(&state.db, &apply_request).await?;
    let refreshed = get_listing_review(&state.db, user.id, listing_id).await?;
    Ok(Json(json!({
        "applied": true,
        "report": report,
        "review": refreshed.review,
    })))
}

async fn resolve_listing_review_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing_id): Path<i64>,
    Json(mut payload): Json<ResolveReviewRequest>,
) -> Result<Json<ResolveReviewResponse>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    require_listing_reviewer(&user)?;
    // Enforce ownership before preparing FAA identity, then establish and
    // strictly revalidate the immutable canonical assignment before any
    // Gemini or avionics-catalog writes. This also repairs legacy pending
    // reviews that predate aircraft identity assignment. When explicitly
    // requested below, post-commit finalization repeats the check to close the
    // publication race.
    let review = get_listing_review(&state.db, user.id, listing_id).await?;
    require_current_review_revisions(&review.review, &payload)?;
    preflight_listing_review_resolution(&state.db, &review.review, &payload).await?;
    ensure_listing_canonical_aircraft_identity(&state.db, listing_id)
        .await
        .map_err(|error| {
            ApiError::from(error).with_code("listing_aircraft_identity_preparation_failed")
        })?;
    ground_review_product_creations(&state, user.id, listing_id, &review.review, &mut payload)
        .await?;
    let resolved = resolve_listing_review(&state.db, user.id, listing_id, &payload).await?;
    if payload.finalize_listing {
        finalize_reviewed_listing_ingestion(&state.db, listing_id)
            .await
            .map_err(|error| ApiError::from(error).with_code("listing_finalization_failed"))?;
    }
    Ok(Json(
        resolved_review_response(&state.db, user.id, resolved).await?,
    ))
}

/// Reject stale browser state before any FAA preparation or paid Gemini work.
///
/// The transaction repeats these checks under its database locks; this early
/// gate is only a cost and latency optimization, never the concurrency
/// boundary.
fn require_current_review_revisions(
    review: &ListingReview,
    payload: &ResolveReviewRequest,
) -> Result<(), ApiError> {
    if payload.expected_review_payload_sha256 != review.review_payload_sha256 {
        return Err(ApiError::from(ReviewError::Stale(
            "review payload is stale; reload the review".to_string(),
        )));
    }
    if payload.expected_catalog_revision_sha256 != review.catalog_revision_sha256 {
        return Err(ApiError::from(ReviewError::Stale(
            "approved avionics catalog changed during review; reload and re-evaluate".to_string(),
        )));
    }
    Ok(())
}

fn grounded_review_identity_evidence(
    reviewer_excerpt: &str,
    approved: &ApprovedAvionicsIdentity,
    direct_source_verification: Option<&ReviewDirectSourceVerification>,
) -> Result<String, ApiError> {
    let grounded_evidence = approved.evidence.trim();
    if grounded_evidence.chars().count() <= MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS {
        return Ok(grounded_evidence.to_string());
    }

    let reviewer_excerpt = reviewer_excerpt.trim();
    let reviewer_excerpt_is_eligible = reviewer_excerpt.chars().count()
        <= MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS
        && exact_product_identity_signal_is_present(
            reviewer_excerpt,
            &approved.model,
            &approved.manufacturer_identifier,
        )
        && direct_source_verification.is_some_and(|verification| {
            verification.verifies_exact_anchor(&approved.evidence_url, reviewer_excerpt)
        });
    if !reviewer_excerpt_is_eligible {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!(
                "grounded identity evidence for {} {} exceeded the review excerpt limit, and the submitted bounded excerpt was not verified against that admitted final source",
                approved.manufacturer, approved.model
            ),
        )
        .with_code("avionics_grounding_failed"));
    }

    Ok(reviewer_excerpt.to_string())
}

/// A human corroboration can authorize a catalog write, but it cannot bypass
/// the same grounded identity and collision checks used by automatic
/// ingestion. The preflight is deliberately outside the write transaction:
/// the review transaction repeats exact catalog-revision and uniqueness checks
/// before persisting the Gemini-attested canonical fields below.
async fn ground_review_product_creations(
    state: &AppState,
    user_id: i64,
    listing_id: i64,
    review: &ListingReview,
    payload: &mut ResolveReviewRequest,
) -> Result<(), ApiError> {
    if !payload
        .decisions
        .iter()
        .any(|decision| matches!(decision, ReviewDecision::CreateVerifiedProduct { .. }))
    {
        return Ok(());
    }
    let extractor = state.extractor.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Gemini is required to ground a new verified avionics product",
        )
        .with_code("avionics_grounding_unavailable")
    })?;
    let listing = get_listing(&state.db, user_id, listing_id).await?;

    for decision in &mut payload.decisions {
        let ReviewDecision::CreateVerifiedProduct {
            aspect_id,
            unreviewed_avionics_model_id,
            manufacturer,
            model,
            capabilities,
            manufacturer_identifier_kind,
            manufacturer_identifier,
            identity_source_url,
            identity_source_title,
            identity_evidence_text,
            grounded_claim_source_urls,
        } = decision
        else {
            continue;
        };
        let aspect = review
            .aspects
            .iter()
            .find(|aspect| &aspect.id == aspect_id)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("unknown review aspect {aspect_id}"),
                )
                .with_code("review_decision_invalid")
            })?;
        let staged_candidate_id = aspect
            .proposed_product
            .as_ref()
            .and_then(|product| product.id);
        if let (Some(staged_id), Some(selected_id)) =
            (staged_candidate_id, *unreviewed_avionics_model_id)
        {
            if staged_id != selected_id {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    format!(
                        "review aspect {aspect_id} was staged with unreviewed catalog candidate id {staged_id}, but the decision selected id {selected_id}"
                    ),
                )
                .with_code("avionics_candidate_mismatch"));
            }
        }
        let promotion_candidate_id = (*unreviewed_avionics_model_id).or(staged_candidate_id);
        let submitted_identifier_kind = manufacturer_identifier_kind.trim().to_string();
        let submitted_identifier = normalize_avionics_identifier(manufacturer_identifier);
        let submitted_identity_evidence = identity_evidence_text.trim().to_string();
        let request = AvionicsIdentityRequest {
            aircraft_manufacturer: listing.aircraft.manufacturer.clone(),
            aircraft_model: listing.aircraft.model.clone(),
            aircraft_variant: listing.aircraft.variant.clone(),
            model_year: listing.model_year,
            source_url: listing.source_url.clone().unwrap_or_default(),
            listing_context: json!({
                "context_kind": "human listing review product creation",
                "listing_id": listing_id,
                    "review_aspect_id": aspect_id.to_string(),
                    "unreviewed_avionics_model_id": promotion_candidate_id,
                "reviewer_proposed_identity": {
                    "manufacturer": manufacturer,
                    "model": model,
                    "capabilities": capabilities,
                    "manufacturer_identifier_kind": manufacturer_identifier_kind,
                    "manufacturer_identifier": manufacturer_identifier,
                    "identity_source_title": identity_source_title,
                    "identity_evidence_text": identity_evidence_text,
                },
            })
            .to_string(),
            // The authoritative product source supplied by the reviewer, not
            // the sale listing, is the identity evidence for this pass.
            requires_listing_evidence: false,
            authoritative_direct_source_urls: vec![identity_source_url.clone()],
            authoritative_identity_anchors: vec![
                manufacturer.clone(),
                model.clone(),
                manufacturer_identifier.clone(),
                submitted_identity_evidence.clone(),
            ],
            manufacturer: manufacturer.clone(),
            model: model.clone(),
            avionics_types: capabilities.clone(),
            quantity: aspect.quantity.max(1),
        };
        let outcome =
            resolve_avionics_identity_for_review_preflight(&state.db, extractor, &request)
                .await
                .map_err(|error| {
                    ApiError::new(
                        StatusCode::BAD_GATEWAY,
                        format!("could not ground proposed avionics identity: {error}"),
                    )
                    .with_code("avionics_grounding_failed")
                })?;
        let (approved, direct_source_verification) = match outcome {
            ReviewPreflightAvionicsIdentityOutcome::CatalogConsolidated(approved) => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    format!(
                        "grounded review consolidated duplicate catalog rows into verified product {} {}; reload the review and select catalog id {}",
                        approved.manufacturer, approved.model, approved.id
                    ),
                )
                .with_code("avionics_catalog_consolidated"));
            }
            ReviewPreflightAvionicsIdentityOutcome::Preview {
                outcome,
                direct_source_verification,
            } => match outcome {
                AvionicsIdentityOutcome::Approved(approved) => {
                    (approved, direct_source_verification)
                }
                AvionicsIdentityOutcome::Rejected { reason } => {
                    return Err(ApiError::new(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!("proposed avionics product was rejected: {reason}"),
                    )
                    .with_code("avionics_identity_rejected"));
                }
                AvionicsIdentityOutcome::Unresolved { reason } => {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        format!("proposed avionics identity remains unresolved: {reason}"),
                    )
                    .with_code("avionics_identity_unresolved"));
                }
            },
        };

        if approved.manufacturer_identifier_kind != submitted_identifier_kind
            || normalize_avionics_identifier(&approved.manufacturer_identifier)
                != submitted_identifier
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!(
                    "Gemini did not confirm the submitted manufacturer identifier for {} {}",
                    manufacturer, model
                ),
            )
            .with_code("avionics_identifier_mismatch"));
        }

        if approved.id > 0 && Some(approved.id) != promotion_candidate_id {
            let reuse_attested =
                attest_grounded_existing_avionics_identity(&state.db, &approved)
                    .await
                    .map_err(|error| {
                        ApiError::new(
                            StatusCode::CONFLICT,
                            format!(
                                "Gemini matched catalog id {}, but its current-policy reuse attestation could not be persisted: {error}",
                                approved.id
                            ),
                        )
                        .with_code("avionics_reuse_attestation_failed")
                    })?;
            let existing_is_verified_suggestion = aspect
                .suggested_product
                .as_ref()
                .and_then(|product| product.id)
                == Some(approved.id);
            let guidance = if reuse_attested {
                "the current grounded review was saved as a reuse attestation; reload the review and use that verified product"
            } else if existing_is_verified_suggestion {
                "reload the review and use that verified product instead"
            } else {
                "consolidate or adjudicate that existing catalog candidate before creating a product"
            };
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!(
                    "Gemini matched the proposed product to existing catalog id {}; {guidance}",
                    approved.id,
                ),
            )
            .with_code(if reuse_attested {
                "avionics_identity_reuse_attested"
            } else {
                "avionics_identity_exists"
            }));
        }

        let identity_evidence = grounded_review_identity_evidence(
            &submitted_identity_evidence,
            &approved,
            direct_source_verification.as_ref(),
        )?;

        // Persist exactly the independently grounded canonical identity and
        // source metadata. The reviewer excerpt is retained only when the
        // admitted direct fetch proved that exact bounded anchor.
        *manufacturer = approved.manufacturer;
        *model = approved.model;
        *capabilities = approved.avionics_types;
        *manufacturer_identifier_kind = approved.manufacturer_identifier_kind;
        *manufacturer_identifier = approved.manufacturer_identifier;
        *identity_source_url = approved.evidence_url;
        *identity_source_title = approved.evidence_title;
        *identity_evidence_text = identity_evidence;
        *grounded_claim_source_urls = approved.grounded_claim_source_urls;
    }
    Ok(())
}

async fn aircraft_variant_detail_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(variant_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let detail = aircraft_variant_detail_with_model(
        &state.db,
        user.id,
        variant_id,
        state.valuation_model.as_ref(),
    )
    .await
    .map_err(ApiError::from)?;
    Ok(Json(json!({"current_user": user, "aircraft": detail})))
}

async fn preview_listing_payload(
    payload: Value,
    state: &AppState,
) -> Result<ListingPreview, ApiError> {
    let request: PreviewRequest = serde_json::from_value(payload).map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid request body: {error}"),
        )
    })?;
    match (request.source_url, request.listing) {
        (Some(_), Some(_)) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "provide either source_url or listing, not both",
        )),
        (Some(source_url), None) => {
            let extractor = state.extractor.clone().ok_or_else(|| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "GEMINI_API_KEY must be set to use Gemini listing extraction",
                )
            })?;
            preview_listing_url(&source_url, &extractor)
                .await
                .map_err(|error| ApiError::new(StatusCode::BAD_GATEWAY, format!("{error:#}")))
        }
        (None, Some(listing)) => {
            if !listing.is_object() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "listing must be a JSON object",
                ));
            }
            Ok(preview_manual_listing(&listing))
        }
        (None, None) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "provide source_url or listing",
        )),
    }
}

async fn load_current_user(db: &AppDb, headers: &HeaderMap) -> Result<User, ApiError> {
    let email = user_email(headers);
    db.current_user(email.as_deref())
        .await
        .map_err(|error| ApiError::new(StatusCode::UNAUTHORIZED, error.to_string()))
}

fn user_email(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-user-email")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Minimal reviewer authorization until the application has durable roles.
/// The existing header identity mechanism is unchanged; production deployments
/// must configure the exact reviewer email allowlist server-side.
fn require_listing_reviewer(user: &User) -> Result<(), ApiError> {
    let allow_local_reviewer = cfg!(debug_assertions)
        || std::env::var("AIRCOST_ALLOW_LOCAL_REVIEWER")
            .ok()
            .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes"));
    let local_developer = allow_local_reviewer
        && user.auth_provider == "local"
        && user.email.eq_ignore_ascii_case(crate::db::DEVELOPER_EMAIL);
    let configured = std::env::var("AIRCOST_REVIEWER_EMAILS")
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|email| !email.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .any(|email| email.eq_ignore_ascii_case(&user.email));
    if local_developer || configured {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "current user is not configured as a listing reviewer",
        ))
    }
}

async fn plugin_submission_response(
    db: &AppDb,
    valuation_model: Option<&Arc<dyn ValuationModel>>,
    user: User,
    outcome: PluginSubmissionOutcome,
) -> Value {
    let listing_estimate = match outcome.listing.as_ref() {
        Some(listing) => {
            aircraft_listing_value_with_model(db, user.id, listing.id, valuation_model)
                .await
                .ok()
        }
        None => None,
    };
    json!({
        "current_user": user,
        "submission": outcome.submission,
        "preview": outcome.preview,
        "listing": outcome.listing,
        "listing_estimate": listing_estimate,
    })
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
    code: Option<&'static str>,
    details: Option<Value>,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: None,
            details: None,
        }
    }

    fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status;
        let mut error = json!({
            "message": self.message,
            "status": status.as_u16(),
            "code": self.code,
        });
        if let Some(Value::Object(details)) = self.details {
            if let Some(error) = error.as_object_mut() {
                error.extend(details);
            }
        }
        let body = Json(json!({ "error": error }));
        (status, body).into_response()
    }
}

impl From<ListingStoreError> for ApiError {
    fn from(error: ListingStoreError) -> Self {
        match error {
            ListingStoreError::Validation(message) => {
                ApiError::new(StatusCode::BAD_REQUEST, message)
            }
            ListingStoreError::NotFound(message) => ApiError::new(StatusCode::NOT_FOUND, message),
            ListingStoreError::Permission(message) => ApiError::new(StatusCode::FORBIDDEN, message),
            ListingStoreError::State(message) => ApiError::new(StatusCode::CONFLICT, message),
            ListingStoreError::AircraftAdmission(error) => {
                ApiError::new(StatusCode::CONFLICT, error.to_string())
            }
            ListingStoreError::Ingestion {
                listing_id,
                message,
            } => ApiError::new(
                StatusCode::CONFLICT,
                format!("listing {listing_id} was quarantined: {message}"),
            ),
            ListingStoreError::Database(message) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, message)
            }
        }
    }
}

impl From<PluginStoreError> for ApiError {
    fn from(error: PluginStoreError) -> Self {
        match error {
            PluginStoreError::Validation(message) => {
                ApiError::new(StatusCode::BAD_REQUEST, message)
            }
            PluginStoreError::DeterministicValidation(error) => {
                ApiError::new(StatusCode::BAD_REQUEST, error.to_string())
            }
            PluginStoreError::Permission(message) => ApiError::new(StatusCode::FORBIDDEN, message),
            PluginStoreError::NotFound(message) => ApiError::new(StatusCode::NOT_FOUND, message),
            PluginStoreError::AircraftAdmission(error) => {
                ApiError::new(StatusCode::CONFLICT, error.to_string())
            }
            PluginStoreError::AdmissionBlocked(reason) => ApiError::new(
                StatusCode::CONFLICT,
                format!("replay admission is blocked: {}", reason.code()),
            ),
            PluginStoreError::Database(message) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, message)
            }
        }
    }
}

impl From<AircraftStoreError> for ApiError {
    fn from(error: AircraftStoreError) -> Self {
        match error {
            AircraftStoreError::NotFound(message) => ApiError::new(StatusCode::NOT_FOUND, message),
            AircraftStoreError::Model(message) => ApiError::new(StatusCode::BAD_GATEWAY, message),
            AircraftStoreError::Database(message) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, message)
            }
        }
    }
}

impl From<AvionicsInspectionError> for ApiError {
    fn from(error: AvionicsInspectionError) -> Self {
        match error {
            AvionicsInspectionError::Validation(message) => {
                ApiError::new(StatusCode::BAD_REQUEST, message)
            }
            AvionicsInspectionError::NotFound(message) => {
                ApiError::new(StatusCode::NOT_FOUND, message)
            }
            AvionicsInspectionError::Database(message) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, message)
            }
        }
    }
}

impl From<ConsolidationError> for ApiError {
    fn from(error: ConsolidationError) -> Self {
        match error {
            ConsolidationError::Validation(message) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, message)
                    .with_code("avionics_consolidation_invalid")
            }
            ConsolidationError::Conflict(message) => ApiError::new(StatusCode::CONFLICT, message)
                .with_code("avionics_consolidation_conflict"),
            ConsolidationError::Database(message) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, message)
                    .with_code("avionics_consolidation_failed")
            }
        }
    }
}

impl From<AvionicsProductDeletionError> for ApiError {
    fn from(error: AvionicsProductDeletionError) -> Self {
        match error {
            AvionicsProductDeletionError::Validation(message) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, message)
                    .with_code("avionics_product_deletion_invalid")
            }
            AvionicsProductDeletionError::NotFound(message) => {
                ApiError::new(StatusCode::NOT_FOUND, message)
                    .with_code("avionics_product_not_found")
            }
            AvionicsProductDeletionError::Conflict(message) => {
                ApiError::new(StatusCode::CONFLICT, message)
                    .with_code("avionics_product_deletion_conflict")
            }
            AvionicsProductDeletionError::Database(message) => {
                eprintln!("avionics product deletion failed: {message}");
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "The avionics product could not be deleted.",
                )
                .with_code("avionics_product_deletion_failed")
            }
        }
    }
}

impl From<ReviewError> for ApiError {
    fn from(error: ReviewError) -> Self {
        match error {
            ReviewError::Validation(message) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, message)
            }
            ReviewError::Stale(message) => {
                ApiError::new(StatusCode::PRECONDITION_FAILED, message).with_code("review_stale")
            }
            ReviewError::Conflict(message) => {
                ApiError::new(StatusCode::CONFLICT, message).with_code("review_conflict")
            }
            ReviewError::NotFound(message) => ApiError::new(StatusCode::NOT_FOUND, message),
            ReviewError::Permission(message) => ApiError::new(StatusCode::FORBIDDEN, message),
            ReviewError::Database(message) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, message)
            }
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}"))
    }
}

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_CSS: &str = include_str!("../web/app.css");
const APP_JS: &str = include_str!("../web/app.js");
const AVIONICS_JS: &str = include_str!("../web/avionics.js");
const REVIEW_JS: &str = include_str!("../web/review.js");
const REVIEW_DOMAIN_JS: &str = include_str!("../web/review/domain.mjs");
const REVIEW_AUTOMATION_JS: &str = include_str!("../web/review/automation.mjs");

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::to_bytes;
    use axum::extract::{Path, Query, State};
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
    use axum::Json;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine as _;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use serde_json::json;
    use sqlx::SqlitePool;
    use tokio::sync::Notify;

    use super::{
        approve_replacement_products_handler, attest_review_avionics_product_handler,
        avionics_options_handler, cancel_verification_run_handler, create_verification_run_handler,
        get_listing_review, get_verification_run_handler, grounded_review_identity_evidence,
        list_avionics_handler, list_verification_run_items_handler,
        process_claimed_verification_run_item, proposed_identity_matches_consolidation_members,
        rebuild_listing_avionics_review_handler, require_current_review_revisions,
        required_idempotency_key, start_plugin_submission_job,
        use_existing_review_avionics_handler, verification_run_api_error,
        verification_run_failure_reason, verify_existing_review_avionics_handler, AppState,
        AttestReviewAvionicsProductRequest, CreateVerificationRunHttpRequest,
        RebuildPendingAvionicsReviewRequest, ReviewerListingPreflightQuery,
        UseExistingReviewAvionicsRequest, VerificationRunItemsHttpQuery,
        VerifyExistingReviewAvionicsRequest, REVIEW_AUTOMATION_JS,
    };
    use crate::aircraft::faa::require_listing_faa_admission;
    use crate::avionics::catalog::{ApprovedAvionicsIdentity, ReviewDirectSourceVerification};
    use crate::avionics::inspection::AvionicsCatalogQuery;
    use crate::avionics::manufacturer::{
        ensure_manufacturer_identity, ManufacturerIdentityEvidence,
    };
    use crate::avionics::reuse::refresh_reuse_attestation_sqlite;
    use crate::db::{AppDb, DatabaseBackend};
    use crate::listing::review::replacement::{
        ApproveReplacementProductsRequest, ReplacementProductSelection,
    };
    use crate::listing::review::{
        restage_unattested_preserved_products, stage_pending_review, ListingAssociationRole,
        ListingReview, PendingReviewAspect, ResolveReviewRequest, ReviewAircraftIdentityState,
        ReviewAircraftIdentityStatus, ReviewAircraftSummary, ReviewAspectId, ReviewProduct,
    };
    use crate::listing::run::{
        claim_next_verification_run_item, create_verification_run, get_verification_run,
        list_verification_run_items, CreateVerificationRunRequest, VerificationRunError,
        VerificationRunItemStatus, VerificationRunItemsQuery, VerificationRunStatus,
    };
    use crate::listing::verification::ListingVerificationError;
    use crate::models::PluginSubmissionRequest;
    use crate::normalize::{
        normalize_avionics_identifier, normalize_avionics_manufacturer_name,
        normalize_avionics_model_name, normalize_name,
    };
    use crate::plugin::{
        plugin_url_status, register_plugin_install, sha256_hex, signature_message,
    };
    use crate::valuation::store::{ServingValuationState, ServingValuationStatus};

    #[test]
    fn oversized_grounded_review_evidence_uses_only_the_verified_bounded_anchor() {
        let final_source_url = "https://static.garmin.com/manuals/gdc74a.pdf";
        let reviewer_excerpt = "Garmin identifies GDC 74A by manufacturer model number GDC 74A.";
        let approved = ApprovedAvionicsIdentity {
            id: 0,
            manufacturer: "Garmin".to_string(),
            model: "GDC 74A".to_string(),
            avionics_types: vec!["Air data computer".to_string()],
            manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
            manufacturer_identifier: "GDC 74A".to_string(),
            evidence_url: final_source_url.to_string(),
            evidence_title: "Garmin GDC 74A installation manual".to_string(),
            evidence: format!(
                "Garmin identifies GDC 74A by manufacturer model number GDC 74A. {}",
                "Publisher details. ".repeat(8)
            ),
            reason: "The admitted manufacturer source confirms the exact identity.".to_string(),
            grounded_claim_source_urls: vec![final_source_url.to_string()],
            verified_local_reuse_proof: None,
        };
        assert!(approved.evidence.chars().count() > 128);

        let verified = ReviewDirectSourceVerification::for_test(final_source_url, reviewer_excerpt);
        assert_eq!(
            grounded_review_identity_evidence(reviewer_excerpt, &approved, Some(&verified),)
                .expect("the exact freshly verified publisher anchor remains eligible"),
            reviewer_excerpt
        );

        let unverified =
            grounded_review_identity_evidence(reviewer_excerpt, &approved, None).unwrap_err();
        assert_eq!(unverified.code, Some("avionics_grounding_failed"));

        let wrong_anchor = ReviewDirectSourceVerification::for_test(
            final_source_url,
            "Garmin identifies GMU 44 by manufacturer model number GMU 44.",
        );
        let mismatched =
            grounded_review_identity_evidence(reviewer_excerpt, &approved, Some(&wrong_anchor))
                .unwrap_err();
        assert_eq!(mismatched.code, Some("avionics_grounding_failed"));

        let wrong_source = ReviewDirectSourceVerification::for_test(
            "https://attacker.example/gdc74a.pdf",
            reviewer_excerpt,
        );
        assert!(grounded_review_identity_evidence(
            reviewer_excerpt,
            &approved,
            Some(&wrong_source),
        )
        .is_err());
    }
    fn test_state(db: AppDb) -> AppState {
        AppState {
            db,
            extractor: None,
            automatic_aircraft_gemini: None,
            automatic_aircraft_drs: None,
            automatic_runtime_config: None,
            verification_run_wake: Arc::new(Notify::new()),
            valuation_model: None,
            valuation_status: ServingValuationStatus {
                state: ServingValuationState::Unavailable,
                calibrated: false,
                listing_only_available: false,
                model_kind: None,
                model_version_id: None,
                snapshot_id: None,
                warnings: vec![],
            },
        }
    }

    #[test]
    fn consolidation_endpoint_accepts_any_authorized_member_model_key() {
        let members = [
            ("Garmin", "g1000"),
            ("Garmin", "g1000 integrated flight deck"),
        ];
        assert!(proposed_identity_matches_consolidation_members(
            "Garmin",
            "G1000 Integrated Flight Deck",
            members
        ));
        assert!(!proposed_identity_matches_consolidation_members(
            "Garmin",
            "G1000 NXi",
            members
        ));
    }

    fn sqlite_pool(db: &AppDb) -> &SqlitePool {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("server test database is not SQLite");
        };
        pool
    }

    #[test]
    fn verification_run_requests_reject_unknown_fields() {
        let payload = json!({
            "listing_ids": [42],
            "apply": true
        });
        assert!(
            serde_json::from_value::<CreateVerificationRunHttpRequest>(payload).is_err(),
            "run creation must reject caller-selected execution flags"
        );
        assert!(
            serde_json::from_value::<VerificationRunItemsHttpQuery>(json!({
                "limit": 10,
                "owner_user_id": 7
            }))
            .is_err(),
            "item pagination must reject caller-selected ownership"
        );
    }

    #[test]
    fn verification_run_requires_exactly_one_nonempty_idempotency_key() {
        let missing = HeaderMap::new();
        assert_eq!(
            required_idempotency_key(&missing).unwrap_err().status,
            StatusCode::BAD_REQUEST
        );

        let mut empty = HeaderMap::new();
        empty.insert("idempotency-key", HeaderValue::from_static("   "));
        assert_eq!(
            required_idempotency_key(&empty).unwrap_err().status,
            StatusCode::BAD_REQUEST
        );

        let mut duplicate = HeaderMap::new();
        duplicate.append("idempotency-key", HeaderValue::from_static("first"));
        duplicate.append("idempotency-key", HeaderValue::from_static("second"));
        assert_eq!(
            required_idempotency_key(&duplicate).unwrap_err().status,
            StatusCode::BAD_REQUEST
        );

        let mut valid = HeaderMap::new();
        valid.insert(
            "idempotency-key",
            HeaderValue::from_static(" browser-request-42 "),
        );
        assert_eq!(
            required_idempotency_key(&valid).unwrap(),
            "browser-request-42"
        );
    }

    #[test]
    fn verification_run_errors_do_not_expose_provider_or_database_details() {
        let api_error = verification_run_api_error(VerificationRunError::Database(
            "database password and raw SQL".to_string(),
        ));
        assert_eq!(api_error.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!api_error.message.contains("password"));
        assert!(!api_error.message.contains("SQL"));

        let (reason_code, reason) = verification_run_failure_reason(
            &ListingVerificationError::Avionics("raw Gemini response".to_string()),
        );
        assert_eq!(reason_code, "avionics_verification_failed");
        assert!(!reason.contains("Gemini"));
        assert!(!reason.contains("raw"));
    }

    #[tokio::test]
    async fn verification_run_http_creation_is_idempotent_and_reports_active_conflicts() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (owner_user_id, listing_id) = insert_review_listing(&db).await;
        let (_, other_listing_id) = insert_review_listing(&db).await;
        let state = test_state(db.clone());
        let headers = verification_run_headers("browser-request-42");

        let first = create_verification_run_handler(
            State(state.clone()),
            headers.clone(),
            Json(CreateVerificationRunHttpRequest {
                listing_ids: vec![listing_id],
            }),
        )
        .await
        .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let location = first
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let first_body: serde_json::Value =
            serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        let run_id = first_body["run"]["id"].as_i64().unwrap();
        assert_eq!(location, format!("/api/review/verification-runs/{run_id}"));
        let actual_fields = first_body["run"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected_fields = [
            "blocked_items",
            "cancelled_items",
            "current_listing_id",
            "failed_items",
            "id",
            "pending_review_items",
            "queued_items",
            "running_items",
            "status",
            "total_items",
            "verified_items",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert_eq!(actual_fields, expected_fields);
        assert_eq!(first_body["run"]["total_items"], 1);
        assert_eq!(first_body["run"]["queued_items"], 1);

        let replay = create_verification_run_handler(
            State(state.clone()),
            headers.clone(),
            Json(CreateVerificationRunHttpRequest {
                listing_ids: vec![listing_id],
            }),
        )
        .await
        .unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(
            replay.headers().get(header::LOCATION).unwrap(),
            location.as_str()
        );

        let idempotency_conflict = create_verification_run_handler(
            State(state.clone()),
            headers,
            Json(CreateVerificationRunHttpRequest {
                listing_ids: vec![other_listing_id],
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(idempotency_conflict.status, StatusCode::CONFLICT);
        assert_eq!(
            idempotency_conflict.code,
            Some("verification_run_idempotency_conflict")
        );
        assert_eq!(
            idempotency_conflict.details.unwrap()["active_run_id"],
            run_id
        );

        let active_conflict = create_verification_run_handler(
            State(state),
            verification_run_headers("another-browser-request"),
            Json(CreateVerificationRunHttpRequest {
                listing_ids: vec![listing_id],
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(active_conflict.status, StatusCode::CONFLICT);
        assert_eq!(
            active_conflict.code,
            Some("verification_run_listing_active")
        );
        let details = active_conflict.details.unwrap();
        assert_eq!(details["listing_id"], listing_id);
        assert_eq!(details["active_run_id"], run_id);

        let stored_run = crate::listing::run::get_verification_run(&db, owner_user_id, run_id)
            .await
            .unwrap();
        assert_eq!(stored_run.total_items, 1);
    }

    #[tokio::test]
    async fn verification_run_http_reads_are_owner_scoped_and_keyset_paginated() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (owner_user_id, first_listing_id) = insert_review_listing(&db).await;
        let (_, second_listing_id) = insert_review_listing(&db).await;
        let created = create_verification_run(
            &db,
            &CreateVerificationRunRequest {
                owner_user_id,
                idempotency_key: "pagination-test".to_string(),
                listing_ids: vec![first_listing_id, second_listing_id],
            },
        )
        .await
        .unwrap();
        let state = test_state(db.clone());

        let Json(first_page) = list_verification_run_items_handler(
            State(state.clone()),
            HeaderMap::new(),
            Path(created.run.id),
            Query(VerificationRunItemsHttpQuery {
                limit: Some(1),
                after_item_id: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(first_page["items"].as_array().unwrap().len(), 1);
        assert_eq!(first_page["checkpoint"]["has_more"], true);
        let cursor = first_page["checkpoint"]["resume_after_item_id"]
            .as_i64()
            .unwrap();
        let item_fields = first_page["items"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            item_fields,
            [
                "id",
                "listing_id",
                "outcome",
                "reason",
                "reason_code",
                "status"
            ]
            .into_iter()
            .collect()
        );

        let Json(second_page) = list_verification_run_items_handler(
            State(state.clone()),
            HeaderMap::new(),
            Path(created.run.id),
            Query(VerificationRunItemsHttpQuery {
                limit: Some(1),
                after_item_id: Some(cursor),
            }),
        )
        .await
        .unwrap();
        assert_eq!(second_page["items"].as_array().unwrap().len(), 1);
        assert_eq!(second_page["checkpoint"]["has_more"], false);
        assert_eq!(
            second_page["checkpoint"]["resume_after_item_id"],
            second_page["items"][0]["id"]
        );

        let pool = sqlite_pool(&db);
        let foreign_user_id: i64 = sqlx::query_scalar(
            "INSERT INTO users (email, display_name, auth_subject) VALUES ('foreign@example.com', 'Foreign', 'foreign-subject') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let variant_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let foreign_listing_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            ) VALUES (?, ?, 'https://broker.example/aircraft/foreign-review',
                      2020, 450000, 900)
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .bind(foreign_user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let foreign_run = create_verification_run(
            &db,
            &CreateVerificationRunRequest {
                owner_user_id: foreign_user_id,
                idempotency_key: "foreign-run".to_string(),
                listing_ids: vec![foreign_listing_id],
            },
        )
        .await
        .unwrap();
        let hidden =
            get_verification_run_handler(State(state), HeaderMap::new(), Path(foreign_run.run.id))
                .await
                .unwrap_err();
        assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn verification_run_worker_persists_a_terminal_sanitized_outcome() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (owner_user_id, listing_id) = insert_review_listing(&db).await;
        let created = create_verification_run(
            &db,
            &CreateVerificationRunRequest {
                owner_user_id,
                idempotency_key: "worker-terminal-test".to_string(),
                listing_ids: vec![listing_id],
            },
        )
        .await
        .unwrap();
        let lease_token = "test-worker-terminal-lease";
        let claim = claim_next_verification_run_item(&db, lease_token, Duration::from_secs(60))
            .await
            .unwrap()
            .expect("the queued item should be claimable");

        process_claimed_verification_run_item(&test_state(db.clone()), claim, lease_token).await;

        let run = get_verification_run(&db, owner_user_id, created.run.id)
            .await
            .unwrap();
        assert_eq!(run.status, VerificationRunStatus::Completed);
        assert_eq!(run.running_items, 0);
        assert_eq!(run.blocked_items, 1);
        let page = list_verification_run_items(
            &db,
            owner_user_id,
            run.id,
            &VerificationRunItemsQuery::default(),
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 1);
        let item = &page.items[0];
        assert_eq!(item.status, VerificationRunItemStatus::Blocked);
        assert!(item.outcome.is_some());
        assert_eq!(
            item.reason_code.as_deref(),
            Some("aircraft_verification_remaining")
        );
        assert!(item
            .reason
            .as_deref()
            .is_some_and(|reason| !reason.contains("database")));
    }

    #[tokio::test]
    async fn verification_run_worker_applies_a_zero_provider_plan_without_gemini() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (owner_user_id, listing_id) = insert_review_listing(&db).await;
        insert_server_faa_admission(&db, listing_id).await;
        let evidence = "Garmin GNS 430W installed";
        let submission_id = insert_review_bound_submission(
            &db,
            owner_user_id,
            listing_id,
            &format!("<p>{evidence}</p>"),
        )
        .await;
        sqlx::query(
            "UPDATE plugin_submissions SET extracted_listing_json = ?, extraction_error = NULL WHERE id = ?",
        )
        .bind(
            serde_json::json!({
                "manufacturer": "Test Aircraft",
                "model": "Test Model",
                "variant": "Test Variant",
                "model_year": 2020,
                "asking_price_usd": 450000,
                "currency": "USD",
                "airframe_hours": 900,
                "engine_hours": null,
                "engine_time_basis": "unknown",
                "engine_time_evidence": null,
                "engine_time_confidence": null,
                "propeller_hours": null,
                "propeller_time_basis": "unknown",
                "propeller_time_evidence": null,
                "propeller_time_confidence": null,
                "installed_engine": null,
                "installed_propeller": null,
                "registration_number": "N123AB",
                "serial_number": null,
                "status": "for_sale",
                "avionics": [{
                    "manufacturer": "Garmin",
                    "model": "GNS 430W",
                    "types": ["GPS"],
                    "quantity": 1,
                    "configuration_action": "installed",
                    "replaces": null,
                    "source_evidence_text": evidence,
                    "source_confidence": "high"
                }],
                "valuation_facts": []
            })
            .to_string(),
        )
        .bind(submission_id)
        .execute(pool)
        .await
        .unwrap();
        let product_id = insert_approved_garmin_product(&db).await;
        attest_approved_garmin_product(&db, product_id).await;
        stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[PendingReviewAspect::avionics(
                "server:keyless-local:0",
                "avionics",
                "Garmin GNS 430W",
                evidence,
                "exact catalog suggestion requires replay",
                1,
                "installed",
                Some(evidence.to_string()),
                Some("high".to_string()),
            )
            .with_suggested_product(ReviewProduct::verified(
                product_id,
                "Garmin",
                "GNS 430W",
                vec!["GPS".to_string()],
            ))],
        )
        .await
        .unwrap();

        let preflight = crate::listing::verification::verify_listings(
            &db,
            crate::listing::verification::ListingVerificationMode::Preflight,
            &crate::listing::verification::ListingVerificationScope::new(1, Some(listing_id), None),
            crate::listing::verification::ListingVerificationServices::unavailable(),
        )
        .await
        .unwrap();
        assert!(
            !preflight.provider_request_plan.requires_provider(),
            "unexpected provider plan: {:#?}",
            preflight.provider_request_plan
        );

        let created = create_verification_run(
            &db,
            &CreateVerificationRunRequest {
                owner_user_id,
                idempotency_key: "worker-keyless-local-test".to_string(),
                listing_ids: vec![listing_id],
            },
        )
        .await
        .unwrap();
        let lease_token = "test-worker-keyless-local-lease";
        let claim = claim_next_verification_run_item(&db, lease_token, Duration::from_secs(60))
            .await
            .unwrap()
            .expect("the queued item should be claimable");
        process_claimed_verification_run_item(&test_state(db.clone()), claim, lease_token).await;

        let run = get_verification_run(&db, owner_user_id, created.run.id)
            .await
            .unwrap();
        assert_eq!(run.status, VerificationRunStatus::Completed);
        assert_eq!(run.failed_items, 0);
        assert_eq!(run.pending_review_items, 1);
        let page = list_verification_run_items(
            &db,
            owner_user_id,
            run.id,
            &VerificationRunItemsQuery::default(),
        )
        .await
        .unwrap();
        let item = &page.items[0];
        assert_eq!(item.status, VerificationRunItemStatus::PendingReview);
        assert_ne!(
            item.reason_code.as_deref(),
            Some("automatic_verification_unavailable")
        );
        assert_eq!(
            item.outcome
                .as_ref()
                .and_then(|outcome| outcome["avionics"]["accepted"].as_u64()),
            Some(1)
        );
        let stored: (i64, i64, i64) = sqlx::query_as(
            r#"
            SELECT
              (SELECT COUNT(*) FROM aircraft_sale_listing_avionics
                WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?),
              (SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews
                WHERE listing_id = ?),
              (SELECT COUNT(*) FROM gemini_api_usage
                WHERE aircraft_sale_listing_id = ?)
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .bind(listing_id)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored, (1, 0, 0));
    }

    #[tokio::test]
    async fn cancelling_a_run_stops_queued_work_without_stealing_the_current_lease() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (owner_user_id, first_listing_id) = insert_review_listing(&db).await;
        let (_, second_listing_id) = insert_review_listing(&db).await;
        let created = create_verification_run(
            &db,
            &CreateVerificationRunRequest {
                owner_user_id,
                idempotency_key: "worker-cancellation-test".to_string(),
                listing_ids: vec![first_listing_id, second_listing_id],
            },
        )
        .await
        .unwrap();
        let lease_token = "test-worker-cancellation-lease";
        let claim = claim_next_verification_run_item(&db, lease_token, Duration::from_secs(60))
            .await
            .unwrap()
            .expect("the first item should be claimable");
        assert_eq!(claim.listing_id, first_listing_id);

        let Json(cancelled) = cancel_verification_run_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(created.run.id),
        )
        .await
        .unwrap();
        assert_eq!(cancelled["run"]["status"], "cancelling");
        assert_eq!(cancelled["run"]["running_items"], 1);
        assert_eq!(cancelled["run"]["cancelled_items"], 1);
        assert_eq!(
            cancelled["run"]["current_listing_id"],
            serde_json::json!(first_listing_id)
        );

        process_claimed_verification_run_item(&test_state(db.clone()), claim, lease_token).await;
        let run = get_verification_run(&db, owner_user_id, created.run.id)
            .await
            .unwrap();
        assert_eq!(run.status, VerificationRunStatus::Cancelled);
        assert_eq!(run.running_items, 0);
        assert_eq!(run.cancelled_items, 1);
        assert_eq!(run.blocked_items, 1);
        assert!(
            claim_next_verification_run_item(&db, "no-more-work", Duration::from_secs(60))
                .await
                .unwrap()
                .is_none()
        );
    }

    fn verification_run_headers(idempotency_key: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", HeaderValue::from_static(idempotency_key));
        headers
    }

    #[test]
    fn reviewer_preflight_query_cannot_accept_an_owner_or_execution_mode() {
        for payload in [
            json!({"owner_user_id": 42}),
            json!({"mode": "apply"}),
            json!({"limit": 10, "listing_id": 42, "unexpected": true}),
        ] {
            assert!(serde_json::from_value::<ReviewerListingPreflightQuery>(payload).is_err());
        }
    }

    #[test]
    fn review_automation_module_is_embedded() {
        assert!(!REVIEW_AUTOMATION_JS.trim().is_empty());
    }

    async fn insert_review_listing(db: &AppDb) -> (i64, i64) {
        let pool = sqlite_pool(db);
        let owner_user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email = ?")
            .bind(crate::db::DEVELOPER_EMAIL)
            .fetch_one(pool)
            .await
            .unwrap();
        let variant_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            ) VALUES (?, ?, 'https://broker.example/aircraft/server-review/' ||
              (SELECT COUNT(*) + 1 FROM aircraft_sale_listings), 2020, 450000, 900)
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .bind(owner_user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        (owner_user_id, listing_id)
    }

    async fn insert_server_faa_admission(db: &AppDb, listing_id: i64) {
        let pool = sqlite_pool(db);
        sqlx::query(
            "UPDATE aircraft_sale_listings SET registration_number = 'N123AB', serial_number = NULL WHERE id = ?",
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();
        let release_url =
            format!("https://www.faa.gov/aircraft-registry/server-test-{listing_id}.zip");
        let archive_sha256 = sha256_hex(release_url.as_bytes());
        let evidence_source_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO curation_evidence_sources (
              source_url, resolved_url, source_title, publisher, source_domain,
              source_tier, content_sha256, retrieved_at
            ) VALUES (
              ?, ?, 'FAA registry fixture', 'Federal Aviation Administration',
              'faa.gov', 'regulator_primary', ?, CURRENT_TIMESTAMP
            )
            RETURNING id
            "#,
        )
        .bind(&release_url)
        .bind(&release_url)
        .bind(&archive_sha256)
        .fetch_one(pool)
        .await
        .unwrap();
        let snapshot_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO faa_registry_snapshots (
              evidence_source_id, snapshot_date, source_url, archive_sha256,
              source_manifest_sha256, target_set_sha256,
              master_member_name, master_member_sha256,
              aircraft_member_name, aircraft_member_sha256,
              engine_member_name, engine_member_sha256, record_hash_domain
            ) VALUES (
              ?, '2026-08-27', ?, ?, ?, ?, 'MASTER.txt', ?,
              'ACFTREF.txt', ?, 'ENGINE.txt', ?,
              'aircost-faa-master-retained-aircraft-projection-v1'
            )
            RETURNING id
            "#,
        )
        .bind(evidence_source_id)
        .bind(&release_url)
        .bind(&archive_sha256)
        .bind("b".repeat(64))
        .bind("c".repeat(64))
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .bind("f".repeat(64))
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO faa_registry_aircraft (
              snapshot_id, n_number, aircraft_code, year_manufactured,
              source_record_sha256
            ) VALUES (?, 'N123AB', 'TEST-1', 2020, ?)
            "#,
        )
        .bind(snapshot_id)
        .bind("0".repeat(64))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO faa_registry_aircraft_references (
              snapshot_id, aircraft_code, manufacturer_name, model_name
            ) VALUES (?, 'TEST-1', 'CESSNA', '182H')
            "#,
        )
        .bind(snapshot_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO faa_registry_coverage (snapshot_id, n_number, lookup_status) VALUES (?, 'N123AB', 'matched')",
        )
        .bind(snapshot_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_review_bound_submission(
        db: &AppDb,
        owner_user_id: i64,
        listing_id: i64,
        rendered_html: &str,
    ) -> i64 {
        let pool = sqlite_pool(db);
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'test-key') RETURNING id",
        )
        .bind(owner_user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let source_url: String =
            sqlx::query_scalar("SELECT source_url FROM aircraft_sale_listings WHERE id = ?")
                .bind(listing_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let rendered_html_sha256 = sha256_hex(rendered_html.as_bytes());
        sqlx::query_scalar(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, canonical_listing_id
            ) VALUES (?, ?, ?, ?, ?, 'test-signature', ?)
            RETURNING id
            "#,
        )
        .bind(owner_user_id)
        .bind(install_id)
        .bind(source_url)
        .bind(rendered_html)
        .bind(rendered_html_sha256)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn store_current_review_avionics_checkpoint(
        db: &AppDb,
        submission_id: i64,
        avionics: serde_json::Value,
    ) {
        let checkpoint = json!({
            "manufacturer": "Test Aircraft",
            "model": "Test Model",
            "variant": "Test Variant",
            "model_year": 2020,
            "asking_price_usd": 450000,
            "currency": "USD",
            "airframe_hours": 900,
            "engine_hours": null,
            "engine_time_basis": "unknown",
            "engine_time_evidence": null,
            "engine_time_confidence": null,
            "propeller_hours": null,
            "propeller_time_basis": "unknown",
            "propeller_time_evidence": null,
            "propeller_time_confidence": null,
            "installed_engine": null,
            "installed_propeller": null,
            "registration_number": null,
            "serial_number": null,
            "status": "for_sale",
            "avionics": avionics,
            "valuation_facts": []
        });
        sqlx::query(
            "UPDATE plugin_submissions SET extracted_listing_json = ?, extraction_error = NULL WHERE id = ?",
        )
        .bind(checkpoint.to_string())
        .bind(submission_id)
        .execute(sqlite_pool(db))
        .await
        .unwrap();
    }

    async fn insert_approved_garmin_product(db: &AppDb) -> i64 {
        insert_approved_garmin_product_named(db, "GNS 430W", "011-01064-40").await
    }

    async fn insert_approved_garmin_product_named(
        db: &AppDb,
        model: &str,
        identifier: &str,
    ) -> i64 {
        let pool = sqlite_pool(db);
        let manufacturer_key = normalize_avionics_manufacturer_name("Garmin");
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Garmin', ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(&manufacturer_key)
        .execute(pool)
        .await
        .unwrap();
        let manufacturer_id: i64 =
            sqlx::query_scalar("SELECT id FROM avionics_manufacturers WHERE normalized_name = ?")
                .bind(&manufacturer_key)
                .fetch_one(pool)
                .await
                .unwrap();
        ensure_manufacturer_identity(
            db,
            manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: "https://www.garmin.com/en-US/aviation/".to_string(),
                source_title: "Garmin Aviation".to_string(),
                evidence_text:
                    "Garmin's authoritative aviation site identifies Garmin as the manufacturer."
                        .to_string(),
            },
        )
        .await
        .unwrap();

        let model_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_evidence_kind, identity_confidence, catalog_reviewed_at
            ) VALUES (?, ?, ?, 'manufacturer_part_number', ?, ?,
                      'https://www.garmin.com/aviation/product',
                      'Garmin avionics product manual',
                      'Garmin identifies this avionics product by its manufacturer part number.',
                      'authoritative_reference', 'very_high', CURRENT_TIMESTAMP)
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .bind(model)
        .bind(normalize_avionics_model_name(model))
        .bind(identifier)
        .bind(normalize_avionics_identifier(identifier))
        .fetch_one(pool)
        .await
        .unwrap();
        let capability = "GPS";
        sqlx::query(
            "INSERT INTO avionics_types (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(capability)
        .bind(normalize_name(capability))
        .execute(pool)
        .await
        .unwrap();
        let capability_id: i64 =
            sqlx::query_scalar("SELECT id FROM avionics_types WHERE normalized_name = ?")
                .bind(normalize_name(capability))
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(model_id)
        .bind(capability_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        model_id
    }

    async fn attest_approved_garmin_product(db: &AppDb, avionics_model_id: i64) {
        let pool = sqlite_pool(db);
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
              'https://www.garmin.com',
              'https://www.garmin.com/aviation/product',
              'Garmin aviation product catalog',
              'Garmin publishes this exact product on its first-party aviation catalog.',
              'curated_bootstrap',
              'Server verification cleanup test'
            FROM avionics_approved_product_identities product_identity
            WHERE product_identity.avionics_model_id = ?
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(avionics_model_id)
        .execute(pool)
        .await
        .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        assert!(refresh_reuse_attestation_sqlite(
            db,
            &mut transaction,
            avionics_model_id,
            "https://www.garmin.com/aviation/product",
        )
        .await
        .unwrap());
        transaction.commit().await.unwrap();
    }

    fn review_with_revisions(review_revision: &str, catalog_revision: &str) -> ListingReview {
        ListingReview {
            listing_id: 1,
            source_url: None,
            label: "test listing".to_string(),
            aircraft: ReviewAircraftSummary {
                manufacturer: "Test".to_string(),
                model: "Model".to_string(),
                variant: "Variant".to_string(),
            },
            aircraft_identity: ReviewAircraftIdentityStatus {
                status: ReviewAircraftIdentityState::Verified,
                reason_code: None,
                faa_n_number: Some("N1".to_string()),
                faa_snapshot_id: Some(1),
                repair: None,
            },
            registration_number: Some("N1".to_string()),
            model_year: 2000,
            review_payload_sha256: review_revision.to_string(),
            catalog_revision_sha256: catalog_revision.to_string(),
            allowed_capabilities: vec![],
            aspects: vec![],
        }
    }

    #[tokio::test]
    async fn explicit_avionics_rebuild_http_contract_is_hash_guarded_and_typed() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (owner_user_id, listing_id) = insert_review_listing(&db).await;
        let evidence = "Garmin GNS 430W installed";
        let submission_id = insert_review_bound_submission(
            &db,
            owner_user_id,
            listing_id,
            &format!("<p>{evidence}</p>"),
        )
        .await;
        sqlx::query(
            "UPDATE plugin_submissions SET extracted_listing_json = ?, extraction_error = NULL WHERE id = ?",
        )
        .bind(
            serde_json::json!({"avionics": [{
                "manufacturer": "Garmin",
                "model": "GNS 430W",
                "types": ["GPS"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": evidence,
                "source_confidence": "high"
            }]})
            .to_string(),
        )
        .bind(submission_id)
        .execute(pool)
        .await
        .unwrap();
        let product_id = insert_approved_garmin_product(&db).await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', ?, 'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .bind(evidence)
        .fetch_one(pool)
        .await
        .unwrap();
        let staged = stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[PendingReviewAspect::avionics(
                "legacy-gps",
                "avionics",
                "Garmin GNS 430W",
                "Garmin GNS 430W",
                "legacy_machine_reason",
                1,
                "installed",
                Some(evidence.to_string()),
                Some("high".to_string()),
            )
            .with_covered_association(
                link_id,
                ListingAssociationRole::Installed,
                product_id,
            )],
        )
        .await
        .unwrap();
        let state = test_state(db.clone());

        let stale = rebuild_listing_avionics_review_handler(
            State(state.clone()),
            HeaderMap::new(),
            Path(listing_id),
            Json(RebuildPendingAvionicsReviewRequest {
                review_payload_sha256: "0".repeat(64),
            }),
        )
        .await
        .expect_err("the endpoint must reject a stale review revision");
        assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);

        let Json(rebuilt) = rebuild_listing_avionics_review_handler(
            State(state),
            HeaderMap::new(),
            Path(listing_id),
            Json(RebuildPendingAvionicsReviewRequest {
                review_payload_sha256: staged.review_payload_sha256,
            }),
        )
        .await
        .unwrap();
        let rebuilt = serde_json::to_value(rebuilt).unwrap();
        assert_eq!(rebuilt["status"], "rebuilt");
        assert_eq!(rebuilt["listing_id"], listing_id);
        assert_eq!(rebuilt["review_complete"], false);
        assert_eq!(rebuilt["review"]["aspects"].as_array().unwrap().len(), 1);
        assert_eq!(rebuilt["review"]["aspects"][0]["id"], "avionics:0:primary");

        let (owner_user_id, legacy_listing_id) = insert_review_listing(&db).await;
        let legacy_submission_id = insert_review_bound_submission(
            &db,
            owner_user_id,
            legacy_listing_id,
            &format!("<p>{evidence}</p>"),
        )
        .await;
        sqlx::query(
            "UPDATE plugin_submissions SET extracted_listing_json = ?, extraction_error = NULL WHERE id = ?",
        )
        .bind(
            serde_json::json!({"avionics": [{
                "manufacturer": "Garmin",
                "model": "GNS 430W",
                "types": ["GPS"],
                "source_evidence_text": evidence,
                "source_confidence": "high"
            }]})
            .to_string(),
        )
        .bind(legacy_submission_id)
        .execute(pool)
        .await
        .unwrap();
        let legacy = stage_pending_review(
            &db,
            legacy_listing_id,
            Some(legacy_submission_id),
            &[PendingReviewAspect::avionics(
                "legacy-defaults",
                "avionics",
                "Garmin GNS 430W",
                "Garmin GNS 430W",
                "legacy_machine_reason",
                1,
                "installed",
                Some(evidence.to_string()),
                Some("high".to_string()),
            )],
        )
        .await
        .unwrap();
        let Json(refused) = rebuild_listing_avionics_review_handler(
            State(test_state(db)),
            HeaderMap::new(),
            Path(legacy_listing_id),
            Json(RebuildPendingAvionicsReviewRequest {
                review_payload_sha256: legacy.review_payload_sha256,
            }),
        )
        .await
        .unwrap();
        let refused = serde_json::to_value(refused).unwrap();
        assert_eq!(refused["status"], "blocked");
        assert_eq!(refused["listing_id"], legacy_listing_id);
        assert_eq!(refused["review_complete"], false);
        assert_eq!(refused["reason_code"], "extraction_not_current");
        assert_eq!(
            refused["message"],
            "The retained extraction does not satisfy the current avionics schema. Run a validated re-extraction before rebuilding its review."
        );
        assert!(refused.get("reason").is_none());
        assert!(!refused.to_string().contains("quantity must be an explicit"));
        assert!(refused.get("review").is_none());
    }

    #[tokio::test]
    async fn multi_quantity_preserved_product_corroboration_keeps_link_unchanged_and_uses_no_gemini(
    ) {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (owner_user_id, listing_id) = insert_review_listing(&db).await;
        let submission_id = insert_review_bound_submission(
            &db,
            owner_user_id,
            listing_id,
            "<html><body>Garmin GNS 430W P/N 011-01064-40 shown in the listing</body></html>",
        )
        .await;
        store_current_review_avionics_checkpoint(
            &db,
            submission_id,
            json!([{
                "manufacturer": "Garmin",
                "model": "GNS 430W",
                "types": ["GPS"],
                "quantity": 2,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": "Garmin GNS 430W P/N 011-01064-40 shown in the listing",
                "source_confidence": "high"
            }]),
        )
        .await;
        let preserved_id = insert_approved_garmin_product(&db).await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 2, 'listing', 'Garmin GNS 430W P/N 011-01064-40 shown in the listing',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(preserved_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let primary = PendingReviewAspect::avionics(
            "genuine-primary-observation",
            "avionics_identity",
            "Garmin GTX 345",
            "Garmin GTX 345 transponder",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("GTX 345 shown in listing equipment".to_string()),
            Some("high".to_string()),
        );
        stage_pending_review(&db, listing_id, Some(submission_id), &[primary])
            .await
            .unwrap();
        let before_attestation =
            restage_unattested_preserved_products(&db, owner_user_id, listing_id)
                .await
                .unwrap()
                .expect("the genuine aspect must keep the review pending");
        assert_eq!(before_attestation.pending_aspect_count, 2);

        // This is the exact durable boundary reached by the handler only after
        // its guarded OEM fetch and deterministic product checks succeed.
        attest_approved_garmin_product(&db, preserved_id).await;
        let usage_before_cleanup: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();

        let detail = get_listing_review(&db, owner_user_id, listing_id)
            .await
            .unwrap()
            .review;
        let synthetic = detail
            .aspects
            .iter()
            .find(|aspect| {
                aspect
                    .reuse_attestation_target
                    .as_ref()
                    .and_then(|product| product.id)
                    == Some(preserved_id)
            })
            .expect("the preserved association must be staged");
        assert_eq!(synthetic.quantity, 2);
        let response = verify_existing_review_avionics_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(VerifyExistingReviewAvionicsRequest {
                review_payload_sha256: before_attestation.review_payload_sha256.clone(),
                catalog_revision_sha256: before_attestation.catalog_revision_sha256.clone(),
                aspect_id: synthetic.id.clone(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_ne!(
            response["review"]["review_payload_sha256"],
            before_attestation.review_payload_sha256
        );
        assert_eq!(response["review"]["aspects"].as_array().unwrap().len(), 1);
        assert_eq!(
            response["review"]["aspects"][0]["id"],
            "genuine-primary-observation"
        );
        assert!(response["review"]["aspects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|aspect| aspect["reuse_attestation_target"]["id"] != preserved_id));
        let usage_after_cleanup: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(usage_after_cleanup, usage_before_cleanup);
        let unchanged_link: (i64, i64, String, Option<i64>, String) = sqlx::query_as(
            r#"
            SELECT avionics_model_id, quantity, configuration_action,
                   replaces_avionics_model_id, source
            FROM aircraft_sale_listing_avionics
            WHERE id = ?
            "#,
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            unchanged_link,
            (
                preserved_id,
                2,
                "installed".to_string(),
                None,
                "listing".to_string()
            ),
            "corroboration must not rewrite the preserved association"
        );
    }

    #[tokio::test]
    async fn ordinary_hash_bound_aspect_uses_existing_product_without_gemini_usage() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (owner_user_id, listing_id) = insert_review_listing(&db).await;
        let submission_id = insert_review_bound_submission(
            &db,
            owner_user_id,
            listing_id,
            "<html><body><p>Two Garmin GNS <strong>430W</strong>\n navigators</p></body></html>",
        )
        .await;
        store_current_review_avionics_checkpoint(
            &db,
            submission_id,
            json!([{
                "manufacturer": "Garmin",
                "model": "GNS 430W",
                "types": ["GPS"],
                "quantity": 2,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": "Two Garmin GNS 430W navigators",
                "source_confidence": "high"
            }]),
        )
        .await;
        let product_id = insert_approved_garmin_product(&db).await;
        attest_approved_garmin_product(&db, product_id).await;
        let ordinary = PendingReviewAspect::avionics(
            "observation-17",
            "avionics_identity",
            "Garmin GNS 430W",
            "Two Garmin GNS 430W navigators",
            "catalog_match_requires_review",
            2,
            "installed",
            Some("Two Garmin GNS 430W navigators".to_string()),
            Some("high".to_string()),
        )
        .with_reuse_attestation_target(product_id);
        let remaining = PendingReviewAspect::avionics(
            "observation-18",
            "avionics_identity",
            "Unknown radio",
            "Unknown radio",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("Unknown radio".to_string()),
            Some("low".to_string()),
        );
        let staged =
            stage_pending_review(&db, listing_id, Some(submission_id), &[ordinary, remaining])
                .await
                .unwrap();
        let usage_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();

        let response = verify_existing_review_avionics_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(VerifyExistingReviewAvionicsRequest {
                review_payload_sha256: staged.review_payload_sha256.clone(),
                catalog_revision_sha256: staged.catalog_revision_sha256.clone(),
                aspect_id: ReviewAspectId::from("observation-17"),
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(response["review"]["aspects"].as_array().unwrap().len(), 1);
        assert_eq!(response["review"]["aspects"][0]["id"], "observation-18");
        let link: (i64, i64, String, Option<String>) = sqlx::query_as(
            r#"
            SELECT avionics_model_id, quantity, source, source_confidence
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            link,
            (
                product_id,
                2,
                "listing_review".to_string(),
                Some("high".to_string()),
            )
        );
        let usage_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(usage_after, usage_before);
    }

    #[tokio::test]
    async fn generated_avionics_explanation_is_not_listing_evidence() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (owner_user_id, listing_id) = insert_review_listing(&db).await;
        let submission_id = insert_review_bound_submission(
            &db,
            owner_user_id,
            listing_id,
            "<html><body><p>Installed Garmin GDL 690A receiver</p></body></html>",
        )
        .await;
        let product_id = insert_approved_garmin_product_named(&db, "GDL 69A", "011-00987-00").await;
        attest_approved_garmin_product(&db, product_id).await;
        let generated_explanation =
            "The candidate 'GDL690A' was identified as a typo for Garmin GDL 69A.";
        store_current_review_avionics_checkpoint(
            &db,
            submission_id,
            json!([{
                "manufacturer": "Garmin",
                "model": "GDL 69A",
                "types": ["GPS"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": generated_explanation,
                "source_confidence": "high"
            }]),
        )
        .await;
        let aspect = PendingReviewAspect::avionics(
            "observation-25",
            "avionics_identity",
            "Garmin GDL 69A",
            "Garmin GDL 690A",
            "catalog_match_requires_review",
            1,
            "installed",
            Some(generated_explanation.to_string()),
            Some("high".to_string()),
        )
        .with_reuse_attestation_target(product_id);
        let staged = stage_pending_review(&db, listing_id, Some(submission_id), &[aspect])
            .await
            .unwrap();
        let usage_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();

        let error = verify_existing_review_avionics_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(VerifyExistingReviewAvionicsRequest {
                review_payload_sha256: staged.review_payload_sha256.clone(),
                catalog_revision_sha256: staged.catalog_revision_sha256,
                aspect_id: ReviewAspectId::from("observation-25"),
            }),
        )
        .await
        .expect_err("generated reasoning must not authorize an automatic listing link");
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            error
                .message
                .contains("exact structurally visible-body span"),
            "{error:?}"
        );

        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(link_count, 0);
        let retained_hash: String = sqlx::query_scalar(
            "SELECT review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained_hash, staged.review_payload_sha256);
        let usage_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(usage_after, usage_before);
    }

    #[tokio::test]
    async fn ordinary_local_verification_without_source_capture_stays_pending() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (_owner_user_id, listing_id) = insert_review_listing(&db).await;
        let product_id = insert_approved_garmin_product(&db).await;
        attest_approved_garmin_product(&db, product_id).await;
        let aspect = PendingReviewAspect::avionics(
            "observation-manual",
            "avionics_identity",
            "Garmin GNS 430W",
            "Garmin GNS 430W",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("Garmin GNS 430W".to_string()),
            Some("high".to_string()),
        )
        .with_reuse_attestation_target(product_id);
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();

        let error = verify_existing_review_avionics_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(VerifyExistingReviewAvionicsRequest {
                review_payload_sha256: staged.review_payload_sha256.clone(),
                catalog_revision_sha256: staged.catalog_revision_sha256,
                aspect_id: ReviewAspectId::from("observation-manual"),
            }),
        )
        .await
        .expect_err("manual reviews without immutable capture must remain pending");
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(error.message.contains("exact plugin submission"));

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
    async fn current_product_attestation_is_an_idempotent_zero_fetch_success() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (owner_user_id, listing_id) = insert_review_listing(&db).await;
        let product_id = insert_approved_garmin_product(&db).await;
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing',
                      'Garmin GNS 430W P/N 011-01064-40 shown in the listing',
                      'high', 'installed')
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .execute(pool)
        .await
        .unwrap();
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[PendingReviewAspect::avionics(
                "primary-observation",
                "avionics_identity",
                "Other unit",
                "Other unit shown in the listing",
                "catalog_match_requires_review",
                1,
                "installed",
                Some("Other unit shown in the listing".to_string()),
                Some("high".to_string()),
            )],
        )
        .await
        .unwrap();
        restage_unattested_preserved_products(&db, owner_user_id, listing_id)
            .await
            .unwrap()
            .unwrap();
        attest_approved_garmin_product(&db, product_id).await;
        let review = get_listing_review(&db, owner_user_id, listing_id)
            .await
            .unwrap()
            .review;
        let authorization = review
            .aspects
            .iter()
            .find(|aspect| {
                aspect
                    .reuse_attestation_target
                    .as_ref()
                    .and_then(|product| product.id)
                    == Some(product_id)
            })
            .expect("the pending product association must authorize attestation");
        let usage_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();

        let response = attest_review_avionics_product_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(product_id),
            Json(AttestReviewAvionicsProductRequest {
                listing_id,
                review_payload_sha256: review.review_payload_sha256.clone(),
                aspect_id: authorization.id.clone(),
                catalog_revision_sha256: review.catalog_revision_sha256,
                identity_source_url: String::new(),
                identity_source_title: String::new(),
                identity_evidence_text: String::new(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(response["attestation_status"], "current");
        assert_eq!(response["reused"], true);
        assert_eq!(response["review"]["listing_id"], listing_id);
        assert_eq!(
            response["review"]["review_payload_sha256"],
            review.review_payload_sha256
        );
        let usage_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(usage_after, usage_before);
    }

    #[test]
    fn product_attestation_request_accepts_only_the_direct_authorization_contract() {
        let canonical = serde_json::from_value::<AttestReviewAvionicsProductRequest>(json!({
            "listing_id": 23,
            "review_payload_sha256": "a".repeat(64),
            "aspect_id": "preserved:1",
            "catalog_revision_sha256": "b".repeat(64),
            "identity_source_url": "https://www.garmin.com/aviation/product",
            "identity_source_title": "Garmin product",
            "identity_evidence_text": "Garmin GNS 430W 011-01064-40"
        }));
        assert!(canonical.is_ok());
        assert!(
            serde_json::from_value::<AttestReviewAvionicsProductRequest>(json!({
                "catalog_revision_sha256": "b".repeat(64),
                "identity_source_url": "https://www.garmin.com/aviation/product",
                "identity_source_title": "Garmin product",
                "identity_evidence_text": "Garmin GNS 430W 011-01064-40"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<AttestReviewAvionicsProductRequest>(json!({
                "listing_id": 23,
                "expected_review_payload_sha256": "a".repeat(64),
                "aspect_id": "preserved:1",
                "catalog_revision_sha256": "b".repeat(64),
                "identity_source_url": "https://www.garmin.com/aviation/product",
                "identity_source_title": "Garmin product",
                "identity_evidence_text": "Garmin GNS 430W 011-01064-40"
            }))
            .is_err()
        );
    }

    #[test]
    fn association_verification_request_accepts_only_the_canonical_source_free_contract() {
        let canonical = serde_json::from_value::<VerifyExistingReviewAvionicsRequest>(json!({
            "review_payload_sha256": "a".repeat(64),
            "catalog_revision_sha256": "b".repeat(64),
            "aspect_id": "preserved:1"
        }));
        assert!(canonical.is_ok());
        assert!(
            serde_json::from_value::<VerifyExistingReviewAvionicsRequest>(json!({
                "expected_review_payload_sha256": "a".repeat(64),
                "expected_catalog_revision_sha256": "b".repeat(64),
                "aspect_id": "preserved:1"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<VerifyExistingReviewAvionicsRequest>(json!({
                "review_payload_sha256": "a".repeat(64),
                "catalog_revision_sha256": "b".repeat(64),
                "aspect_id": "preserved:1",
                "identity_source_url": "https://www.garmin.com"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn aspect_scoped_use_existing_reports_missing_reuse_attestation_actionably() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (owner_user_id, listing_id) = insert_review_listing(&db).await;
        let product_id = insert_approved_garmin_product(&db).await;
        let aspect = PendingReviewAspect::avionics(
            "selected-observation",
            "avionics_identity",
            "Garmin GNS 430W",
            "Garmin GNS 430W navigator",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("Garmin GNS 430W navigator".to_string()),
            Some("high".to_string()),
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();

        let error = use_existing_review_avionics_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(UseExistingReviewAvionicsRequest {
                expected_review_payload_sha256: staged.review_payload_sha256.clone(),
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                aspect_id: "selected-observation".into(),
                avionics_model_id: product_id,
            }),
        )
        .await
        .expect_err("an approved product still requires current reuse attestation");

        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.code, Some("review_conflict"));
        assert!(error.message.contains("approved avionics catalog id"));
        assert!(error.message.contains("no current reuse attestation"));
        assert!(error.message.contains("Known avionics products"));
        let current = get_listing_review(&db, owner_user_id, listing_id)
            .await
            .unwrap();
        assert_eq!(
            current.review.review_payload_sha256,
            staged.review_payload_sha256
        );
    }

    #[tokio::test]
    async fn final_aspect_scoped_use_existing_attempts_canonical_finalization() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (_owner_user_id, listing_id) = insert_review_listing(&db).await;
        let product_id = insert_approved_garmin_product(&db).await;
        attest_approved_garmin_product(&db, product_id).await;
        let aspect = PendingReviewAspect::avionics(
            "selected-observation",
            "avionics_identity",
            "Garmin GNS 430W",
            "Garmin GNS 430W navigator",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("Garmin GNS 430W navigator".to_string()),
            Some("high".to_string()),
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();

        let response = use_existing_review_avionics_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(UseExistingReviewAvionicsRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                aspect_id: "selected-observation".into(),
                avionics_model_id: product_id,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(response["review"].is_null());
        assert_eq!(response["review_complete"], true);
        assert_eq!(response["finalization_attempted"], true);
        assert_eq!(response["listing_ready"], false);
        assert_eq!(response["listing_verified"], false);
        assert_eq!(response["listing"]["ingestion_state"], "quarantined");
        let finalization_error = response["finalization_error"]
            .as_str()
            .expect("the exact canonical aircraft blocker should be returned");
        assert!(finalization_error.contains("FAA aircraft admission rejected"));
        assert_eq!(
            response["listing"]["ingestion_error"].as_str(),
            Some(finalization_error)
        );
        let pending_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(pending_count, 0);
    }

    #[tokio::test]
    async fn final_aspect_scoped_use_existing_returns_ready_verified_listing() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (_owner_user_id, listing_id) = insert_review_listing(&db).await;
        insert_server_faa_admission(&db, listing_id).await;
        let grounding = require_listing_faa_admission(&db, listing_id)
            .await
            .expect("the server FAA fixture should admit");
        crate::aircraft::identity::seed_test_curated_identity_assignment(
            &db, listing_id, &grounding,
        )
        .await
        .expect("the fixture should receive its exact canonical hierarchy");
        let product_id = insert_approved_garmin_product(&db).await;
        attest_approved_garmin_product(&db, product_id).await;
        let aspect = PendingReviewAspect::avionics(
            "selected-observation",
            "avionics_identity",
            "Garmin GNS 430W",
            "Garmin GNS 430W navigator",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("Garmin GNS 430W navigator".to_string()),
            Some("high".to_string()),
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();

        let response = use_existing_review_avionics_handler(
            State(test_state(db)),
            HeaderMap::new(),
            Path(listing_id),
            Json(UseExistingReviewAvionicsRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                aspect_id: "selected-observation".into(),
                avionics_model_id: product_id,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(response["review"].is_null());
        assert_eq!(response["review_complete"], true);
        assert_eq!(response["finalization_attempted"], true);
        assert!(response["finalization_error"].is_null());
        assert_eq!(response["listing_ready"], true);
        assert_eq!(response["listing_verified"], true);
        assert_eq!(response["listing"]["ingestion_state"], "ready");
        assert_eq!(response["listing"]["is_verified"], true);
    }

    #[tokio::test]
    async fn aspect_scoped_use_existing_returns_only_remaining_review_without_gemini_usage() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (_owner_user_id, listing_id) = insert_review_listing(&db).await;
        let product_id = insert_approved_garmin_product(&db).await;
        attest_approved_garmin_product(&db, product_id).await;
        let selected = PendingReviewAspect::avionics(
            "selected-observation",
            "avionics_identity",
            "Garmin GNS 430W",
            "Two Garmin GNS 430W navigators",
            "catalog_match_requires_review",
            2,
            "installed",
            Some("Two Garmin GNS 430W navigators".to_string()),
            Some("high".to_string()),
        );
        let remaining = PendingReviewAspect::avionics(
            "remaining-observation",
            "avionics_identity",
            "Garmin GTX 345",
            "Garmin GTX 345 transponder",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("Garmin GTX 345 transponder".to_string()),
            Some("medium".to_string()),
        );
        let staged = stage_pending_review(&db, listing_id, None, &[selected, remaining])
            .await
            .unwrap();
        let usage_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();

        let response = use_existing_review_avionics_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(UseExistingReviewAvionicsRequest {
                expected_review_payload_sha256: staged.review_payload_sha256.clone(),
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                aspect_id: "selected-observation".into(),
                avionics_model_id: product_id,
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(response["review_complete"], false);
        assert_ne!(
            response["review"]["review_payload_sha256"],
            staged.review_payload_sha256
        );
        assert_eq!(response["review"]["aspects"].as_array().unwrap().len(), 1);
        assert_eq!(
            response["review"]["aspects"][0]["id"],
            "remaining-observation"
        );
        let link: (i64, i64, String, Option<String>) = sqlx::query_as(
            r#"
            SELECT avionics_model_id, quantity, source, source_confidence
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            link,
            (
                product_id,
                2,
                "listing_review".to_string(),
                Some("high".to_string()),
            )
        );
        let usage_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(usage_after, usage_before);
    }

    #[tokio::test]
    async fn aspect_scoped_use_existing_preserves_quantity_three_and_rejects_stale_retry() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (_owner_user_id, listing_id) = insert_review_listing(&db).await;
        let product_id = insert_approved_garmin_product(&db).await;
        attest_approved_garmin_product(&db, product_id).await;
        let selected = PendingReviewAspect::avionics(
            "selected-observation",
            "avionics_identity",
            "Garmin GNS 430W",
            "Three Garmin GNS 430W navigators",
            "catalog_match_requires_review",
            3,
            "installed",
            Some("Three Garmin GNS 430W navigators".to_string()),
            Some("high".to_string()),
        );
        let remaining = PendingReviewAspect::avionics(
            "remaining-observation",
            "avionics_identity",
            "Garmin GTX 345",
            "Garmin GTX 345 transponder",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("Garmin GTX 345 transponder".to_string()),
            Some("medium".to_string()),
        );
        let staged = stage_pending_review(&db, listing_id, None, &[selected, remaining])
            .await
            .unwrap();
        let usage_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();

        let response = use_existing_review_avionics_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(UseExistingReviewAvionicsRequest {
                expected_review_payload_sha256: staged.review_payload_sha256.clone(),
                expected_catalog_revision_sha256: staged.catalog_revision_sha256.clone(),
                aspect_id: "selected-observation".into(),
                avionics_model_id: product_id,
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(response["review_complete"], false);
        let quantity: i64 = sqlx::query_scalar(
            "SELECT quantity FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(quantity, 3);
        let usage_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(usage_after, usage_before);

        let stale = use_existing_review_avionics_handler(
            State(test_state(db)),
            HeaderMap::new(),
            Path(listing_id),
            Json(UseExistingReviewAvionicsRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                aspect_id: "selected-observation".into(),
                avionics_model_id: product_id,
            }),
        )
        .await
        .expect_err("the consumed review hash must not be replayable");
        assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(stale.code, Some("review_stale"));
    }

    #[tokio::test]
    async fn approved_replacement_products_update_one_link_in_place_without_gemini_usage() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (_owner_user_id, listing_id) = insert_review_listing(&db).await;
        let old_parent =
            insert_approved_garmin_product_named(&db, "GNS 430W", "011-01064-40").await;
        let old_child = insert_approved_garmin_product_named(&db, "KX 155", "069-1024-01").await;
        let selected_parent =
            insert_approved_garmin_product_named(&db, "GTN 650Xi", "010-02351-01").await;
        let selected_child =
            insert_approved_garmin_product_named(&db, "KX 155A", "069-1055-00").await;
        let unrelated_product =
            insert_approved_garmin_product_named(&db, "GTX 327", "011-00490-01").await;
        attest_approved_garmin_product(&db, selected_parent).await;

        let listing_link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action,
              replaces_avionics_model_id
            ) VALUES (?, ?, 2, 'listing', 'Two new navigators replace the old unit',
                      'medium', 'replaces', ?)
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(old_parent)
        .bind(old_child)
        .fetch_one(pool)
        .await
        .unwrap();
        let unrelated_link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Garmin GTX 327 transponder shown in listing',
                      'medium', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(unrelated_product)
        .fetch_one(pool)
        .await
        .unwrap();

        let parent = PendingReviewAspect::avionics(
            "replacement-parent",
            "avionics_identity",
            "two replacement navigators",
            "Two new navigators replace the old unit",
            "catalog_match_requires_review",
            2,
            "replaces",
            Some("Two new navigators replace the old unit".to_string()),
            Some("medium".to_string()),
        )
        .with_replacement_aspect("replacement-child")
        .with_covered_association(
            listing_link_id,
            crate::listing::review::ListingAssociationRole::Installed,
            old_parent,
        );
        let child = PendingReviewAspect::avionics(
            "replacement-child",
            "avionics_identity",
            "old navigator",
            "old unit",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("old unit".to_string()),
            Some("medium".to_string()),
        )
        .with_covered_association(
            listing_link_id,
            crate::listing::review::ListingAssociationRole::Replacement,
            old_child,
        );
        let staged = stage_pending_review(&db, listing_id, None, &[parent, child])
            .await
            .unwrap();
        let usage_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();

        let unattested_error = approve_replacement_products_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(ApproveReplacementProductsRequest {
                review_payload_sha256: staged.review_payload_sha256.clone(),
                catalog_revision_sha256: staged.catalog_revision_sha256.clone(),
                parent: ReplacementProductSelection {
                    aspect_id: "replacement-parent".into(),
                    product_id: selected_parent,
                    quantity: 2,
                },
                child: ReplacementProductSelection {
                    aspect_id: "replacement-child".into(),
                    product_id: selected_child,
                    quantity: 1,
                },
            }),
        )
        .await
        .expect_err("both products require current global attestations");
        assert_eq!(unattested_error.status, StatusCode::CONFLICT);
        let unchanged: (i64, i64, Option<i64>) = sqlx::query_as(
            r#"
            SELECT id, avionics_model_id, replaces_avionics_model_id
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(unchanged, (listing_link_id, old_parent, Some(old_child)));

        attest_approved_garmin_product(&db, selected_child).await;
        let single_child_error = use_existing_review_avionics_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(UseExistingReviewAvionicsRequest {
                expected_review_payload_sha256: staged.review_payload_sha256.clone(),
                expected_catalog_revision_sha256: staged.catalog_revision_sha256.clone(),
                aspect_id: "replacement-child".into(),
                avionics_model_id: selected_child,
            }),
        )
        .await
        .expect_err("a replacement child must not be approved independently");
        assert_eq!(single_child_error.status, StatusCode::UNPROCESSABLE_ENTITY);

        let accepted_request = ApproveReplacementProductsRequest {
            review_payload_sha256: staged.review_payload_sha256.clone(),
            catalog_revision_sha256: staged.catalog_revision_sha256.clone(),
            parent: ReplacementProductSelection {
                aspect_id: "replacement-parent".into(),
                product_id: selected_parent,
                quantity: 2,
            },
            child: ReplacementProductSelection {
                aspect_id: "replacement-child".into(),
                product_id: selected_child,
                quantity: 1,
            },
        };
        let response = approve_replacement_products_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(accepted_request.clone()),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(response["review_complete"], false);
        assert_eq!(response["review"]["aspects"].as_array().unwrap().len(), 1);
        assert_eq!(
            response["review"]["aspects"][0]["reuse_attestation_target"]["id"],
            unrelated_product
        );
        let link: (i64, i64, i64, String, Option<i64>, String, Option<String>) = sqlx::query_as(
            r#"
                SELECT id, avionics_model_id, quantity, configuration_action,
                       replaces_avionics_model_id, source, source_confidence
                FROM aircraft_sale_listing_avionics
                WHERE aircraft_sale_listing_id = ? AND id = ?
                "#,
        )
        .bind(listing_id)
        .bind(listing_link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            link,
            (
                listing_link_id,
                selected_parent,
                2,
                "replaces".to_string(),
                Some(selected_child),
                "listing_review".to_string(),
                Some("high".to_string()),
            )
        );
        let unrelated: (i64, i64, String, Option<String>) = sqlx::query_as(
            r#"
            SELECT id, avionics_model_id, source, source_confidence
            FROM aircraft_sale_listing_avionics
            WHERE id = ?
            "#,
        )
        .bind(unrelated_link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            unrelated,
            (
                unrelated_link_id,
                unrelated_product,
                "listing".to_string(),
                Some("medium".to_string()),
            )
        );

        let stale_retry = approve_replacement_products_handler(
            State(test_state(db.clone())),
            HeaderMap::new(),
            Path(listing_id),
            Json(accepted_request),
        )
        .await
        .expect_err("a stale retry must not insert or merge another link");
        assert_eq!(stale_retry.status, StatusCode::PRECONDITION_FAILED);
        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(link_count, 2);
        let usage_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(usage_after, usage_before);
    }

    #[tokio::test]
    async fn product_attestation_api_reports_the_exact_source_title_limit() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let (owner_user_id, listing_id) = insert_review_listing(&db).await;
        let preserved_id = insert_approved_garmin_product(&db).await;
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Garmin GNS 430W P/N 011-01064-40 shown in the listing',
                      'high', 'installed')
            "#,
        )
        .bind(listing_id)
        .bind(preserved_id)
        .execute(pool)
        .await
        .unwrap();
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[PendingReviewAspect::avionics(
                "primary-observation",
                "avionics_identity",
                "Garmin GTX 345",
                "Garmin GTX 345 transponder",
                "catalog_match_requires_review",
                1,
                "installed",
                Some("GTX 345 shown in listing equipment".to_string()),
                Some("high".to_string()),
            )],
        )
        .await
        .unwrap();
        restage_unattested_preserved_products(&db, owner_user_id, listing_id)
            .await
            .unwrap()
            .expect("the preserved product must require review");
        let review = get_listing_review(&db, owner_user_id, listing_id)
            .await
            .unwrap()
            .review;
        let authorization = review
            .aspects
            .iter()
            .find(|aspect| {
                aspect
                    .reuse_attestation_target
                    .as_ref()
                    .and_then(|product| product.id)
                    == Some(preserved_id)
            })
            .expect("the pending product association must authorize attestation");
        let error = attest_review_avionics_product_handler(
            State(test_state(db)),
            HeaderMap::new(),
            Path(preserved_id),
            Json(AttestReviewAvionicsProductRequest {
                listing_id,
                review_payload_sha256: review.review_payload_sha256.clone(),
                aspect_id: authorization.id.clone(),
                catalog_revision_sha256: review.catalog_revision_sha256.clone(),
                identity_source_url: "https://www.garmin.com/aviation/product".to_string(),
                identity_source_title: "x".repeat(201),
                identity_evidence_text:
                    "Garmin identifies GNS 430W by manufacturer part number 011-01064-40."
                        .to_string(),
            }),
        )
        .await
        .expect_err("the API must reject an oversized identity source title");
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            error.message,
            "identity_source_title must contain at most 200 characters"
        );
    }

    #[test]
    fn stale_review_revisions_are_rejected_before_grounding() {
        let review_revision = "a".repeat(64);
        let catalog_revision = "b".repeat(64);
        let review = review_with_revisions(&review_revision, &catalog_revision);
        let current = ResolveReviewRequest {
            expected_review_payload_sha256: review_revision.clone(),
            expected_catalog_revision_sha256: catalog_revision.clone(),
            finalize_listing: false,
            decisions: vec![],
        };
        require_current_review_revisions(&review, &current).unwrap();

        let stale_review = ResolveReviewRequest {
            expected_review_payload_sha256: "c".repeat(64),
            ..current
        };
        let error = require_current_review_revisions(&review, &stale_review).unwrap_err();
        assert_eq!(error.status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(error.code, Some("review_stale"));
        assert!(error.message.contains("review payload is stale"));

        let stale_catalog = ResolveReviewRequest {
            expected_review_payload_sha256: review_revision,
            expected_catalog_revision_sha256: "d".repeat(64),
            finalize_listing: false,
            decisions: vec![],
        };
        let error = require_current_review_revisions(&review, &stale_catalog).unwrap_err();
        assert_eq!(error.status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(error.code, Some("review_stale"));
        assert!(error.message.contains("catalog changed"));
    }

    #[tokio::test]
    async fn avionics_routes_authenticate_and_surface_query_validation() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let state = test_state(db);
        let options = avionics_options_handler(State(state.clone()), HeaderMap::new())
            .await
            .unwrap();
        assert!(options.0["options"]["statuses"].is_array());

        let invalid = list_avionics_handler(
            State(state.clone()),
            HeaderMap::new(),
            Query(AvionicsCatalogQuery {
                limit: Some(0),
                ..Default::default()
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(invalid.status, StatusCode::BAD_REQUEST);

        let mut unknown_headers = HeaderMap::new();
        unknown_headers.insert(
            "x-user-email",
            HeaderValue::from_static("missing@example.test"),
        );
        let unauthorized = avionics_options_handler(State(state), unknown_headers)
            .await
            .unwrap_err();
        assert_eq!(unauthorized.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn background_upload_survives_progress_disconnect() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();
        let public_key_base64 = BASE64_STANDARD.encode(key_pair.public_key().as_ref());
        let install = register_plugin_install(&db, &user, &public_key_base64)
            .await
            .unwrap();
        let source_url = "https://example.test/disconnected-progress";
        let rendered_html = "<html><body>aircraft listing</body></html>";
        let rendered_html_sha256 = sha256_hex(rendered_html.as_bytes());
        let message = signature_message(install.id, source_url, &rendered_html_sha256);
        let signature = key_pair.sign(&rng, message.as_bytes()).unwrap();
        let request = PluginSubmissionRequest {
            plugin_install_id: install.id,
            source_url: source_url.to_string(),
            rendered_html: rendered_html.to_string(),
            signature: BASE64_STANDARD.encode(signature.as_ref()),
        };
        let progress = start_plugin_submission_job(test_state(db.clone()), user.clone(), request);

        // Model the browser closing the extension popup immediately after the
        // server accepts the upload and returns its progress response.
        drop(progress);

        let mut completed = None;
        for _ in 0..100 {
            tokio::task::yield_now().await;
            let status = plugin_url_status(&db, &user, source_url).await.unwrap();
            if status.submission.is_some() {
                completed = Some(status);
                break;
            }
        }
        let completed = completed.expect("background upload should finish after disconnect");
        assert!(completed.submitted);
        assert!(completed
            .submission
            .and_then(|submission| submission.extraction_error)
            .is_some_and(|error| error.contains("GEMINI_API_KEY")));
    }
}
