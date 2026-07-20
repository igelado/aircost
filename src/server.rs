use anyhow::{Context, Result};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;

use crate::aircraft::{
    aircraft_listing_value, aircraft_options, aircraft_variant_detail, AircraftStoreError,
};
use crate::db::AppDb;
use crate::extract::{preview_listing_url, preview_manual_listing, GeminiListingExtractor};
use crate::listings::{
    create_listing, delete_listing, get_listing, list_listings, update_listing, ListingStoreError,
};
use crate::models::{
    ListingPreview, ListingUpdateRequest, PluginRegisterRequest, PluginSubmissionRequest,
    PreviewRequest, User,
};
use crate::plugin::{
    register_plugin_install, reprocess_plugin_submission, submit_plugin_html, PluginStoreError,
    PluginSubmissionOutcome,
};

pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub database_url: String,
}

#[derive(Clone)]
struct AppState {
    db: AppDb,
    extractor: Option<GeminiListingExtractor>,
}

#[derive(Debug, Deserialize)]
struct AircraftVariantQuery {
    annual_hours: Option<f64>,
}

pub async fn run_server(config: ServerConfig) -> Result<()> {
    let db = AppDb::connect(&config.database_url).await?;
    let state = AppState {
        db,
        extractor: GeminiListingExtractor::from_environment().ok(),
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
        .route("/health", get(health))
        .route("/api/users/current", get(current_user_handler))
        .route("/api/plugin/register", post(register_plugin_handler))
        .route("/api/plugin/submissions", post(plugin_submission_handler))
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

async fn health() -> Json<Value> {
    Json(json!({"ok": true}))
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
    let response = plugin_submission_response(&state.db, user, outcome).await;
    Ok((StatusCode::CREATED, Json(response)).into_response())
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
        plugin_submission_response(&state.db, user, outcome).await,
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
    let listing_estimate = aircraft_listing_value(&state.db, user_for_response.id, listing.id)
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
    let listing_estimate = aircraft_listing_value(&state.db, user_for_response.id, listing.id)
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
    let detail = aircraft_variant_detail(&state.db, user.id, variant_id, annual_hours)
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

async fn plugin_submission_response(
    db: &AppDb,
    user: User,
    outcome: PluginSubmissionOutcome,
) -> Value {
    let listing_estimate = match outcome.listing.as_ref() {
        Some(listing) => aircraft_listing_value(db, user.id, listing.id).await.ok(),
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
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status;
        let body = Json(json!({
            "error": {
                "message": self.message,
                "status": status.as_u16(),
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

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}"))
    }
}

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_CSS: &str = include_str!("../web/app.css");
const APP_JS: &str = include_str!("../web/app.js");
