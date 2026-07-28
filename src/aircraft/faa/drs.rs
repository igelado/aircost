//! Bounded access to current FAA Dynamic Regulatory System TCDS documents.
//!
//! This module validates DRS metadata before following its document UUID to a
//! PDF. It returns regulator text and provenance, but deliberately does not
//! interpret model families or serial-number applicability.

use std::fmt;
use std::time::Duration;

use lopdf::{Document, LoadOptions};
use reqwest::header::{HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::{redirect::Policy, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const PRODUCTION_BASE_URL: &str = "https://drs.faa.gov";
const METADATA_PATH: &str = "/api/drs/data-pull/TCDSMODEL/filtered";
const DOWNLOAD_PATH_PREFIX: &str = "/api/drs/data-pull/download/";
const API_KEY_HEADER: &str = "x-api-key";

const MAX_INPUT_BYTES: usize = 128;
const MAX_METADATA_VALUE_BYTES: usize = 4_096;
const HARD_MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;
const HARD_MAX_PDF_BYTES: usize = 32 * 1024 * 1024;
const HARD_MAX_PAGES: usize = 512;
const HARD_MAX_PAGE_DECOMPRESSED_BYTES: usize = 8 * 1024 * 1024;
const HARD_MAX_TOTAL_TEXT_BYTES: usize = 8 * 1024 * 1024;
const HARD_MAX_MODEL_BLOCK_BYTES: usize = 128 * 1024;
const HARD_MAX_MODEL_BLOCKS: usize = 32;

/// Resource bounds applied before and while parsing untrusted DRS responses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DrsLimits {
    pub max_metadata_bytes: usize,
    pub max_pdf_bytes: usize,
    pub max_pages: usize,
    pub max_page_decompressed_bytes: usize,
    pub max_total_text_bytes: usize,
    pub max_model_block_bytes: usize,
    pub max_model_blocks: usize,
}

impl Default for DrsLimits {
    fn default() -> Self {
        Self {
            max_metadata_bytes: 1024 * 1024,
            max_pdf_bytes: 16 * 1024 * 1024,
            max_pages: 128,
            max_page_decompressed_bytes: 2 * 1024 * 1024,
            max_total_text_bytes: 2 * 1024 * 1024,
            max_model_block_bytes: 64 * 1024,
            max_model_blocks: 8,
        }
    }
}

impl DrsLimits {
    fn validate(&self) -> Result<(), DrsError> {
        let values = [
            (self.max_metadata_bytes, HARD_MAX_METADATA_BYTES),
            (self.max_pdf_bytes, HARD_MAX_PDF_BYTES),
            (self.max_pages, HARD_MAX_PAGES),
            (
                self.max_page_decompressed_bytes,
                HARD_MAX_PAGE_DECOMPRESSED_BYTES,
            ),
            (self.max_total_text_bytes, HARD_MAX_TOTAL_TEXT_BYTES),
            (self.max_model_block_bytes, HARD_MAX_MODEL_BLOCK_BYTES),
            (self.max_model_blocks, HARD_MAX_MODEL_BLOCKS),
        ];
        if values
            .into_iter()
            .any(|(configured, hard_max)| configured == 0 || configured > hard_max)
        {
            return Err(DrsError::InvalidLimits);
        }
        Ok(())
    }
}

/// Validated metadata for exactly one current FAA TCDS document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CurrentTcdsMetadata {
    pub document_guid: String,
    pub document_url: String,
    pub tcds_number: String,
    pub revision_number: Option<String>,
    pub revision_date: Option<String>,
    pub tc_holder: Option<String>,
    pub former_tc_holders: Vec<String>,
    pub models: Vec<String>,
    pub exact_model: String,
}

/// Text extracted from one physical PDF page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TcdsPageText {
    pub page_number: u32,
    pub text: String,
}

/// A literal model-heading-to-serial-label span from one page.
///
/// The byte offsets refer to [`TcdsPageText::text`]. The block is intentionally
/// raw: callers must not treat it as a parsed serial range.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExactModelTextBlock {
    pub page_number: u32,
    pub start_byte: usize,
    pub end_byte: usize,
    pub raw_text: String,
}

/// One bounded, digest-addressed current TCDS PDF and its extracted text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TcdsDocument {
    pub metadata: CurrentTcdsMetadata,
    pub source_url: String,
    pub pdf_sha256: String,
    pub pdf_size_bytes: usize,
    pub page_count: usize,
    pub pages: Vec<TcdsPageText>,
    pub exact_model_blocks: Vec<ExactModelTextBlock>,
}

