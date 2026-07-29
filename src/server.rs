use anyhow::{Context, Result};
use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::{wrappers::UnboundedReceiverStream, StreamExt};
use tower_http::cors::CorsLayer;

use crate::aircraft::{
    aircraft_listing_value_with_model, aircraft_options, aircraft_variant_detail_with_model,
    AircraftStoreError,
};
use crate::avionics::catalog::{
    attest_grounded_existing_avionics_identity, attest_pending_review_product_identity,
    preview_avionics_identity, verify_approved_avionics_product_source_without_gemini,
    ApprovedAvionicsProductSourceRequest, ApprovedProductSourceVerificationOutcome,
    AvionicsIdentityOutcome, AvionicsIdentityRequest, CatalogError,
};
use crate::avionics::consolidation::{
    consolidate_avionics_models_with_human_review,
    preview_human_reviewed_avionics_model_consolidation, ConsolidationError,
    HumanReviewedAvionicsConsolidationRequest, HumanReviewedConsolidationProvenance,
};
use crate::avionics::inspection::{
    avionics_catalog_options, get_avionics_catalog_detail, list_avionics_catalog,
    AvionicsCatalogQuery, AvionicsInspectionError,
};
use crate::db::AppDb;
use crate::extract::{preview_listing_url, preview_manual_listing, GeminiListingExtractor};
use crate::gemini::source::ProductIdentityTarget;
use crate::listing::review::replacement::{
    approve_replacement_products_and_restage, ApproveReplacementProductsRequest,
};
use crate::listing::review::{
    active_collision_closure_revision_sha256, approve_locally_verified_ordinary_aspect_and_restage,
    corroborate_existing_product_association_and_restage, evaluate_existing_product_association,
    get_listing_review, list_listing_reviews, list_pending_product_associations,
    list_pending_product_reviews, preflight_listing_review_resolution,
    preflight_pending_product_attestation, resolve_listing_review, resolved_review_response,
    restage_unattested_preserved_products, use_existing_product_for_aspect_and_restage,
    ExistingProductAssociationCommit, ExistingProductAssociationEvaluation, ListingReview,
    ListingReviewDetail, ListingReviewQueue, PendingProductAssociationPage,
    PendingProductReviewPage, ProductReviewPageQuery, ResolveReviewRequest, ResolveReviewResponse,
    ReviewAspectId, ReviewDecision, ReviewError, ReviewQueueQuery, StagedPendingReview,
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
    valuation_model: Option<Arc<dyn ValuationModel>>,
    valuation_status: ServingValuationStatus,
}

