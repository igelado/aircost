use std::fmt;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use ring::digest;
use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};
use serde_json::{json, Value};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{parse_listing_html, validate_source_url, GeminiListingExtractor};
use crate::listings::{create_listing, ListingStoreError};
use crate::models::{
    ListingPreview, PluginInstall, PluginSubmission, PluginSubmissionRequest, SaleListing, User,
};

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
            ListingStoreError::NotFound(message) => PluginStoreError::NotFound(message),
            ListingStoreError::Permission(message) => PluginStoreError::Permission(message),
            ListingStoreError::Database(message) => PluginStoreError::Database(message),
        }
    }
}

type StoreResult<T> = Result<T, PluginStoreError>;

#[derive(Debug)]
pub struct PluginSubmissionOutcome {
    pub submission: PluginSubmission,
    pub preview: Option<ListingPreview>,
    pub listing: Option<SaleListing>,
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

    if let Some(extractor) = extractor {
        match parse_listing_html(&request.source_url, &request.rendered_html, extractor).await {
            Ok(parsed_preview) => {
                extracted_listing_json = Some(json!(parsed_preview.parsed_listing));
                match create_listing(db, user.id, &parsed_preview, None, Some(extractor)).await {
                    Ok(created_listing) => {
                        canonical_listing_id = Some(created_listing.id);
                        listing = Some(created_listing);
                    }
                    Err(error) => {
                        extraction_error = Some(error.to_string());
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

    Ok(PluginSubmissionOutcome {
        submission,
        preview,
        listing,
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
    let mut canonical_listing_id = None;

    if let Some(extractor) = extractor {
        match parse_listing_html(&stored.source_url, &stored.rendered_html, extractor).await {
            Ok(parsed_preview) => {
                extracted_listing_json = Some(json!(parsed_preview.parsed_listing));
                match create_listing(db, user.id, &parsed_preview, None, Some(extractor)).await {
                    Ok(created_listing) => {
                        canonical_listing_id = Some(created_listing.id);
                        listing = Some(created_listing);
                    }
                    Err(error) => {
                        extraction_error = Some(error.to_string());
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

    let submission = update_plugin_submission_result(
        db,
        user.id,
        stored.id,
        extracted_listing_json.as_ref(),
        extraction_error.as_deref(),
        canonical_listing_id,
    )
    .await?;

    Ok(PluginSubmissionOutcome {
        submission,
        preview,
        listing,
    })
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
          rendered_html
        FROM plugin_submissions
        WHERE id = ? AND user_id = ?
        "#,
        submission_id,
        user_id
    )?;
    row.ok_or_else(|| PluginStoreError::NotFound("plugin submission not found".to_string()))
}

fn verify_submission_signature(
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
          extracted_listing_json = ?,
          extraction_error = ?,
          canonical_listing_id = ?
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

    use super::{sha256_hex, signature_message, verify_submission_signature};

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
}