/// Fail-closed DRS transport and validation errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DrsError {
    InvalidLimits,
    InvalidApiKey,
    InvalidInput(&'static str),
    ClientInitialization,
    MetadataRequestFailed,
    MetadataRejected(u16),
    MetadataContentTypeInvalid,
    MetadataTooLarge,
    MetadataJsonInvalid,
    MetadataNotUnique { total: usize, returned: usize },
    MetadataInvalid(&'static str),
    RequestedTcdsMismatch,
    RequestedModelMismatch,
    PdfRequestFailed,
    PdfRejected(u16),
    PdfContentTypeInvalid,
    PdfTooLarge,
    PdfDigestMismatch,
    PdfInvalid,
    PdfEncrypted,
    PdfPageLimitExceeded,
    PdfTextLimitExceeded,
    PdfTextExtractionFailed,
    ModelBlockLimitExceeded,
}

impl fmt::Display for DrsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => formatter.write_str("FAA DRS limits are invalid"),
            Self::InvalidApiKey => formatter.write_str("FAA DRS API key is invalid"),
            Self::InvalidInput(field) => write!(formatter, "FAA DRS {field} is invalid"),
            Self::ClientInitialization => {
                formatter.write_str("FAA DRS client initialization failed")
            }
            Self::MetadataRequestFailed => formatter.write_str("FAA DRS metadata request failed"),
            Self::MetadataRejected(status) => {
                write!(formatter, "FAA DRS metadata request returned HTTP {status}")
            }
            Self::MetadataContentTypeInvalid => {
                formatter.write_str("FAA DRS metadata content type is invalid")
            }
            Self::MetadataTooLarge => {
                formatter.write_str("FAA DRS metadata exceeded its size limit")
            }
            Self::MetadataJsonInvalid => formatter.write_str("FAA DRS metadata JSON is invalid"),
            Self::MetadataNotUnique { total, returned } => write!(
                formatter,
                "FAA DRS metadata was not unique (total {total}, returned {returned})"
            ),
            Self::MetadataInvalid(field) => {
                write!(formatter, "FAA DRS metadata field {field} is invalid")
            }
            Self::RequestedTcdsMismatch => {
                formatter.write_str("FAA DRS metadata did not match the exact requested TCDS")
            }
            Self::RequestedModelMismatch => formatter
                .write_str("FAA DRS metadata did not contain the exact requested model once"),
            Self::PdfRequestFailed => formatter.write_str("FAA DRS PDF request failed"),
            Self::PdfRejected(status) => {
                write!(formatter, "FAA DRS PDF request returned HTTP {status}")
            }
            Self::PdfContentTypeInvalid => {
                formatter.write_str("FAA DRS PDF content type is invalid")
            }
            Self::PdfTooLarge => formatter.write_str("FAA DRS PDF exceeded its size limit"),
            Self::PdfDigestMismatch => {
                formatter.write_str("FAA DRS PDF did not match the operator-supplied SHA-256")
            }
            Self::PdfInvalid => formatter.write_str("FAA DRS PDF is invalid"),
            Self::PdfEncrypted => formatter.write_str("FAA DRS PDF is encrypted"),
            Self::PdfPageLimitExceeded => {
                formatter.write_str("FAA DRS PDF exceeded its page limit")
            }
            Self::PdfTextLimitExceeded => {
                formatter.write_str("FAA DRS PDF text exceeded its size limit")
            }
            Self::PdfTextExtractionFailed => {
                formatter.write_str("FAA DRS PDF text extraction failed")
            }
            Self::ModelBlockLimitExceeded => {
                formatter.write_str("FAA DRS exact-model evidence exceeded its limit")
            }
        }
    }
}

impl std::error::Error for DrsError {}

/// Parse a current FAA DRS PDF supplied explicitly to an administrator.
///
/// This is the offline counterpart to [`DrsClient`], intended for one-time
/// catalog migrations when the operator has already obtained the exact
/// official PDF but the runtime has no DRS API key. It is never selected by
/// the web server or by environment fallback. The caller must provide the
/// expected digest and official DRS metadata, and every production-origin,
/// exact-model, size, PDF, and text bound is revalidated here.
pub fn parse_operator_supplied_current_tcds(
    metadata: CurrentTcdsMetadata,
    source_url: impl Into<String>,
    expected_pdf_sha256: &str,
    pdf: Vec<u8>,
) -> Result<TcdsDocument, DrsError> {
    let limits = DrsLimits::default();
    limits.validate()?;
    validate_operator_metadata(&metadata)?;
    if expected_pdf_sha256.len() != 64
        || !expected_pdf_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DrsError::InvalidInput("expected PDF SHA-256"));
    }
    if pdf.len() > limits.max_pdf_bytes {
        return Err(DrsError::PdfTooLarge);
    }
    let source_url = source_url.into();
    let validated_source_url =
        validate_download_url(&source_url, &metadata.document_guid, PRODUCTION_BASE_URL)?;
    let document = parse_tcds_pdf(metadata, validated_source_url, pdf, &limits)?;
    if document.pdf_sha256 != expected_pdf_sha256 {
        return Err(DrsError::PdfDigestMismatch);
    }
    Ok(document)
}

fn validate_operator_metadata(metadata: &CurrentTcdsMetadata) -> Result<(), DrsError> {
    if !valid_uuid(&metadata.document_guid) {
        return Err(DrsError::MetadataInvalid("documentGuid"));
    }
    let document_url = url::Url::parse(&metadata.document_url)
        .map_err(|_| DrsError::MetadataInvalid("documentURL"))?;
    if document_url.scheme() != "https"
        || document_url.host_str() != Some("drs.faa.gov")
        || document_url.port().is_some()
        || !document_url.username().is_empty()
        || document_url.password().is_some()
        || document_url.query().is_some()
        || document_url.fragment().is_some()
        || document_url.path() == "/"
    {
        return Err(DrsError::MetadataInvalid("documentURL"));
    }
    validate_input(&metadata.tcds_number, "TCDS number")?;
    validate_input(&metadata.exact_model, "model")?;
    if metadata
        .models
        .iter()
        .filter(|model| model.as_str() == metadata.exact_model)
        .count()
        != 1
        || metadata
            .models
            .iter()
            .any(|model| validate_input(model, "model").is_err())
    {
        return Err(DrsError::RequestedModelMismatch);
    }
    for value in [
        metadata.revision_number.as_deref(),
        metadata.revision_date.as_deref(),
        metadata.tc_holder.as_deref(),
    ]
    .into_iter()
    .flatten()
    .chain(metadata.former_tc_holders.iter().map(String::as_str))
    {
        validate_metadata_value(value, false)
            .map_err(|_| DrsError::MetadataInvalid("operator metadata"))?;
    }
    Ok(())
}