#[derive(Debug, Deserialize)]
struct AircraftVariantQuery {
    annual_hours: Option<f64>,
}

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyExistingReviewAvionicsRequest {
    review_payload_sha256: String,
    catalog_revision_sha256: String,
    aspect_id: ReviewAspectId,
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

pub async fn run_server(config: ServerConfig) -> Result<()> {
    let db = AppDb::connect(&config.database_url).await?;
    let serving_valuation = load_serving_valuation(&db).await?;
    for warning in &serving_valuation.status.warnings {
        eprintln!("valuation warning: {warning}");
    }
    let extractor = GeminiListingExtractor::from_environment_with_usage(&db).ok();
    let state = AppState {
        db,
        extractor,
        valuation_model: serving_valuation.model,
        valuation_status: serving_valuation.status,
    };
    let app = router(state);
    let address = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("could not bind {address}"))?;

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
        .route("/api/avionics/{id}", get(avionics_detail_handler))
        .route("/api/review/listings", get(list_listing_reviews_handler))
        .route(
            "/api/review/avionics/products",
            get(list_pending_product_reviews_handler),
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
            "/api/review/listings/{id}/avionics/verify-existing",
            post(verify_existing_review_avionics_handler),
        )
        .route(
            "/api/review/listings/{id}/avionics/use-existing",
            post(use_existing_review_avionics_handler),
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
        return Ok(Json(json!({
            "product_id": product_id,
            "attestation_status": "current",
            "reused": true
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
    Ok(Json(json!({
        "product_id": product_id,
        "attestation_status": "current",
        "reused": false
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

async fn review_maintenance_response(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    staged: Option<StagedPendingReview>,
) -> Result<Json<Value>, ApiError> {
    match staged {
        Some(_) => {
            let detail = get_listing_review(db, owner_user_id, listing_id).await?;
            Ok(Json(json!({
                "review": detail.review,
                "review_complete": false
            })))
        }
        None => Ok(Json(json!({
            "review": Value::Null,
            "review_complete": true
        }))),
    }
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
        active_collision_closure_revision_sha256(&state.db, target_id).await?;

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

/// Preview or apply an explicit, evidence-backed consolidation of the exact
/// unreviewed catalog collision blocking one listing-review aspect.
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
    let proposed_manufacturer_key = normalize_avionics_manufacturer_name(&proposed.manufacturer);
    let proposed_model_key = normalize_avionics_model_name(&proposed.model);
    if proposed_model_key != preview.authorization.canonical_model_key
        || preview.authorization.members.iter().all(|member| {
            normalize_avionics_manufacturer_name(&member.manufacturer) != proposed_manufacturer_key
        })
    {
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
    // publication race around network enrichment.
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
        finalize_reviewed_listing_ingestion(&state.db, listing_id, state.extractor.as_ref(), None)
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
                identity_evidence_text.clone(),
            ],
            manufacturer: manufacturer.clone(),
            model: model.clone(),
            avionics_types: capabilities.clone(),
            quantity: aspect.quantity.max(1),
        };
        let outcome = preview_avionics_identity(&state.db, extractor, &request)
            .await
            .map_err(|error| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    format!("could not ground proposed avionics identity: {error}"),
                )
                .with_code("avionics_grounding_failed")
            })?;
        let approved = match outcome {
            AvionicsIdentityOutcome::Approved(approved) => approved,
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

        // Persist exactly the independently grounded canonical identity and
        // evidence, never unchecked reviewer-entered catalog fields.
        *manufacturer = approved.manufacturer;
        *model = approved.model;
        *capabilities = approved.avionics_types;
        *manufacturer_identifier_kind = approved.manufacturer_identifier_kind;
        *manufacturer_identifier = approved.manufacturer_identifier;
        *identity_source_url = approved.evidence_url;
        *identity_source_title = approved.evidence_title;
        *identity_evidence_text = approved.evidence;
        *grounded_claim_source_urls = approved.grounded_claim_source_urls;
    }
    Ok(())
}

async fn aircraft_variant_detail_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(variant_id): Path<i64>,
    Query(query): Query<AircraftVariantQuery>,
) -> Result<Json<Value>, ApiError> {
    let user = load_current_user(&state.db, &headers).await?;
    let annual_hours = match query.annual_hours {
        Some(value) if value.is_finite() && (0.0..=2_000.0).contains(&value) => Some(value),
        Some(_) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "annual_hours must be between 0 and 2000".to_string(),
            ));
        }
        None => None,
    };
    let detail = aircraft_variant_detail_with_model(
        &state.db,
        user.id,
        variant_id,
        annual_hours,
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
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: None,
        }
    }

    fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status;
        let body = Json(json!({
            "error": {
                "message": self.message,
                "status": status.as_u16(),
                "code": self.code,
            }
        }));
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
            PluginStoreError::Permission(message) => ApiError::new(StatusCode::FORBIDDEN, message),
            PluginStoreError::NotFound(message) => ApiError::new(StatusCode::NOT_FOUND, message),
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

#[cfg(test)]
mod tests {
    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::Json;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine as _;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use serde_json::json;
    use sqlx::SqlitePool;

    use super::{
        approve_replacement_products_handler, attest_review_avionics_product_handler,
        avionics_options_handler, get_listing_review, list_avionics_handler,
        require_current_review_revisions, start_plugin_submission_job,
        use_existing_review_avionics_handler, verify_existing_review_avionics_handler, AppState,
        AttestReviewAvionicsProductRequest, UseExistingReviewAvionicsRequest,
        VerifyExistingReviewAvionicsRequest,
    };
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
        restage_unattested_preserved_products, stage_pending_review, ListingReview,
        PendingReviewAspect, ResolveReviewRequest, ReviewAircraftIdentityState,
        ReviewAircraftIdentityStatus, ReviewAircraftSummary, ReviewAspectId,
    };
    use crate::models::PluginSubmissionRequest;
    use crate::normalize::{
        normalize_avionics_identifier, normalize_avionics_manufacturer_name,
        normalize_avionics_model_name, normalize_name,
    };
    use crate::plugin::{
        plugin_url_status, register_plugin_install, sha256_hex, signature_message,
    };
    use crate::valuation::store::{ServingValuationState, ServingValuationStatus};

    fn test_state(db: AppDb) -> AppState {
        AppState {
            db,
            extractor: None,
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

    fn sqlite_pool(db: &AppDb) -> &SqlitePool {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("server test database is not SQLite");
        };
        pool
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
            ) VALUES (?, ?, 'https://broker.example/aircraft/server-review', 2020, 450000, 900)
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
        let rendered_html_sha256 = sha256_hex(rendered_html.as_bytes());
        sqlx::query_scalar(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, canonical_listing_id
            ) VALUES (?, ?, 'https://broker.example/aircraft/server-review', ?, ?,
                      'test-signature', ?)
            RETURNING id
            "#,
        )
        .bind(owner_user_id)
        .bind(install_id)
        .bind(rendered_html)
        .bind(rendered_html_sha256)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap()
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
        assert!(error
            .message
            .contains("exact structurally visible-body span"));

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