/// Client for the fixed FAA DRS production origin.
#[derive(Clone)]
pub struct DrsClient {
    client: reqwest::Client,
    api_key: HeaderValue,
    limits: DrsLimits,
    base_url: String,
}

impl DrsClient {
    pub fn new(api_key: impl AsRef<str>) -> Result<Self, DrsError> {
        Self::with_limits(api_key, DrsLimits::default())
    }

    pub fn with_limits(api_key: impl AsRef<str>, limits: DrsLimits) -> Result<Self, DrsError> {
        Self::build(PRODUCTION_BASE_URL.to_string(), api_key.as_ref(), limits)
    }

    fn build(base_url: String, api_key: &str, limits: DrsLimits) -> Result<Self, DrsError> {
        limits.validate()?;
        if api_key.is_empty() || api_key.trim() != api_key || api_key.len() > 4_096 {
            return Err(DrsError::InvalidApiKey);
        }
        let mut api_key = HeaderValue::from_str(api_key).map_err(|_| DrsError::InvalidApiKey)?;
        api_key.set_sensitive(true);
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .user_agent("aircost-rs/0.1 FAA-DRS")
            .build()
            .map_err(|_| DrsError::ClientInitialization)?;
        Ok(Self {
            client,
            api_key,
            limits,
            base_url,
        })
    }

    /// Fetch the unique current TCDS matching both an exact TCDS number and
    /// an exact model token.
    pub async fn fetch_current_tcds(
        &self,
        tcds_number: &str,
        exact_model: &str,
    ) -> Result<TcdsDocument, DrsError> {
        validate_input(tcds_number, "TCDS number")?;
        validate_input(exact_model, "model")?;
        self.fetch(
            MetadataQuery::TcdsNumber(tcds_number),
            Some(tcds_number),
            exact_model,
        )
        .await
    }

    /// Fetch the unique current TCDS containing an exact model token.
    ///
    /// This path is intended for ACFTREF observations whose TCDS field is
    /// blank. Ambiguous model searches fail instead of selecting a document.
    pub async fn fetch_unique_current_tcds_for_model(
        &self,
        exact_model: &str,
    ) -> Result<TcdsDocument, DrsError> {
        validate_input(exact_model, "model")?;
        self.fetch(MetadataQuery::Model(exact_model), None, exact_model)
            .await
    }

    async fn fetch(
        &self,
        query: MetadataQuery<'_>,
        expected_tcds: Option<&str>,
        exact_model: &str,
    ) -> Result<TcdsDocument, DrsError> {
        let metadata = self
            .fetch_metadata(query, expected_tcds, exact_model)
            .await?;
        let source_url = metadata.download_url.clone();
        let pdf = self.download_pdf(&source_url).await?;
        parse_tcds_pdf(metadata.into_public(), source_url, pdf, &self.limits)
    }

    async fn fetch_metadata(
        &self,
        query: MetadataQuery<'_>,
        expected_tcds: Option<&str>,
        exact_model: &str,
    ) -> Result<ValidatedMetadata, DrsError> {
        let body = match query {
            MetadataQuery::TcdsNumber(value) => serde_json::json!({
                "offset": 0,
                "sortOrder": "DESC",
                "documentFilters": {
                    "drs:status": ["Current"],
                    "drs:tcdsmodelModel": [exact_model],
                    "drs:documentNumber": [value]
                },
            }),
            MetadataQuery::Model(value) => serde_json::json!({
                "offset": 0,
                "sortOrder": "DESC",
                "documentFilters": {
                    "drs:status": ["Current"],
                    "drs:tcdsmodelModel": [value]
                },
            }),
        };
        let response = self
            .client
            .post(format!("{}{}", self.base_url, METADATA_PATH))
            .header(API_KEY_HEADER, self.api_key.clone())
            .json(&body)
            .send()
            .await
            .map_err(|_| DrsError::MetadataRequestFailed)?;
        if response.status() != StatusCode::OK {
            return Err(DrsError::MetadataRejected(response.status().as_u16()));
        }
        if !content_type_is(response.headers(), "application/json") {
            return Err(DrsError::MetadataContentTypeInvalid);
        }
        let bytes = read_limited(
            response,
            self.limits.max_metadata_bytes,
            DrsError::MetadataTooLarge,
            DrsError::MetadataRequestFailed,
        )
        .await?;
        let response: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| DrsError::MetadataJsonInvalid)?;
        validate_metadata(response, expected_tcds, exact_model, &self.base_url)
    }

    async fn download_pdf(&self, source_url: &str) -> Result<Vec<u8>, DrsError> {
        let response = self
            .client
            .get(source_url)
            .header(API_KEY_HEADER, self.api_key.clone())
            .send()
            .await
            .map_err(|_| DrsError::PdfRequestFailed)?;
        if response.status() != StatusCode::OK {
            return Err(DrsError::PdfRejected(response.status().as_u16()));
        }
        if !content_type_is(response.headers(), "application/pdf") {
            return Err(DrsError::PdfContentTypeInvalid);
        }
        read_limited(
            response,
            self.limits.max_pdf_bytes,
            DrsError::PdfTooLarge,
            DrsError::PdfRequestFailed,
        )
        .await
    }

    #[cfg(test)]
    fn for_test(base_url: String, api_key: &str, limits: DrsLimits) -> Result<Self, DrsError> {
        Self::build(base_url, api_key, limits)
    }
}

enum MetadataQuery<'a> {
    TcdsNumber(&'a str),
    Model(&'a str),
}

struct ValidatedMetadata {
    public: CurrentTcdsMetadata,
    download_url: String,
}

fn validate_metadata(
    response: serde_json::Value,
    expected_tcds: Option<&str>,
    exact_model: &str,
    base_url: &str,
) -> Result<ValidatedMetadata, DrsError> {
    let root = response
        .as_object()
        .ok_or(DrsError::MetadataInvalid("response"))?;
    let summary = object_required(root, "summary")?;
    if string_required(summary, "drsDoctypeName")? != "TCDSMODEL" {
        return Err(DrsError::MetadataInvalid("summary.drsDoctypeName"));
    }
    validate_metadata_value(string_required(summary, "doctypeName")?, false)
        .map_err(|_| DrsError::MetadataInvalid("summary.doctypeName"))?;
    let count = unsigned_required(summary, "count")?;
    let total = unsigned_required(summary, "totalItems")?;
    let offset = unsigned_required(summary, "offset")?;
    let has_more = bool_required(summary, "hasMoreItems")?;
    if count != 1 || total != 1 || offset != 0 || has_more {
        let returned = root
            .get("documents")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        return Err(DrsError::MetadataNotUnique {
            total: usize::try_from(total).unwrap_or(usize::MAX),
            returned,
        });
    }
    if string_required(summary, "sortByOrder")? != "DESC" {
        return Err(DrsError::MetadataInvalid("summary.sortByOrder"));
    }
    validate_metadata_value(string_required(summary, "sortBy")?, false)
        .map_err(|_| DrsError::MetadataInvalid("summary.sortBy"))?;

    let documents = root
        .get("documents")
        .and_then(serde_json::Value::as_array)
        .ok_or(DrsError::MetadataInvalid("documents"))?;
    if documents.len() != 1 {
        return Err(DrsError::MetadataNotUnique {
            total: 1,
            returned: documents.len(),
        });
    }
    let document = documents[0]
        .as_object()
        .ok_or(DrsError::MetadataInvalid("documents[0]"))?;

    let document_guid = string_required(document, "documentGuid")?;
    if !valid_uuid(document_guid) {
        return Err(DrsError::MetadataInvalid("documentGuid"));
    }
    let document_url = string_required(document, "documentURL")?;
    validate_metadata_value(document_url, false)
        .map_err(|_| DrsError::MetadataInvalid("documentURL"))?;
    let main_document_file_name = string_required(document, "mainDocumentFileName")?;
    validate_metadata_value(main_document_file_name, false)
        .map_err(|_| DrsError::MetadataInvalid("mainDocumentFileName"))?;
    if !main_document_file_name
        .to_ascii_lowercase()
        .ends_with(".pdf")
    {
        return Err(DrsError::MetadataInvalid("mainDocumentFileName"));
    }

    let status = required_text_values(document, "drs:status", false)?;
    if status.as_slice() != ["Current"] {
        return Err(DrsError::MetadataInvalid("drs:status"));
    }
    let tcds_numbers = required_text_values(document, "drs:documentNumber", false)?;
    if tcds_numbers.len() != 1 {
        return Err(DrsError::MetadataInvalid("drs:documentNumber"));
    }
    let tcds_number = &tcds_numbers[0];
    validate_input(tcds_number, "TCDS number")
        .map_err(|_| DrsError::MetadataInvalid("drs:documentNumber"))?;
    if expected_tcds.is_some_and(|expected| expected != tcds_number) {
        return Err(DrsError::RequestedTcdsMismatch);
    }

    let models = required_text_values(document, "drs:tcdsmodelModel", true)?;
    if models
        .iter()
        .filter(|model| model.as_str() == exact_model)
        .count()
        != 1
    {
        return Err(DrsError::RequestedModelMismatch);
    }

    let tc_holder = optional_single_text(document, "drs:tcdsmodelTCHolder")?;
    let former_tc_holders = optional_text_values(document, "drs:tcdsmodelFormerHolders", false)?;
    let revision_number = optional_single_text(document, "drs:tcdsmodelRevisionNumber")?;
    let revision_date = optional_single_text(document, "drs:tcdsmodelRevisionDate")?;
    let raw_download_url = string_required(document, "mainDocumentDownloadURL")?;
    let download_url = validate_download_url(raw_download_url, document_guid, base_url)?;

    Ok(ValidatedMetadata {
        public: CurrentTcdsMetadata {
            document_guid: document_guid.to_string(),
            document_url: document_url.to_string(),
            tcds_number: tcds_number.to_string(),
            revision_number,
            revision_date,
            tc_holder,
            former_tc_holders,
            models,
            exact_model: exact_model.to_string(),
        },
        download_url,
    })
}

impl ValidatedMetadata {
    fn into_public(self) -> CurrentTcdsMetadata {
        self.public
    }
}

fn object_required<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    name: &'static str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, DrsError> {
    object
        .get(name)
        .and_then(serde_json::Value::as_object)
        .ok_or(DrsError::MetadataInvalid(name))
}

fn string_required<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    name: &'static str,
) -> Result<&'a str, DrsError> {
    object
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or(DrsError::MetadataInvalid(name))
}

fn unsigned_required(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &'static str,
) -> Result<u64, DrsError> {
    object
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .ok_or(DrsError::MetadataInvalid(name))
}

fn bool_required(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &'static str,
) -> Result<bool, DrsError> {
    object
        .get(name)
        .and_then(serde_json::Value::as_bool)
        .ok_or(DrsError::MetadataInvalid(name))
}

fn required_text_values(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &'static str,
    split_pipe_scalar: bool,
) -> Result<Vec<String>, DrsError> {
    let values = object.get(name).ok_or(DrsError::MetadataInvalid(name))?;
    parse_text_values(values, name, split_pipe_scalar, false)
}

fn optional_text_values(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &'static str,
    split_pipe_scalar: bool,
) -> Result<Vec<String>, DrsError> {
    let Some(values) = object.get(name) else {
        return Ok(Vec::new());
    };
    parse_text_values(values, name, split_pipe_scalar, true)
}

fn optional_single_text(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &'static str,
) -> Result<Option<String>, DrsError> {
    let values = optional_text_values(object, name, false)?;
    if values.len() > 1 {
        return Err(DrsError::MetadataInvalid(name));
    }
    Ok(values.into_iter().next())
}

fn parse_text_values(
    value: &serde_json::Value,
    name: &'static str,
    split_pipe_scalar: bool,
    allow_empty: bool,
) -> Result<Vec<String>, DrsError> {
    let raw_values: Vec<&str> = match value {
        serde_json::Value::String(value) if split_pipe_scalar => value.split('|').collect(),
        serde_json::Value::String(value) => vec![value],
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| value.as_str().ok_or(DrsError::MetadataInvalid(name)))
            .collect::<Result<_, _>>()?,
        _ => return Err(DrsError::MetadataInvalid(name)),
    };
    let mut result = Vec::new();
    for raw in raw_values {
        let value = raw.trim();
        if value.is_empty() && allow_empty {
            continue;
        }
        validate_metadata_value(value, false).map_err(|_| DrsError::MetadataInvalid(name))?;
        if !value.is_ascii() || value.len() > MAX_INPUT_BYTES {
            return Err(DrsError::MetadataInvalid(name));
        }
        result.push(value.to_string());
    }
    if result.is_empty() && !allow_empty {
        return Err(DrsError::MetadataInvalid(name));
    }
    Ok(result)
}

fn validate_download_url(
    value: &str,
    document_guid: &str,
    base_url: &str,
) -> Result<String, DrsError> {
    let url =
        url::Url::parse(value).map_err(|_| DrsError::MetadataInvalid("mainDocumentDownloadURL"))?;
    let base = url::Url::parse(base_url)
        .map_err(|_| DrsError::MetadataInvalid("mainDocumentDownloadURL"))?;
    if url.scheme() != base.scheme()
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != format!("{DOWNLOAD_PATH_PREFIX}{document_guid}")
    {
        return Err(DrsError::MetadataInvalid("mainDocumentDownloadURL"));
    }
    if base_url == PRODUCTION_BASE_URL
        && (url.scheme() != "https"
            || url.host_str() != Some("drs.faa.gov")
            || url.port().is_some())
    {
        return Err(DrsError::MetadataInvalid("mainDocumentDownloadURL"));
    }
    Ok(url.to_string())
}

fn validate_metadata_value(value: &str, allow_empty: bool) -> Result<(), ()> {
    if value.len() > MAX_METADATA_VALUE_BYTES
        || (!allow_empty && value.is_empty())
        || value.chars().any(char::is_control)
    {
        return Err(());
    }
    Ok(())
}

fn validate_input(value: &str, field: &'static str) -> Result<(), DrsError> {
    if value.is_empty()
        || value.len() > MAX_INPUT_BYTES
        || value.trim() != value
        || !value.is_ascii()
        || value.chars().any(char::is_control)
    {
        return Err(DrsError::InvalidInput(field));
    }
    Ok(())
}

fn valid_uuid(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }
    value.bytes().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => byte == b'-',
        _ => byte.is_ascii_hexdigit(),
    })
}

fn content_type_is(headers: &reqwest::header::HeaderMap, expected: &str) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
    too_large: DrsError,
    transport_error: DrsError,
) -> Result<Vec<u8>, DrsError> {
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > limit as u64)
    {
        return Err(too_large);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| transport_error.clone())?
    {
        let new_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| too_large.clone())?;
        if new_len > limit {
            return Err(too_large);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn parse_tcds_pdf(
    metadata: CurrentTcdsMetadata,
    source_url: String,
    pdf: Vec<u8>,
    limits: &DrsLimits,
) -> Result<TcdsDocument, DrsError> {
    if !pdf.starts_with(b"%PDF-") {
        return Err(DrsError::PdfInvalid);
    }
    let pdf_sha256 = format!("{:x}", Sha256::digest(&pdf));
    let pdf_size_bytes = pdf.len();
    let document = Document::load_mem_with_options(
        &pdf,
        LoadOptions {
            strict: true,
            max_decompressed_size: Some(limits.max_page_decompressed_bytes),
            ..LoadOptions::default()
        },
    )
    .map_err(|_| DrsError::PdfInvalid)?;
    if document.is_encrypted() || document.was_encrypted() {
        return Err(DrsError::PdfEncrypted);
    }
    let page_map = document.get_pages();
    if page_map.is_empty() || page_map.len() > limits.max_pages {
        return Err(DrsError::PdfPageLimitExceeded);
    }

    let mut pages = Vec::with_capacity(page_map.len());
    let mut total_text_bytes = 0usize;
    for page_number in page_map.keys().copied() {
        let text = document
            .extract_text_with_limit(&[page_number], limits.max_page_decompressed_bytes)
            .map_err(|_| DrsError::PdfTextExtractionFailed)?;
        total_text_bytes = total_text_bytes
            .checked_add(text.len())
            .ok_or(DrsError::PdfTextLimitExceeded)?;
        if total_text_bytes > limits.max_total_text_bytes {
            return Err(DrsError::PdfTextLimitExceeded);
        }
        pages.push(TcdsPageText { page_number, text });
    }
    let exact_model_blocks = extract_exact_model_blocks(&pages, &metadata.exact_model, limits)?;

    Ok(TcdsDocument {
        metadata,
        source_url,
        pdf_sha256,
        pdf_size_bytes,
        page_count: pages.len(),
        pages,
        exact_model_blocks,
    })
}

fn extract_exact_model_blocks(
    pages: &[TcdsPageText],
    exact_model: &str,
    limits: &DrsLimits,
) -> Result<Vec<ExactModelTextBlock>, DrsError> {
    let mut blocks = Vec::new();
    for page in pages {
        let lines = line_spans(&page.text);
        for (heading_index, heading) in lines.iter().enumerate() {
            if !is_exact_model_heading(heading.text, exact_model) {
                continue;
            }
            let Some(serial_index) = lines
                .iter()
                .enumerate()
                .skip(heading_index + 1)
                .find_map(|(index, line)| is_serial_eligibility_marker(line.text).then_some(index))
            else {
                continue;
            };
            let mut end_index = serial_index;
            for index in (serial_index + 1)..lines.len().min(serial_index + 17) {
                if lines[index].text.trim().is_empty() {
                    break;
                }
                end_index = index;
            }
            let start_byte = heading.start;
            let end_byte = lines[end_index].end;
            if end_byte.saturating_sub(start_byte) > limits.max_model_block_bytes {
                return Err(DrsError::ModelBlockLimitExceeded);
            }
            blocks.push(ExactModelTextBlock {
                page_number: page.page_number,
                start_byte,
                end_byte,
                raw_text: page.text[start_byte..end_byte].to_string(),
            });
            if blocks.len() > limits.max_model_blocks {
                return Err(DrsError::ModelBlockLimitExceeded);
            }
        }
    }
    Ok(blocks)
}

fn is_serial_eligibility_marker(line: &str) -> bool {
    ["Serial Numbers Eligible", "Serial Nos. Eligible"]
        .into_iter()
        .any(|marker| exact_token_positions(line, marker).next().is_some())
}

struct LineSpan<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

fn line_spans(text: &str) -> Vec<LineSpan<'_>> {
    let mut result = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        result.push(LineSpan {
            start,
            end,
            text: line,
        });
        start = end;
    }
    if start < text.len() {
        result.push(LineSpan {
            start,
            end: text.len(),
            text: &text[start..],
        });
    }
    result
}

fn is_exact_model_heading(line: &str, exact_model: &str) -> bool {
    let Some(model_word) =
        find_ascii_word(line, "Model").or_else(|| find_ascii_word(line, "Models"))
    else {
        return false;
    };
    exact_token_positions(line, exact_model).any(|position| position >= model_word)
}

fn find_ascii_word(haystack: &str, word: &str) -> Option<usize> {
    haystack
        .match_indices(word)
        .find_map(|(index, _)| ascii_token_boundaries(haystack, index, word.len()).then_some(index))
}

fn exact_token_positions<'a>(
    haystack: &'a str,
    token: &'a str,
) -> impl Iterator<Item = usize> + 'a {
    haystack.match_indices(token).filter_map(move |(index, _)| {
        ascii_token_boundaries(haystack, index, token.len()).then_some(index)
    })
}

fn ascii_token_boundaries(haystack: &str, start: usize, len: usize) -> bool {
    let bytes = haystack.as_bytes();
    let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
    let end = start + len;
    let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
    before_ok && after_ok
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Object, Stream};
    use serde_json::{json, Value};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    use super::*;

    const DOCUMENT_GUID: &str = "cbe9c99d-492f-4d25-9d37-925d57816f27";
    const MOCK_API_KEY: &str = "mock-api-key";

    #[derive(Clone)]
    struct MockState {
        expected_body: Arc<Value>,
        metadata: Arc<Value>,
        pdf: Arc<Vec<u8>>,
        pdf_status: StatusCode,
        pdf_content_type: &'static str,
        pdf_requests: Arc<AtomicUsize>,
    }

    struct MockServer {
        base_url: String,
        task: JoinHandle<()>,
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn spawn_mock(mut state: MockState) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base_url = format!("http://{address}");
        Arc::make_mut(&mut state.metadata)["documents"][0]["mainDocumentDownloadURL"] =
            json!(format!("{base_url}{DOWNLOAD_PATH_PREFIX}{DOCUMENT_GUID}"));
        let app = Router::new()
            .route(METADATA_PATH, post(metadata))
            .route("/api/drs/data-pull/download/{guid}", get(pdf))
            .with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        MockServer { base_url, task }
    }

    async fn metadata(
        State(state): State<MockState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        if !has_mock_api_key(&headers) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if body != *state.expected_body {
            return StatusCode::BAD_REQUEST.into_response();
        }
        Json((*state.metadata).clone()).into_response()
    }

    async fn pdf(
        State(state): State<MockState>,
        Path(guid): Path<String>,
        headers: HeaderMap,
    ) -> Response {
        state.pdf_requests.fetch_add(1, Ordering::SeqCst);
        if !has_mock_api_key(&headers) || guid != DOCUMENT_GUID {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        (
            state.pdf_status,
            [("content-type", state.pdf_content_type)],
            (*state.pdf).clone(),
        )
            .into_response()
    }

    fn has_mock_api_key(headers: &HeaderMap) -> bool {
        headers
            .get(API_KEY_HEADER)
            .and_then(|value| value.to_str().ok())
            == Some(MOCK_API_KEY)
    }

    fn current_metadata(model_value: Value) -> Value {
        json!({
            "summary": {
                "doctypeName": "Type Certificate Data Sheet Model",
                "drsDoctypeName": "TCDSMODEL",
                "count": 1,
                "hasMoreItems": false,
                "totalItems": 1,
                "offset": 0,
                "sortBy": "docLastModifiedDate",
                "sortByOrder": "DESC"
            },
            "documents": [{
                "docLastModifiedDate": "2024-08-07",
                "documentGuid": DOCUMENT_GUID,
                "documentURL": "DRSDOCID109699679420240809163108.0001",
                "drs:status": "Current",
                "drs:tcdsmodelModel": model_value,
                "drs:documentNumber": "3A13",
                "drs:tcdsmodelRevisionNumber": "75",
                "drs:tcdsmodelTCHolder": "Textron Aviation Inc.",
                "drs:tcdsmodelFormerHolders": ["Cessna Aircraft Company"],
                "drs:tcdsmodelRevisionDate": "2024-08-07",
                "mainDocumentDownloadURL": "set-by-mock-server",
                "mainDocumentFileName": "3A13_Rev75.pdf"
            }]
        })
    }

    fn model_request() -> Value {
        json!({
            "offset": 0,
            "sortOrder": "DESC",
            "documentFilters": {
                "drs:status": ["Current"],
                "drs:tcdsmodelModel": ["182T"]
            }
        })
    }

    fn tcds_request() -> Value {
        json!({
            "offset": 0,
            "sortOrder": "DESC",
            "documentFilters": {
                "drs:status": ["Current"],
                "drs:tcdsmodelModel": ["182T"],
                "drs:documentNumber": ["3A13"]
            }
        })
    }

    fn state(expected_body: Value, metadata: Value) -> MockState {
        MockState {
            expected_body: Arc::new(expected_body),
            metadata: Arc::new(metadata),
            pdf: Arc::new(test_pdf()),
            pdf_status: StatusCode::OK,
            pdf_content_type: "application/pdf",
            pdf_requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn test_pdf() -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let lines = [
            "XIII. Model 182S, Approved 03 October 1996.",
            "      Model T182T, Approved 16 July 2001.",
            "      Model 182T, Approved 23 February 2001.",
            "Other exact regulator text.",
            "Serial Numbers Eligible",
            "182T: 18280945 and On",
            "",
            "Data Pertinent to Models 182S and 182T",
        ];
        let mut operations = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 10.into()]),
            Operation::new("Td", vec![50.into(), 740.into()]),
        ];
        for line in lines {
            operations.push(Operation::new("Tj", vec![Object::string_literal(line)]));
            operations.push(Operation::new("T*", vec![]));
        }
        operations.push(Operation::new("ET", vec![]));
        let content = Content { operations };
        let content_id =
            document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn exact_heading_does_not_confuse_related_model_tokens() {
        assert!(!is_exact_model_heading("Model T182T", "182T"));
        assert!(!is_exact_model_heading("Model 182", "182T"));
        assert!(is_exact_model_heading("Model 182T, Skylane", "182T"));
        assert!(is_exact_model_heading(
            "Models 182S and 182T are approved",
            "182T"
        ));
    }

    #[test]
    fn exact_model_blocks_accept_the_faa_abbreviated_serial_marker() {
        let pages = vec![TcdsPageText {
            page_number: 15,
            text: concat!(
                "X. Model 182Q (cont’d)\n",
                "Fuel Capacity (S/N 18265176 thru 18266590)\n",
                "Serial Nos. Eligible        18265176 thru 18265965 (1977 Model)\n",
                "                            18263479, 18265966 thru 18266590 (1978 Model)\n",
                "                            18266591 thru 18267300 (1979 Model)\n",
                "                            18267301 thru 18267715, except 18267302 (1980 Model)\n",
                "\n",
                "XI. Model R182, Skylane RG\n",
            )
            .to_string(),
        }];

        let blocks = extract_exact_model_blocks(&pages, "182Q", &DrsLimits::default()).unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].raw_text.contains("Serial Nos. Eligible"));
        assert!(blocks[0]
            .raw_text
            .contains("18263479, 18265966 thru 18266590"));
        assert!(blocks[0]
            .raw_text
            .contains("18267301 thru 18267715, except 18267302"));
        assert!(!blocks[0].raw_text.contains("XI. Model R182"));

        for unrelated in [
            "Serial Nos. Eligibility",
            "NotSerial Nos. Eligible",
            "Serial Nos. EligibleEquipment",
        ] {
            assert!(
                !is_serial_eligibility_marker(unrelated),
                "non-marker text was accepted: {unrelated:?}"
            );
        }
    }

    #[tokio::test]
    async fn model_first_fetch_is_bounded_and_preserves_provenance() {
        let mock_state = state(
            model_request(),
            current_metadata(json!("182 | T182T | 182S | 182T")),
        );
        let pdf_requests = Arc::clone(&mock_state.pdf_requests);
        let expected_pdf_sha256 = format!("{:x}", Sha256::digest(mock_state.pdf.as_slice()));
        let server = spawn_mock(mock_state).await;
        let client =
            DrsClient::for_test(server.base_url.clone(), MOCK_API_KEY, DrsLimits::default())
                .unwrap();

        let result = client
            .fetch_unique_current_tcds_for_model("182T")
            .await
            .unwrap();

        assert_eq!(result.metadata.tcds_number, "3A13");
        assert_eq!(result.metadata.revision_number.as_deref(), Some("75"));
        assert_eq!(
            result.metadata.tc_holder.as_deref(),
            Some("Textron Aviation Inc.")
        );
        assert_eq!(result.pdf_sha256, expected_pdf_sha256);
        assert_eq!(result.page_count, 1);
        assert_eq!(result.exact_model_blocks.len(), 1);
        assert!(result.exact_model_blocks[0]
            .raw_text
            .contains("182T: 18280945 and On"));
        assert_eq!(
            result.source_url,
            format!("{}{DOWNLOAD_PATH_PREFIX}{DOCUMENT_GUID}", server.base_url)
        );
        assert_eq!(pdf_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exact_tcds_fetch_uses_the_tcds_filter() {
        let mock_state = state(tcds_request(), current_metadata(json!(["182S", "182T"])));
        let server = spawn_mock(mock_state).await;
        let client =
            DrsClient::for_test(server.base_url.clone(), MOCK_API_KEY, DrsLimits::default())
                .unwrap();

        let result = client.fetch_current_tcds("3A13", "182T").await.unwrap();

        assert_eq!(result.metadata.document_guid, DOCUMENT_GUID);
    }

    #[tokio::test]
    async fn ambiguous_metadata_never_reaches_the_pdf_route() {
        let mut metadata = current_metadata(json!("182T"));
        metadata["summary"]["count"] = json!(2);
        metadata["summary"]["totalItems"] = json!(2);
        let mock_state = state(model_request(), metadata);
        let pdf_requests = Arc::clone(&mock_state.pdf_requests);
        let server = spawn_mock(mock_state).await;
        let client =
            DrsClient::for_test(server.base_url.clone(), MOCK_API_KEY, DrsLimits::default())
                .unwrap();

        let error = client
            .fetch_unique_current_tcds_for_model("182T")
            .await
            .unwrap_err();

        assert_eq!(
            error,
            DrsError::MetadataNotUnique {
                total: 2,
                returned: 1
            }
        );
        assert_eq!(pdf_requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn related_but_non_exact_model_is_rejected_before_download() {
        let mock_state = state(model_request(), current_metadata(json!(["182", "T182T"])));
        let pdf_requests = Arc::clone(&mock_state.pdf_requests);
        let server = spawn_mock(mock_state).await;
        let client =
            DrsClient::for_test(server.base_url.clone(), MOCK_API_KEY, DrsLimits::default())
                .unwrap();

        let error = client
            .fetch_unique_current_tcds_for_model("182T")
            .await
            .unwrap_err();

        assert_eq!(error, DrsError::RequestedModelMismatch);
        assert_eq!(pdf_requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pdf_redirect_is_not_followed() {
        let mut mock_state = state(model_request(), current_metadata(json!("182T")));
        mock_state.pdf_status = StatusCode::FOUND;
        let pdf_requests = Arc::clone(&mock_state.pdf_requests);
        let server = spawn_mock(mock_state).await;
        let client =
            DrsClient::for_test(server.base_url.clone(), MOCK_API_KEY, DrsLimits::default())
                .unwrap();

        let error = client
            .fetch_unique_current_tcds_for_model("182T")
            .await
            .unwrap_err();

        assert_eq!(error, DrsError::PdfRejected(302));
        assert_eq!(pdf_requests.load(Ordering::SeqCst), 1);
    }
}
