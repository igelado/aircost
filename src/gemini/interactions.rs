//! Strict, stateless client primitives for Gemini's versioned Interactions API.
//!
//! Aircraft catalog curation needs more than the final model text: callers must
//! be able to prove that a requested grounding tool ran, retain the exact tool
//! steps and citations, and fail closed on incomplete responses.  This module
//! deliberately does not contain catalog or database writes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error as StdError;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use lopdf::{Document, LoadOptions};
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, RANGE, RETRY_AFTER,
};
use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode};
use scraper::{Html, Selector};
use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use url::{Host, Url};
use xml::reader::{ParserConfig as XmlParserConfig, XmlEvent};

use crate::gemini::config::GeminiTask;
use crate::gemini::usage::{
    estimate_paid_list_cost, ApiFamily, Metrics as UsageMetrics, Outcome as UsageOutcome,
    SourceCorrelation, Start as UsageStart, Status as UsageStatus, Store as UsageStore,
    ToolUseBilling,
};

/// Versioned endpoint and wire revision used by the current multimodal
/// Interactions request shape. The `/v1` endpoint currently exposes a
/// different step-oriented input contract and rejects top-level image parts.
pub const GEMINI_INTERACTIONS_ENDPOINT: &str =
    "https://generativelanguage.googleapis.com/v1beta/interactions";
pub const GEMINI_INTERACTIONS_API_REVISION: &str = "2026-05-20";
pub const DEFAULT_GEMINI_CURATION_MODEL: &str = "gemini-3.5-flash";

const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 20_000_000;
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
const MAX_REDIRECTS: usize = 10;
const MAX_SOURCE_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE_PDF_PAGES: usize = 128;
const MAX_SOURCE_PDF_PAGE_DECOMPRESSED_BYTES: usize = 2 * 1024 * 1024;
const MAX_SOURCE_PDF_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ROBOTS_BYTES: usize = 256 * 1024;
const MAX_SOURCE_INDEX_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE_INDEXES: usize = 8;
const MAX_ROBOTS_SITEMAPS: usize = 4;
const MAX_SOURCE_INDEX_URLS: usize = 4_096;
const MAX_SOURCE_INDEX_URL_BYTES: usize = 4_096;
const MAX_SOURCE_INDEX_XML_DEPTH: usize = 16;
const MAX_SOURCE_INDEX_XML_EVENTS: usize = 100_000;
const GARMIN_PRODUCT_ORIGIN: &str = "https://www.garmin.com";
const MAX_GARMIN_PRODUCT_DISPLAY_NAME_BYTES: usize = 256;
const MAX_GARMIN_PRODUCT_SKU_BYTES: usize = 96;
const MAX_GARMIN_PRODUCT_METADATA_SENTENCE_BYTES: usize = 512;

pub type GeminiInteractionsResult<T> = Result<T, GeminiInteractionsError>;

#[derive(Debug)]
pub enum GeminiInteractionsError {
    InvalidRequest(String),
    InvalidResponse(String),
    InvalidUrl(String),
    Transport(reqwest::Error),
    Decode {
        source: serde_json::Error,
        body_excerpt: String,
    },
    Http {
        status: StatusCode,
        api_status: Option<String>,
        message: String,
        body_excerpt: String,
    },
    Accounting(String),
}

impl fmt::Display for GeminiInteractionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => write!(formatter, "invalid Gemini request: {message}"),
            Self::InvalidResponse(message) => {
                write!(formatter, "invalid Gemini response: {message}")
            }
            Self::InvalidUrl(message) => write!(formatter, "invalid public source URL: {message}"),
            Self::Transport(error) => write!(formatter, "Gemini transport error: {error}"),
            Self::Decode {
                source,
                body_excerpt,
            } => write!(
                formatter,
                "could not decode Gemini response: {source}; body={body_excerpt}"
            ),
            Self::Http {
                status,
                api_status,
                message,
                body_excerpt,
            } => {
                write!(formatter, "Gemini returned HTTP {status}")?;
                if let Some(api_status) = api_status {
                    write!(formatter, " ({api_status})")?;
                }
                if !message.is_empty() {
                    write!(formatter, ": {message}")?;
                }
                if message.is_empty() && !body_excerpt.is_empty() {
                    write!(formatter, ": {body_excerpt}")?;
                }
                Ok(())
            }
            Self::Accounting(message) => {
                write!(formatter, "Gemini usage accounting error: {message}")
            }
        }
    }
}

impl StdError for GeminiInteractionsError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Decode { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RetryPolicy {
    max_attempts: u8,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl RetryPolicy {
    pub fn new(
        max_attempts: u8,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> GeminiInteractionsResult<Self> {
        if !(1..=8).contains(&max_attempts) {
            return Err(GeminiInteractionsError::InvalidRequest(
                "retry max_attempts must be between 1 and 8".to_string(),
            ));
        }
        if initial_backoff.is_zero() {
            return Err(GeminiInteractionsError::InvalidRequest(
                "retry initial_backoff must be positive".to_string(),
            ));
        }
        if max_backoff < initial_backoff {
            return Err(GeminiInteractionsError::InvalidRequest(
                "retry max_backoff must be at least initial_backoff".to_string(),
            ));
        }
        Ok(Self {
            max_attempts,
            initial_backoff,
            max_backoff,
        })
    }

    pub fn max_attempts(&self) -> u8 {
        self.max_attempts
    }

    pub fn initial_backoff(&self) -> Duration {
        self.initial_backoff
    }

    pub fn max_backoff(&self) -> Duration {
        self.max_backoff
    }

    fn delay_after(&self, failed_attempt: u8, retry_after: Option<Duration>) -> Duration {
        if let Some(retry_after) = retry_after {
            return retry_after.min(self.max_backoff);
        }
        let multiplier = 1_u32
            .checked_shl(u32::from(failed_attempt.saturating_sub(1)))
            .unwrap_or(u32::MAX);
        self.initial_backoff
            .saturating_mul(multiplier)
            .min(self.max_backoff)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(4),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingSummaries {
    Auto,
    None,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    None,
    Validated,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_summaries: Option<ThinkingSummaries>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InteractionTool {
    GoogleSearch,
    UrlContext,
    Function {
        name: String,
        description: String,
        parameters: Value,
    },
}

impl InteractionTool {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> GeminiInteractionsResult<Self> {
        let tool = Self::Function {
            name: name.into(),
            description: description.into(),
            parameters,
        };
        validate_tool(&tool)?;
        Ok(tool)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    response_type: &'static str,
    mime_type: &'static str,
    schema: Value,
}

impl ResponseFormat {
    pub fn json(schema: Value) -> GeminiInteractionsResult<Self> {
        if !schema.is_object() {
            return Err(GeminiInteractionsError::InvalidRequest(
                "response JSON schema must be an object".to_string(),
            ));
        }
        Ok(Self {
            response_type: "text",
            mime_type: "application/json",
            schema,
        })
    }
}

/// One first-turn multimodal Interactions input item.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaResolution {
    Unspecified,
    Low,
    Medium,
    High,
    UltraHigh,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InteractionInputItem {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        data: String,
        resolution: MediaResolution,
    },
}

impl InteractionInputItem {
    pub fn text(text: impl Into<String>) -> GeminiInteractionsResult<Self> {
        let item = Self::Text { text: text.into() };
        item.validate()?;
        Ok(item)
    }

    /// Creates an inline image item using the Interactions API's base64 wire
    /// representation. File/URI upload lifecycle is intentionally outside this
    /// strict request primitive.
    pub fn inline_image(
        mime_type: impl Into<String>,
        bytes: &[u8],
    ) -> GeminiInteractionsResult<Self> {
        Self::inline_image_with_resolution(mime_type, bytes, MediaResolution::High)
    }

    pub fn inline_image_with_resolution(
        mime_type: impl Into<String>,
        bytes: &[u8],
        resolution: MediaResolution,
    ) -> GeminiInteractionsResult<Self> {
        if bytes.is_empty() {
            return Err(GeminiInteractionsError::InvalidRequest(
                "inline image bytes must not be empty".to_string(),
            ));
        }
        let item = Self::Image {
            mime_type: mime_type.into(),
            data: BASE64_STANDARD.encode(bytes),
            resolution,
        };
        item.validate()?;
        Ok(item)
    }

    fn validate(&self) -> GeminiInteractionsResult<()> {
        match self {
            Self::Text { text } if text.trim().is_empty() => {
                Err(GeminiInteractionsError::InvalidRequest(
                    "multimodal text input must not be empty".to_string(),
                ))
            }
            Self::Text { .. } => Ok(()),
            Self::Image {
                mime_type, data, ..
            } => {
                if !matches!(
                    mime_type.as_str(),
                    "image/png" | "image/jpeg" | "image/webp" | "image/heic" | "image/heif"
                ) {
                    return Err(GeminiInteractionsError::InvalidRequest(format!(
                        "unsupported inline image MIME type {mime_type:?}"
                    )));
                }
                if data.trim().is_empty() {
                    return Err(GeminiInteractionsError::InvalidRequest(
                        "inline image data must not be empty".to_string(),
                    ));
                }
                BASE64_STANDARD.decode(data).map_err(|_| {
                    GeminiInteractionsError::InvalidRequest(
                        "inline image data must be valid standard base64".to_string(),
                    )
                })?;
                Ok(())
            }
        }
    }
}

/// A simple first-turn text prompt, first-turn multimodal content, or the
/// complete client-managed step history required by stateless continuations.
#[derive(Clone, Debug)]
pub enum InteractionInput {
    Text(String),
    Items(Vec<InteractionInputItem>),
    Steps(Vec<Value>),
}

impl InteractionInput {
    pub fn multimodal(items: Vec<InteractionInputItem>) -> GeminiInteractionsResult<Self> {
        let input = Self::Items(items);
        input.validate()?;
        Ok(input)
    }

    fn validate(&self) -> GeminiInteractionsResult<()> {
        match self {
            Self::Text(text) if text.trim().is_empty() => Err(
                GeminiInteractionsError::InvalidRequest("input text must not be empty".to_string()),
            ),
            Self::Text(_) => Ok(()),
            Self::Items(items) if items.is_empty() => Err(GeminiInteractionsError::InvalidRequest(
                "multimodal input must contain at least one item".to_string(),
            )),
            Self::Items(items) => {
                for item in items {
                    item.validate()?;
                }
                Ok(())
            }
            Self::Steps(steps) => validate_history_steps(steps),
        }
    }
}

impl Serialize for InteractionInput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Text(text) => text.serialize(serializer),
            Self::Items(items) => items.serialize(serializer),
            Self::Steps(steps) => steps.serialize(serializer),
        }
    }
}

impl From<String> for InteractionInput {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for InteractionInput {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

/// Full history for `store=false` calls. Model steps are retained as raw JSON
/// so signatures and fields unknown to this client survive the next request.
#[derive(Clone, Debug)]
pub struct StatelessHistory {
    steps: Vec<Value>,
}

impl StatelessHistory {
    pub fn new(user_text: impl Into<String>) -> GeminiInteractionsResult<Self> {
        let user_text = user_text.into();
        if user_text.trim().is_empty() {
            return Err(GeminiInteractionsError::InvalidRequest(
                "initial user input must not be blank".to_string(),
            ));
        }
        Ok(Self {
            steps: vec![json!({
                "type": "user_input",
                "content": [{"type": "text", "text": user_text}]
            })],
        })
    }

    pub fn from_steps(steps: Vec<Value>) -> GeminiInteractionsResult<Self> {
        validate_history_steps(&steps)?;
        Ok(Self { steps })
    }

    pub fn steps(&self) -> &[Value] {
        &self.steps
    }

    pub fn into_steps(self) -> Vec<Value> {
        self.steps
    }

    pub fn append_user_text(&mut self, text: impl Into<String>) -> GeminiInteractionsResult<()> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(GeminiInteractionsError::InvalidRequest(
                "user input must not be blank".to_string(),
            ));
        }
        self.steps.push(json!({
            "type": "user_input",
            "content": [{"type": "text", "text": text}]
        }));
        Ok(())
    }

    /// Appends model-generated steps exactly at the decoded JSON-value level.
    /// This preserves thought/tool signatures needed for Gemini 3 continuation.
    pub fn append_response(
        &mut self,
        response: &InteractionResponse,
    ) -> GeminiInteractionsResult<()> {
        let raw_steps = response
            .raw
            .as_object()
            .and_then(|object| object.get("steps"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                GeminiInteractionsError::InvalidResponse(
                    "interaction response is missing a steps array".to_string(),
                )
            })?;
        if raw_steps.len() != response.interaction.steps.len() {
            return Err(GeminiInteractionsError::InvalidResponse(
                "raw and typed response step counts differ".to_string(),
            ));
        }
        for step in raw_steps {
            validate_history_step_object(step)?;
        }
        let mut next_steps = self.steps.clone();
        next_steps.extend(raw_steps.iter().cloned());
        validate_history_steps(&next_steps)?;
        self.steps = next_steps;
        Ok(())
    }

    pub fn append_function_result(
        &mut self,
        call: &FunctionCallStep,
        result: Value,
    ) -> GeminiInteractionsResult<()> {
        self.append_function_result_inner(call, result, false)
    }

    pub fn append_function_error(
        &mut self,
        call: &FunctionCallStep,
        error: Value,
    ) -> GeminiInteractionsResult<()> {
        self.append_function_result_inner(call, error, true)
    }

    fn append_function_result_inner(
        &mut self,
        call: &FunctionCallStep,
        result: Value,
        is_error: bool,
    ) -> GeminiInteractionsResult<()> {
        let matching_call = self.steps.iter().any(|step| {
            step.get("type").and_then(Value::as_str) == Some("function_call")
                && step.get("id").and_then(Value::as_str) == Some(call.id.as_str())
                && step.get("name").and_then(Value::as_str) == Some(call.name.as_str())
        });
        if !matching_call {
            return Err(GeminiInteractionsError::InvalidRequest(format!(
                "function call {} ({}) is not present in stateless history",
                call.id, call.name
            )));
        }
        let duplicate_result = self.steps.iter().any(|step| {
            step.get("type").and_then(Value::as_str) == Some("function_result")
                && step.get("call_id").and_then(Value::as_str) == Some(call.id.as_str())
        });
        if duplicate_result {
            return Err(GeminiInteractionsError::InvalidRequest(format!(
                "function call {} already has a result in stateless history",
                call.id
            )));
        }
        let result_text =
            serde_json::to_string(&result).map_err(|source| GeminiInteractionsError::Decode {
                source,
                body_excerpt: "local function result serialization".to_string(),
            })?;
        let mut next_steps = self.steps.clone();
        next_steps.push(json!({
            "type": "function_result",
            "name": call.name,
            "call_id": call.id,
            "is_error": is_error,
            "result": [{"type": "text", "text": result_text}]
        }));
        validate_history_steps(&next_steps)?;
        self.steps = next_steps;
        Ok(())
    }

    pub fn input(&self) -> InteractionInput {
        InteractionInput::Steps(self.steps.clone())
    }

    pub fn require_tool_activity(
        &self,
        requirements: &CurationRequirements,
    ) -> GeminiInteractionsResult<()> {
        requirements.validate()?;
        let steps = parse_history_steps(&self.steps)?;
        require_tool_activity(&steps, requirements)
    }
}

impl From<StatelessHistory> for InteractionInput {
    fn from(value: StatelessHistory) -> Self {
        Self::Steps(value.into_steps())
    }
}

impl From<&StatelessHistory> for InteractionInput {
    fn from(value: &StatelessHistory) -> Self {
        value.input()
    }
}

/// Non-wire attribution attached to one logical Interactions request.
///
/// The context is ignored when the client has no usage store. When accounting
/// is enabled, it is persisted with a SHA-256 fingerprint of the serialized
/// request; prompts and image bytes are never copied into the accounting row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionAccountingContext {
    pub task: GeminiTask,
    pub purpose: String,
    pub correlation_id: Option<String>,
    pub listing_id: Option<i64>,
    pub source: Option<SourceCorrelation>,
}

impl InteractionAccountingContext {
    pub fn new(task: GeminiTask, purpose: impl Into<String>) -> Self {
        Self {
            task,
            purpose: purpose.into(),
            correlation_id: None,
            listing_id: None,
            source: None,
        }
    }

    pub fn with_correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    pub fn with_listing_id(mut self, listing_id: i64) -> Self {
        self.listing_id = Some(listing_id);
        self
    }

    pub fn with_source(mut self, kind: impl Into<String>, id: impl Into<String>) -> Self {
        self.source = Some(SourceCorrelation {
            kind: kind.into(),
            id: id.into(),
        });
        self
    }

    fn validate(&self) -> GeminiInteractionsResult<()> {
        if self.purpose.trim().is_empty() {
            return Err(GeminiInteractionsError::InvalidRequest(
                "accounting purpose must not be blank".to_string(),
            ));
        }
        if self
            .correlation_id
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(GeminiInteractionsError::InvalidRequest(
                "accounting correlation_id must not be blank".to_string(),
            ));
        }
        if self.listing_id.is_some_and(|listing_id| listing_id < 1) {
            return Err(GeminiInteractionsError::InvalidRequest(
                "accounting listing_id must be positive".to_string(),
            ));
        }
        if self
            .source
            .as_ref()
            .is_some_and(|source| source.kind.trim().is_empty() || source.id.trim().is_empty())
        {
            return Err(GeminiInteractionsError::InvalidRequest(
                "accounting source kind and id must not be blank".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct CreateInteractionRequest {
    pub model: String,
    pub input: InteractionInput,
    pub system_instruction: Option<String>,
    pub tools: Vec<InteractionTool>,
    pub response_format: Option<ResponseFormat>,
    pub generation_config: Option<GenerationConfig>,
    /// Optional top-level Interactions service tier (`standard`, `flex`, or
    /// `priority`). `None` preserves provider-default standard service.
    pub service_tier: Option<String>,
    /// Local attribution only; never serialized to Gemini.
    pub accounting_context: Option<InteractionAccountingContext>,
}

impl CreateInteractionRequest {
    pub fn new(model: impl Into<String>, input: impl Into<InteractionInput>) -> Self {
        Self {
            model: model.into(),
            input: input.into(),
            system_instruction: None,
            tools: Vec::new(),
            response_format: None,
            generation_config: None,
            service_tier: None,
            accounting_context: None,
        }
    }

    pub fn for_aircraft_curation(input: impl Into<InteractionInput>) -> Self {
        Self::new(DEFAULT_GEMINI_CURATION_MODEL, input).with_generation_config(GenerationConfig {
            tool_choice: Some(ToolChoice::Validated),
            ..GenerationConfig::default()
        })
    }

    pub fn with_system_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.system_instruction = Some(instruction.into());
        self
    }

    pub fn with_tool(mut self, tool: InteractionTool) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn with_response_format(mut self, response_format: ResponseFormat) -> Self {
        self.response_format = Some(response_format);
        self
    }

    pub fn with_generation_config(mut self, generation_config: GenerationConfig) -> Self {
        self.generation_config = Some(generation_config);
        self
    }

    pub fn with_service_tier(mut self, service_tier: impl Into<String>) -> Self {
        let service_tier = service_tier.into();
        let service_tier = service_tier.trim();
        self.service_tier = (!service_tier.is_empty() && service_tier != "unspecified")
            .then(|| service_tier.to_string());
        self
    }

    pub fn with_accounting_context(
        mut self,
        accounting_context: InteractionAccountingContext,
    ) -> Self {
        self.accounting_context = Some(accounting_context);
        self
    }

    pub fn validate(&self) -> GeminiInteractionsResult<()> {
        let model = self.model.trim();
        if model.is_empty() {
            return Err(GeminiInteractionsError::InvalidRequest(
                "model must not be empty".to_string(),
            ));
        }
        if model.ends_with("-latest") {
            return Err(GeminiInteractionsError::InvalidRequest(
                "aircraft curation requires a pinned model, not a -latest alias".to_string(),
            ));
        }
        self.input.validate()?;
        if self
            .system_instruction
            .as_ref()
            .is_some_and(|instruction| instruction.trim().is_empty())
        {
            return Err(GeminiInteractionsError::InvalidRequest(
                "system_instruction must not be blank".to_string(),
            ));
        }
        if self
            .generation_config
            .as_ref()
            .and_then(|config| config.max_output_tokens)
            == Some(0)
        {
            return Err(GeminiInteractionsError::InvalidRequest(
                "max_output_tokens must be positive".to_string(),
            ));
        }
        if let Some(service_tier) = self.service_tier.as_deref() {
            if !matches!(service_tier, "standard" | "flex" | "priority") {
                return Err(GeminiInteractionsError::InvalidRequest(
                    "service_tier must be standard, flex, priority, or omitted".to_string(),
                ));
            }
        }
        if let Some(context) = &self.accounting_context {
            context.validate()?;
        }

        let mut google_search_seen = false;
        let mut url_context_seen = false;
        let mut function_names = HashSet::new();
        for tool in &self.tools {
            validate_tool(tool)?;
            match tool {
                InteractionTool::GoogleSearch if google_search_seen => {
                    return Err(GeminiInteractionsError::InvalidRequest(
                        "google_search tool may be declared only once".to_string(),
                    ));
                }
                InteractionTool::GoogleSearch => google_search_seen = true,
                InteractionTool::UrlContext if url_context_seen => {
                    return Err(GeminiInteractionsError::InvalidRequest(
                        "url_context tool may be declared only once".to_string(),
                    ));
                }
                InteractionTool::UrlContext => url_context_seen = true,
                InteractionTool::Function { name, .. } if !function_names.insert(name.as_str()) => {
                    return Err(GeminiInteractionsError::InvalidRequest(format!(
                        "duplicate function tool name {name}"
                    )));
                }
                InteractionTool::Function { .. } => {}
            }
        }
        let combines_builtin_and_custom =
            (google_search_seen || url_context_seen) && !function_names.is_empty();
        if combines_builtin_and_custom && !model.starts_with("gemini-3") {
            return Err(GeminiInteractionsError::InvalidRequest(
                "built-in and custom tool combination requires a Gemini 3 model".to_string(),
            ));
        }
        if combines_builtin_and_custom
            && matches!(
                self.generation_config
                    .as_ref()
                    .and_then(|config| config.tool_choice.as_ref()),
                Some(ToolChoice::Auto)
            )
        {
            return Err(GeminiInteractionsError::InvalidRequest(
                "tool_choice=auto is not supported when combining built-in and custom tools; use validated"
                    .to_string(),
            ));
        }
        Ok(())
    }

    pub fn validate_for_curation(
        &self,
        requirements: &CurationRequirements,
    ) -> GeminiInteractionsResult<()> {
        self.validate()?;
        requirements.validate()?;
        if requirements.grounding.requires_google_search()
            && !self
                .tools
                .iter()
                .any(|tool| matches!(tool, InteractionTool::GoogleSearch))
        {
            return Err(GeminiInteractionsError::InvalidRequest(
                "curation requires the google_search tool declaration".to_string(),
            ));
        }
        if requirements.grounding.requires_url_context()
            && !self
                .tools
                .iter()
                .any(|tool| matches!(tool, InteractionTool::UrlContext))
        {
            return Err(GeminiInteractionsError::InvalidRequest(
                "curation requires the url_context tool declaration".to_string(),
            ));
        }
        let declared_functions = self
            .tools
            .iter()
            .filter_map(|tool| match tool {
                InteractionTool::Function { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect::<HashSet<_>>();
        for name in &requirements.required_any_function_names {
            if !declared_functions.contains(name.as_str()) {
                return Err(GeminiInteractionsError::InvalidRequest(format!(
                    "required curation function {name} is not declared as a tool"
                )));
            }
        }
        Ok(())
    }

    fn wire_request(&self) -> WireCreateInteractionRequest<'_> {
        WireCreateInteractionRequest {
            model: &self.model,
            input: &self.input,
            system_instruction: self.system_instruction.as_deref(),
            tools: &self.tools,
            response_format: self.response_format.as_ref(),
            generation_config: self.generation_config.as_ref(),
            service_tier: self.service_tier.as_deref(),
            store: false,
            stream: false,
            background: false,
        }
    }
}

fn validate_tool(tool: &InteractionTool) -> GeminiInteractionsResult<()> {
    let InteractionTool::Function {
        name,
        description,
        parameters,
    } = tool
    else {
        return Ok(());
    };
    if !valid_function_name(name) {
        return Err(GeminiInteractionsError::InvalidRequest(format!(
            "invalid function tool name {name:?}; use 1-64 ASCII letters, digits, or underscores"
        )));
    }
    if description.trim().is_empty() {
        return Err(GeminiInteractionsError::InvalidRequest(format!(
            "function tool {name} requires a description"
        )));
    }
    let schema = parameters.as_object().ok_or_else(|| {
        GeminiInteractionsError::InvalidRequest(format!(
            "function tool {name} parameters must be a JSON Schema object"
        ))
    })?;
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(GeminiInteractionsError::InvalidRequest(format!(
            "function tool {name} parameter schema must have type=object"
        )));
    }
    Ok(())
}

fn valid_function_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn validate_history_step_object(step: &Value) -> GeminiInteractionsResult<()> {
    let object = step.as_object().ok_or_else(|| {
        GeminiInteractionsError::InvalidRequest(
            "each stateless history step must be a JSON object".to_string(),
        )
    })?;
    if object.get("type").and_then(Value::as_str).is_none() {
        return Err(GeminiInteractionsError::InvalidRequest(
            "each stateless history step requires a string type".to_string(),
        ));
    }
    Ok(())
}

fn parse_history_steps(steps: &[Value]) -> GeminiInteractionsResult<Vec<InteractionStep>> {
    steps
        .iter()
        .map(|step| {
            validate_history_step_object(step)?;
            serde_json::from_value(step.clone()).map_err(|source| GeminiInteractionsError::Decode {
                source,
                body_excerpt: "stateless history step".to_string(),
            })
        })
        .collect()
}

fn validate_history_steps(steps: &[Value]) -> GeminiInteractionsResult<()> {
    if steps.is_empty() {
        return Err(GeminiInteractionsError::InvalidRequest(
            "stateless history must not be empty".to_string(),
        ));
    }
    if steps[0].get("type").and_then(Value::as_str) != Some("user_input") {
        return Err(GeminiInteractionsError::InvalidRequest(
            "stateless history must start with a user_input step".to_string(),
        ));
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum CallKind {
        GoogleSearch,
        UrlContext,
        Function,
    }

    let parsed = parse_history_steps(steps)?;
    let mut calls: HashMap<&str, (CallKind, Option<&str>)> = HashMap::new();
    let mut results = HashSet::new();
    for step in &parsed {
        match step {
            InteractionStep::UserInput(input) => {
                if input.content.is_empty() {
                    return Err(GeminiInteractionsError::InvalidRequest(
                        "user_input content must not be empty".to_string(),
                    ));
                }
            }
            InteractionStep::GoogleSearchCall(call) => {
                if calls
                    .insert(call.id.as_str(), (CallKind::GoogleSearch, None))
                    .is_some()
                {
                    return Err(GeminiInteractionsError::InvalidRequest(format!(
                        "duplicate history tool call id {}",
                        call.id
                    )));
                }
            }
            InteractionStep::UrlContextCall(call) => {
                if calls
                    .insert(call.id.as_str(), (CallKind::UrlContext, None))
                    .is_some()
                {
                    return Err(GeminiInteractionsError::InvalidRequest(format!(
                        "duplicate history tool call id {}",
                        call.id
                    )));
                }
            }
            InteractionStep::FunctionCall(call) => {
                if calls
                    .insert(
                        call.id.as_str(),
                        (CallKind::Function, Some(call.name.as_str())),
                    )
                    .is_some()
                {
                    return Err(GeminiInteractionsError::InvalidRequest(format!(
                        "duplicate history tool call id {}",
                        call.id
                    )));
                }
            }
            InteractionStep::GoogleSearchResult(result) => {
                validate_history_result(
                    &calls,
                    &mut results,
                    result.call_id.as_str(),
                    CallKind::GoogleSearch,
                    None,
                )?;
            }
            InteractionStep::UrlContextResult(result) => {
                validate_history_result(
                    &calls,
                    &mut results,
                    result.call_id.as_str(),
                    CallKind::UrlContext,
                    None,
                )?;
            }
            InteractionStep::FunctionResult(result) => {
                validate_history_result(
                    &calls,
                    &mut results,
                    result.call_id.as_str(),
                    CallKind::Function,
                    result.name.as_deref(),
                )?;
            }
            InteractionStep::ModelOutput(_)
            | InteractionStep::Thought(_)
            | InteractionStep::Unknown { .. } => {}
        }
    }
    fn validate_history_result<'a>(
        calls: &HashMap<&'a str, (CallKind, Option<&'a str>)>,
        results: &mut HashSet<&'a str>,
        call_id: &'a str,
        expected_kind: CallKind,
        result_name: Option<&str>,
    ) -> GeminiInteractionsResult<()> {
        let Some((actual_kind, call_name)) = calls.get(call_id) else {
            return Err(GeminiInteractionsError::InvalidRequest(format!(
                "history tool result references unknown or later call id {call_id}"
            )));
        };
        if *actual_kind != expected_kind {
            return Err(GeminiInteractionsError::InvalidRequest(format!(
                "history tool result type does not match call id {call_id}"
            )));
        }
        if let (Some(call_name), Some(result_name)) = (call_name, result_name) {
            if call_name != &result_name {
                return Err(GeminiInteractionsError::InvalidRequest(format!(
                    "function result name {result_name} does not match {call_name} for {call_id}"
                )));
            }
        }
        if !results.insert(call_id) {
            return Err(GeminiInteractionsError::InvalidRequest(format!(
                "duplicate history tool result for call id {call_id}"
            )));
        }
        Ok(())
    }

    Ok(())
}

#[derive(Serialize)]
struct WireCreateInteractionRequest<'a> {
    model: &'a str,
    input: &'a InteractionInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<&'a str>,
    #[serde(skip_serializing_if = "slice_is_empty")]
    tools: &'a [InteractionTool],
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<&'a ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<&'a GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<&'a str>,
    store: bool,
    stream: bool,
    background: bool,
}

fn slice_is_empty<T>(values: &[T]) -> bool {
    values.is_empty()
}

#[derive(Clone)]
pub struct GeminiInteractionsClient {
    api_client: Client,
    api_key: HeaderValue,
    endpoint: Url,
    retry_policy: RetryPolicy,
    resolver_timeout: Duration,
    usage_store: Option<UsageStore>,
}

impl GeminiInteractionsClient {
    pub fn new(api_key: impl AsRef<str>) -> GeminiInteractionsResult<Self> {
        Self::with_options(api_key, Duration::from_secs(60), RetryPolicy::default())
    }

    pub fn with_options(
        api_key: impl AsRef<str>,
        timeout: Duration,
        retry_policy: RetryPolicy,
    ) -> GeminiInteractionsResult<Self> {
        let endpoint = Url::parse(GEMINI_INTERACTIONS_ENDPOINT).map_err(|error| {
            GeminiInteractionsError::InvalidRequest(format!(
                "invalid built-in Interactions endpoint: {error}"
            ))
        })?;
        Self::build(api_key.as_ref(), timeout, retry_policy, endpoint)
    }

    fn build(
        api_key: &str,
        timeout: Duration,
        retry_policy: RetryPolicy,
        endpoint: Url,
    ) -> GeminiInteractionsResult<Self> {
        if api_key.trim().is_empty() {
            return Err(GeminiInteractionsError::InvalidRequest(
                "Gemini API key must not be empty".to_string(),
            ));
        }
        if timeout.is_zero() {
            return Err(GeminiInteractionsError::InvalidRequest(
                "Gemini timeout must be positive".to_string(),
            ));
        }
        let mut api_key = HeaderValue::from_str(api_key.trim()).map_err(|_| {
            GeminiInteractionsError::InvalidRequest(
                "Gemini API key contains invalid header characters".to_string(),
            )
        })?;
        api_key.set_sensitive(true);

        let api_client = Client::builder()
            .timeout(timeout)
            .redirect(Policy::none())
            .build()
            .map_err(GeminiInteractionsError::Transport)?;
        Ok(Self {
            api_client,
            api_key,
            endpoint,
            retry_policy,
            resolver_timeout: timeout.min(Duration::from_secs(60)),
            usage_store: None,
        })
    }

    /// Enable durable usage accounting for requests carrying an accounting
    /// context. Requests without a context remain unrecorded, which keeps
    /// low-level transport probes and existing callers backward-compatible.
    pub fn with_usage_store(mut self, usage_store: UsageStore) -> Self {
        self.usage_store = Some(usage_store);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_endpoint(
        api_key: &str,
        endpoint: Url,
        retry_policy: RetryPolicy,
    ) -> GeminiInteractionsResult<Self> {
        Self::build(api_key, Duration::from_secs(5), retry_policy, endpoint)
    }

    pub async fn create(
        &self,
        request: &CreateInteractionRequest,
    ) -> GeminiInteractionsResult<InteractionResponse> {
        request.validate()?;
        let body = serde_json::to_vec(&request.wire_request()).map_err(|source| {
            GeminiInteractionsError::Decode {
                source,
                body_excerpt: "request serialization".to_string(),
            }
        })?;
        if body.len() >= MAX_REQUEST_BYTES {
            return Err(GeminiInteractionsError::InvalidRequest(format!(
                "serialized Gemini request must be smaller than {MAX_REQUEST_BYTES} bytes"
            )));
        }

        let accounting_attempt = match (
            self.usage_store.as_ref(),
            request.accounting_context.as_ref(),
        ) {
            (Some(store), Some(context)) => {
                let mut start = UsageStart::new(
                    context.task.as_str(),
                    context.purpose.trim(),
                    ApiFamily::Interactions,
                    request.model.trim(),
                );
                start.api_version = Some(GEMINI_INTERACTIONS_API_REVISION.to_string());
                start.service_tier = request
                    .service_tier
                    .clone()
                    .unwrap_or_else(|| "standard".to_string());
                start.correlation_id = context.correlation_id.clone();
                start.request_fingerprint = Some(request_fingerprint(&body));
                start.listing_id = context.listing_id;
                start.source = context.source.clone();
                Some(
                    store
                        .start(&start)
                        .await
                        .map_err(|error| GeminiInteractionsError::Accounting(error.to_string()))?,
                )
            }
            _ => None,
        };

        let result = self.send_create(body).await;
        match result {
            Ok(response) => {
                if let (Some(store), Some(attempt)) =
                    (self.usage_store.as_ref(), accounting_attempt)
                {
                    let outcome = interaction_usage_outcome(
                        &response,
                        &request.model,
                        request.service_tier.as_deref().unwrap_or("standard"),
                        &request.tools,
                    );
                    store
                        .finish(attempt, &outcome)
                        .await
                        .map_err(|error| GeminiInteractionsError::Accounting(error.to_string()))?;
                }
                Ok(response)
            }
            Err((error, attempts)) => {
                if let (Some(store), Some(attempt)) =
                    (self.usage_store.as_ref(), accounting_attempt)
                {
                    let mut outcome = UsageOutcome::failed(error.to_string());
                    outcome.attempt_count = u32::from(attempts.max(1));
                    outcome.retry_count = outcome.attempt_count.saturating_sub(1);
                    if let Err(accounting_error) = store.finish(attempt, &outcome).await {
                        return Err(GeminiInteractionsError::Accounting(format!(
                            "provider request failed with {error}; additionally could not finalize accounting: {accounting_error}"
                        )));
                    }
                }
                Err(error)
            }
        }
    }

    async fn send_create(
        &self,
        body: Vec<u8>,
    ) -> Result<InteractionResponse, (GeminiInteractionsError, u8)> {
        for attempt in 1..=self.retry_policy.max_attempts {
            let response = self
                .api_client
                .post(self.endpoint.clone())
                .header("x-goog-api-key", self.api_key.clone())
                .header("Api-Revision", GEMINI_INTERACTIONS_API_REVISION)
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json")
                .body(body.clone())
                .send()
                .await;

            let response = match response {
                Ok(response) => response,
                Err(error)
                    if is_transient_transport_error(&error)
                        && attempt < self.retry_policy.max_attempts =>
                {
                    tokio::time::sleep(self.retry_policy.delay_after(attempt, None)).await;
                    continue;
                }
                Err(error) => {
                    return Err((GeminiInteractionsError::Transport(error), attempt));
                }
            };

            let status = response.status();
            let retry_after = parse_retry_after(response.headers().get(RETRY_AFTER));
            if response
                .content_length()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES)
            {
                return Err((
                    GeminiInteractionsError::InvalidResponse(format!(
                        "response content-length exceeds {MAX_RESPONSE_BYTES} bytes"
                    )),
                    attempt,
                ));
            }
            let bytes = match response.bytes().await {
                Ok(bytes) => bytes,
                Err(error)
                    if is_transient_transport_error(&error)
                        && attempt < self.retry_policy.max_attempts =>
                {
                    tokio::time::sleep(self.retry_policy.delay_after(attempt, retry_after)).await;
                    continue;
                }
                Err(error) => {
                    return Err((GeminiInteractionsError::Transport(error), attempt));
                }
            };
            if bytes.len() as u64 > MAX_RESPONSE_BYTES {
                return Err((
                    GeminiInteractionsError::InvalidResponse(format!(
                        "response exceeds {MAX_RESPONSE_BYTES} bytes"
                    )),
                    attempt,
                ));
            }

            if !status.is_success() {
                if is_transient_status(status) && attempt < self.retry_policy.max_attempts {
                    tokio::time::sleep(self.retry_policy.delay_after(attempt, retry_after)).await;
                    continue;
                }
                return Err((http_error(status, &bytes), attempt));
            }

            let raw: Value = serde_json::from_slice(&bytes).map_err(|source| {
                (
                    GeminiInteractionsError::Decode {
                        source,
                        body_excerpt: body_excerpt(&bytes),
                    },
                    attempt,
                )
            })?;
            let interaction: Interaction =
                serde_json::from_value(raw.clone()).map_err(|source| {
                    (
                        GeminiInteractionsError::Decode {
                            source,
                            body_excerpt: body_excerpt(&bytes),
                        },
                        attempt,
                    )
                })?;
            interaction
                .validate_wire_shape()
                .map_err(|error| (error, attempt))?;
            return Ok(InteractionResponse {
                interaction,
                raw,
                attempts: attempt,
            });
        }

        Err((
            GeminiInteractionsError::InvalidResponse(
                "retry loop ended without a response".to_string(),
            ),
            self.retry_policy.max_attempts,
        ))
    }

    /// Follows redirects without attaching the Gemini API key and returns the
    /// final HTTP URL. This is a transport canonicalization helper, not proof
    /// that the resulting page supports a model claim.
    pub async fn resolve_final_url(
        &self,
        source_url: &str,
    ) -> GeminiInteractionsResult<ResolvedUrl> {
        let requested_url = Url::parse(source_url).map_err(|error| {
            GeminiInteractionsError::InvalidUrl(format!("{source_url:?}: {error}"))
        })?;
        validate_public_http_url(&requested_url)?;
        let mut current_url = requested_url.clone();
        for redirect_count in 0..=MAX_REDIRECTS {
            let resolver = pinned_public_client(&current_url, self.resolver_timeout).await?;
            let mut response = resolver
                .head(current_url.clone())
                .send()
                .await
                .map_err(GeminiInteractionsError::Transport)?;
            ensure_public_peer(&response)?;
            if matches!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
            ) {
                response = resolver
                    .get(current_url.clone())
                    .header(RANGE, "bytes=0-0")
                    .send()
                    .await
                    .map_err(GeminiInteractionsError::Transport)?;
                ensure_public_peer(&response)?;
            }

            if redirect_status(response.status()) {
                if redirect_count == MAX_REDIRECTS {
                    return Err(GeminiInteractionsError::InvalidUrl(format!(
                        "source redirect count exceeds {MAX_REDIRECTS}"
                    )));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .ok_or_else(|| {
                        GeminiInteractionsError::InvalidUrl(format!(
                            "redirect from {current_url} has no Location header"
                        ))
                    })?
                    .to_str()
                    .map_err(|_| {
                        GeminiInteractionsError::InvalidUrl(format!(
                            "redirect from {current_url} has a non-UTF-8 Location header"
                        ))
                    })?;
                current_url = current_url.join(location).map_err(|error| {
                    GeminiInteractionsError::InvalidUrl(format!(
                        "invalid redirect target {location:?}: {error}"
                    ))
                })?;
                current_url.set_fragment(None);
                validate_public_http_url(&current_url)?;
                continue;
            }

            current_url.set_fragment(None);
            return Ok(ResolvedUrl {
                requested_url,
                final_url: current_url,
                status: response.status(),
            });
        }
        Err(GeminiInteractionsError::InvalidUrl(
            "source URL resolution ended unexpectedly".to_string(),
        ))
    }

    /// Fetch a public publisher document without attaching the Gemini API key.
    ///
    /// This is intentionally crate-private: the normalized body is transient
    /// verification material and must never become an API or persistence
    /// payload. Every DNS answer and connected peer is public, redirects are
    /// followed manually, and the decoded response body is streamed under a
    /// hard cap.
    pub(crate) async fn fetch_public_source_document(
        &self,
        source_url: &str,
    ) -> GeminiInteractionsResult<FetchedSourceDocument> {
        let requested_url = Url::parse(source_url).map_err(|error| {
            GeminiInteractionsError::InvalidUrl(format!("{source_url:?}: {error}"))
        })?;
        validate_public_http_url(&requested_url)?;
        tokio::time::timeout(
            self.resolver_timeout,
            fetch_public_source_document_inner(
                requested_url,
                self.resolver_timeout,
                MAX_SOURCE_DOCUMENT_BYTES,
                None,
            ),
        )
        .await
        .map_err(|_| {
            GeminiInteractionsError::InvalidUrl(
                "public source document fetch exceeded its total timeout".to_string(),
            )
        })?
    }

    /// Fetch one index-discovered publisher document while refusing to follow
    /// any redirect away from the requested URL's exact HTTPS origin.
    ///
    /// The redirect target is validated before another request is made. The
    /// ordinary Search-citation fetch above intentionally retains its existing
    /// public cross-origin redirect behavior.
    pub(crate) async fn fetch_public_same_origin_source_document(
        &self,
        source_url: &str,
    ) -> GeminiInteractionsResult<FetchedSourceDocument> {
        let requested_url = Url::parse(source_url).map_err(|error| {
            GeminiInteractionsError::InvalidUrl(format!("{source_url:?}: {error}"))
        })?;
        validate_public_same_origin_source_document_url(&requested_url, &requested_url)?;
        tokio::time::timeout(
            self.resolver_timeout,
            fetch_public_source_document_inner(
                requested_url.clone(),
                self.resolver_timeout,
                MAX_SOURCE_DOCUMENT_BYTES,
                Some(requested_url),
            ),
        )
        .await
        .map_err(|_| {
            GeminiInteractionsError::InvalidUrl(
                "same-origin public source document fetch exceeded its total timeout".to_string(),
            )
        })?
    }

    /// Discover publisher-owned URLs from the seed publisher's robots and
    /// sitemap indexes without attaching the Gemini API key.
    ///
    /// The returned URLs are discovery hints only. Callers must independently
    /// fetch and verify any candidate before using it as evidence. This helper
    /// accepts only HTTPS URLs on the seed's exact origin, bounds all fetched
    /// bytes and extracted locations, and never returns robots or sitemap
    /// bodies.
    pub(crate) async fn discover_public_source_index_candidates(
        &self,
        seed_url: &str,
    ) -> GeminiInteractionsResult<Vec<Url>> {
        let seed_url = Url::parse(seed_url).map_err(|error| {
            GeminiInteractionsError::InvalidUrl(format!("{seed_url:?}: {error}"))
        })?;
        validate_public_source_index_url(&seed_url, &seed_url)?;
        tokio::time::timeout(
            self.resolver_timeout,
            discover_public_source_index_candidates_inner(seed_url, self.resolver_timeout),
        )
        .await
        .map_err(|_| {
            GeminiInteractionsError::InvalidUrl(
                "public source index discovery exceeded its total timeout".to_string(),
            )
        })?
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedUrl {
    pub requested_url: Url,
    pub final_url: Url,
    pub status: StatusCode,
}

#[derive(Clone)]
pub(crate) struct FetchedSourceDocument {
    pub(crate) final_url: Url,
    pub(crate) content_sha256: String,
    pub(crate) publisher_text: String,
    /// Present only when the strict exact-origin Garmin HTML adapter found one
    /// unique literal `product_display_name`/`sku0` pair.
    ///
    /// Generic HTML, text, and PDFs never populate this field. Keeping the
    /// publisher-specific record typed prevents adjacent table rows or hidden
    /// generic metadata from becoming deterministic product identity proof.
    pub(crate) garmin_product_page_identity: Option<GarminProductPageIdentityRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GarminProductPageIdentityRecord {
    pub(crate) product_display_name: String,
    pub(crate) sku0: String,
}

impl GarminProductPageIdentityRecord {
    pub(crate) fn literal_evidence_text(&self) -> String {
        format!(
            "Garmin product page metadata: product_display_name {}; sku0 {}.",
            self.product_display_name, self.sku0
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceDocumentKind {
    Html,
    PlainText,
    Pdf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceIndexDocumentKind {
    Robots,
    Sitemap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SitemapDocumentKind {
    UrlSet,
    SitemapIndex,
}

#[derive(Debug)]
struct ParsedSitemap {
    kind: SitemapDocumentKind,
    locations: Vec<String>,
}

async fn discover_public_source_index_candidates_inner(
    seed_url: Url,
    timeout: Duration,
) -> GeminiInteractionsResult<Vec<Url>> {
    let mut robots_url = seed_url.clone();
    robots_url.set_path("/robots.txt");
    robots_url.set_query(None);
    robots_url.set_fragment(None);

    let mut sitemap_queue = VecDeque::new();
    match fetch_public_source_index_document(
        robots_url,
        &seed_url,
        timeout,
        MAX_ROBOTS_BYTES,
        SourceIndexDocumentKind::Robots,
    )
    .await
    {
        Ok(body) => {
            sitemap_queue.extend(parse_robots_sitemap_urls(&body, &seed_url));
        }
        Err(GeminiInteractionsError::InvalidResponse(_)) => {}
        Err(error) => return Err(error),
    }

    let mut conventional_sitemap_url = seed_url.clone();
    conventional_sitemap_url.set_path("/sitemap.xml");
    conventional_sitemap_url.set_query(None);
    conventional_sitemap_url.set_fragment(None);
    sitemap_queue.push_back(conventional_sitemap_url);

    let mut visited_indexes = HashSet::new();
    let mut candidate_keys = HashSet::new();
    let mut candidates = Vec::new();
    while let Some(sitemap_url) = sitemap_queue.pop_front() {
        if visited_indexes.len() >= MAX_SOURCE_INDEXES {
            break;
        }
        if !visited_indexes.insert(sitemap_url.as_str().to_string()) {
            continue;
        }

        let body = match fetch_public_source_index_document(
            sitemap_url,
            &seed_url,
            timeout,
            MAX_SOURCE_INDEX_BYTES,
            SourceIndexDocumentKind::Sitemap,
        )
        .await
        {
            Ok(body) => body,
            Err(GeminiInteractionsError::InvalidResponse(_)) => continue,
            Err(error) => return Err(error),
        };
        let parsed = match parse_sitemap(&body) {
            Ok(parsed) => parsed,
            Err(GeminiInteractionsError::InvalidResponse(_)) => continue,
            Err(error) => return Err(error),
        };

        match parsed.kind {
            SitemapDocumentKind::SitemapIndex => {
                for location in parsed.locations {
                    if visited_indexes.len() + sitemap_queue.len() >= MAX_SOURCE_INDEXES {
                        break;
                    }
                    if let Some(url) = parse_same_origin_source_index_url(&location, &seed_url) {
                        let key = url.as_str();
                        if !visited_indexes.contains(key)
                            && !sitemap_queue.iter().any(|queued| queued.as_str() == key)
                        {
                            sitemap_queue.push_back(url);
                        }
                    }
                }
            }
            SitemapDocumentKind::UrlSet => {
                for location in parsed.locations {
                    if candidates.len() >= MAX_SOURCE_INDEX_URLS {
                        break;
                    }
                    if let Some(url) = parse_same_origin_source_index_url(&location, &seed_url) {
                        if candidate_keys.insert(url.as_str().to_string()) {
                            candidates.push(url);
                        }
                    }
                }
            }
        }
    }
    Ok(candidates)
}

async fn fetch_public_source_index_document(
    requested_url: Url,
    seed_url: &Url,
    timeout: Duration,
    max_bytes: usize,
    expected_kind: SourceIndexDocumentKind,
) -> GeminiInteractionsResult<String> {
    validate_public_source_index_url(&requested_url, seed_url)?;
    let mut current_url = requested_url;
    for redirect_count in 0..=MAX_REDIRECTS {
        let resolver = pinned_public_client(&current_url, timeout).await?;
        let accept = match expected_kind {
            SourceIndexDocumentKind::Robots => "text/plain",
            SourceIndexDocumentKind::Sitemap => "application/xml, text/xml;q=0.9",
        };
        let mut response = resolver
            .get(current_url.clone())
            .header(ACCEPT, accept)
            .send()
            .await
            .map_err(GeminiInteractionsError::Transport)?;
        ensure_public_peer(&response)?;

        if redirect_status(response.status()) {
            if redirect_count == MAX_REDIRECTS {
                return Err(GeminiInteractionsError::InvalidUrl(format!(
                    "source index redirect count exceeds {MAX_REDIRECTS}"
                )));
            }
            let location = response
                .headers()
                .get(LOCATION)
                .ok_or_else(|| {
                    GeminiInteractionsError::InvalidUrl(format!(
                        "redirect from {current_url} has no Location header"
                    ))
                })?
                .to_str()
                .map_err(|_| {
                    GeminiInteractionsError::InvalidUrl(format!(
                        "redirect from {current_url} has a non-UTF-8 Location header"
                    ))
                })?;
            current_url = current_url.join(location).map_err(|error| {
                GeminiInteractionsError::InvalidUrl(format!(
                    "invalid redirect target {location:?}: {error}"
                ))
            })?;
            validate_public_source_index_url(&current_url, seed_url)?;
            continue;
        }

        if !response.status().is_success() {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "public source index {current_url} returned HTTP {}",
                response.status()
            )));
        }
        validate_source_index_content_type(response.headers(), expected_kind)?;
        validate_source_content_length(response.headers(), response.content_length(), max_bytes)?;

        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(GeminiInteractionsError::Transport)?
        {
            append_source_body_chunk(&mut body, &chunk, max_bytes)?;
        }
        if body.is_empty() {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "public source index {current_url} returned an empty body"
            )));
        }
        return String::from_utf8(body).map_err(|_| {
            GeminiInteractionsError::InvalidResponse(format!(
                "public source index {current_url} is not valid UTF-8 text"
            ))
        });
    }
    Err(GeminiInteractionsError::InvalidUrl(
        "public source index fetch ended unexpectedly".to_string(),
    ))
}

fn validate_source_index_content_type(
    headers: &HeaderMap,
    expected_kind: SourceIndexDocumentKind,
) -> GeminiInteractionsResult<()> {
    let values = headers.get_all(CONTENT_TYPE).iter().collect::<Vec<_>>();
    if values.len() != 1 {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source index must have exactly one Content-Type header".to_string(),
        ));
    }
    let content_type = values[0].to_str().map_err(|_| {
        GeminiInteractionsError::InvalidResponse(
            "public source index has a non-UTF-8 Content-Type header".to_string(),
        )
    })?;
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let allowed = match expected_kind {
        SourceIndexDocumentKind::Robots => media_type == "text/plain",
        SourceIndexDocumentKind::Sitemap => {
            matches!(media_type.as_str(), "application/xml" | "text/xml")
        }
    };
    if !allowed {
        return Err(GeminiInteractionsError::InvalidResponse(format!(
            "public source index Content-Type {media_type:?} is not allowed for {expected_kind:?}"
        )));
    }
    Ok(())
}

fn parse_robots_sitemap_urls(body: &str, seed_url: &Url) -> Vec<Url> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();
    for line in body.lines() {
        if urls.len() >= MAX_ROBOTS_SITEMAPS {
            break;
        }
        let line = strip_robots_comment(line);
        let Some((directive, value)) = line.split_once(':') else {
            continue;
        };
        if !directive.trim().eq_ignore_ascii_case("sitemap") {
            continue;
        }
        if let Some(url) = parse_same_origin_source_index_url(value, seed_url) {
            if seen.insert(url.as_str().to_string()) {
                urls.push(url);
            }
        }
    }
    urls
}

fn parse_sitemap(body: &str) -> GeminiInteractionsResult<ParsedSitemap> {
    if contains_ascii_case_insensitive(body.as_bytes(), b"<!DOCTYPE")
        || contains_ascii_case_insensitive(body.as_bytes(), b"<!ENTITY")
    {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source sitemap must not contain document type or entity declarations"
                .to_string(),
        ));
    }

    let parser = XmlParserConfig::new()
        .trim_whitespace(false)
        .whitespace_to_characters(true)
        .cdata_to_characters(true)
        .ignore_comments(true)
        .coalesce_characters(true)
        .create_reader(body.as_bytes());
    let mut kind = None;
    let mut element_stack = Vec::new();
    let mut current_location: Option<String> = None;
    let mut locations = Vec::new();
    let mut event_count = 0_usize;

    for event in parser {
        event_count = event_count.checked_add(1).ok_or_else(|| {
            GeminiInteractionsError::InvalidResponse(
                "public source sitemap XML event count overflowed".to_string(),
            )
        })?;
        if event_count > MAX_SOURCE_INDEX_XML_EVENTS {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "public source sitemap XML event count exceeds {MAX_SOURCE_INDEX_XML_EVENTS}"
            )));
        }
        let event = event.map_err(|error| {
            GeminiInteractionsError::InvalidResponse(format!(
                "public source sitemap is not valid XML: {error}"
            ))
        })?;
        match event {
            XmlEvent::StartElement { name, .. } => {
                if current_location.is_some() {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source sitemap loc elements must contain text only".to_string(),
                    ));
                }
                if element_stack.len() >= MAX_SOURCE_INDEX_XML_DEPTH {
                    return Err(GeminiInteractionsError::InvalidResponse(format!(
                        "public source sitemap XML depth exceeds {MAX_SOURCE_INDEX_XML_DEPTH}"
                    )));
                }
                if element_stack.is_empty() {
                    kind = Some(match name.local_name.as_str() {
                        "urlset" => SitemapDocumentKind::UrlSet,
                        "sitemapindex" => SitemapDocumentKind::SitemapIndex,
                        root => {
                            return Err(GeminiInteractionsError::InvalidResponse(format!(
                                "public source sitemap has unsupported root element {root:?}"
                            )));
                        }
                    });
                }
                element_stack.push(name.local_name);
                let expected_entry = match kind {
                    Some(SitemapDocumentKind::UrlSet) => "url",
                    Some(SitemapDocumentKind::SitemapIndex) => "sitemap",
                    None => continue,
                };
                if element_stack.len() == 3
                    && element_stack[1] == expected_entry
                    && element_stack[2] == "loc"
                {
                    current_location = Some(String::new());
                }
            }
            XmlEvent::Characters(text) => {
                if let Some(location) = current_location.as_mut() {
                    let next_length = location.len().checked_add(text.len()).ok_or_else(|| {
                        GeminiInteractionsError::InvalidResponse(
                            "public source sitemap loc length overflowed".to_string(),
                        )
                    })?;
                    if next_length > MAX_SOURCE_INDEX_URL_BYTES {
                        return Err(GeminiInteractionsError::InvalidResponse(format!(
                            "public source sitemap loc exceeds {MAX_SOURCE_INDEX_URL_BYTES} bytes"
                        )));
                    }
                    location.push_str(&text);
                }
            }
            XmlEvent::EndElement { .. } => {
                if current_location.is_some()
                    && element_stack.last().is_some_and(|element| element == "loc")
                {
                    let location = current_location.take().unwrap_or_default();
                    let location = location.trim();
                    if !location.is_empty() && locations.len() < MAX_SOURCE_INDEX_URLS {
                        locations.push(location.to_string());
                    }
                }
                element_stack.pop();
            }
            XmlEvent::ProcessingInstruction { .. } => {
                return Err(GeminiInteractionsError::InvalidResponse(
                    "public source sitemap must not contain processing instructions".to_string(),
                ));
            }
            XmlEvent::StartDocument { .. }
            | XmlEvent::EndDocument
            | XmlEvent::Whitespace(_)
            | XmlEvent::CData(_)
            | XmlEvent::Comment(_) => {}
        }
    }

    let kind = kind.ok_or_else(|| {
        GeminiInteractionsError::InvalidResponse(
            "public source sitemap has no root element".to_string(),
        )
    })?;
    Ok(ParsedSitemap { kind, locations })
}

fn parse_same_origin_source_index_url(value: &str, seed_url: &Url) -> Option<Url> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_SOURCE_INDEX_URL_BYTES {
        return None;
    }
    let url = Url::parse(value).ok()?;
    validate_public_source_index_url(&url, seed_url)
        .ok()
        .map(|_| url)
}

fn strip_robots_comment(line: &str) -> &str {
    let Some(comment_start) = line.find('#') else {
        return line;
    };
    if comment_start == 0
        || line[..comment_start]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
    {
        &line[..comment_start]
    } else {
        line
    }
}

fn validate_public_source_index_url(url: &Url, seed_url: &Url) -> GeminiInteractionsResult<()> {
    validate_public_https_same_origin_url(url, seed_url)?;
    if url.fragment().is_some() {
        return Err(GeminiInteractionsError::InvalidUrl(
            "public source index URLs must not contain fragments".to_string(),
        ));
    }
    Ok(())
}

fn validate_public_same_origin_source_document_url(
    url: &Url,
    seed_url: &Url,
) -> GeminiInteractionsResult<()> {
    validate_public_https_same_origin_url(url, seed_url)?;
    if url.fragment().is_some() {
        return Err(GeminiInteractionsError::InvalidUrl(
            "same-origin public source document URLs must not contain fragments".to_string(),
        ));
    }
    Ok(())
}

fn validate_public_https_same_origin_url(
    url: &Url,
    seed_url: &Url,
) -> GeminiInteractionsResult<()> {
    validate_public_http_url(seed_url)?;
    validate_public_http_url(url)?;
    if seed_url.scheme() != "https" || url.scheme() != "https" {
        return Err(GeminiInteractionsError::InvalidUrl(
            "same-origin public source discovery requires HTTPS".to_string(),
        ));
    }
    if url.origin() != seed_url.origin() {
        return Err(GeminiInteractionsError::InvalidUrl(format!(
            "public source URL {url} is not on seed origin {}",
            seed_url.origin().ascii_serialization()
        )));
    }
    Ok(())
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Preserve the two literal first-party fields that identify a Garmin product
/// as one typed record.
///
/// This is deliberately not part of generic HTML cleanup: the field names are
/// publisher-specific, and accepting arbitrary metadata or JSON-LD would let
/// unrelated pages inject hidden text into source evidence. Duplicate fields,
/// missing content, and oversized/control-bearing values all fail closed.
fn garmin_product_page_identity_record(
    final_url: &Url,
    html: &str,
) -> Option<GarminProductPageIdentityRecord> {
    if final_url.scheme() != "https"
        || final_url.origin().ascii_serialization() != GARMIN_PRODUCT_ORIGIN
    {
        return None;
    }

    let document = Html::parse_document(html);
    let meta_selector = Selector::parse("meta").expect("static meta selector is valid");
    let product_display_name = unique_bounded_literal_meta_content(
        &document,
        &meta_selector,
        "product_display_name",
        MAX_GARMIN_PRODUCT_DISPLAY_NAME_BYTES,
    )?;
    let sku = unique_bounded_literal_meta_content(
        &document,
        &meta_selector,
        "sku0",
        MAX_GARMIN_PRODUCT_SKU_BYTES,
    )?;
    let record = GarminProductPageIdentityRecord {
        product_display_name,
        sku0: sku,
    };
    (record.literal_evidence_text().len() <= MAX_GARMIN_PRODUCT_METADATA_SENTENCE_BYTES)
        .then_some(record)
}

#[cfg(test)]
fn garmin_product_page_metadata_sentence(final_url: &Url, html: &str) -> Option<String> {
    garmin_product_page_identity_record(final_url, html)
        .map(|record| record.literal_evidence_text())
}

fn unique_bounded_literal_meta_content(
    document: &Html,
    meta_selector: &Selector,
    field_name: &str,
    max_bytes: usize,
) -> Option<String> {
    let mut matching = document
        .select(meta_selector)
        .filter(|element| element.value().attr("name") == Some(field_name));
    let element = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    let value = element.value().attr("content")?.trim();
    (!value.is_empty()
        && value.len() <= max_bytes
        && value.chars().count() <= max_bytes
        && value.chars().all(|character| !character.is_control()))
    .then(|| value.to_string())
}

struct CleanedPublicSourceHtml {
    publisher_text: String,
    garmin_product_page_identity: Option<GarminProductPageIdentityRecord>,
}

fn clean_public_source_html(final_url: &Url, html: &str) -> CleanedPublicSourceHtml {
    let publisher_text = crate::html::clean::clean_publisher_source_html(html);
    let garmin_product_page_identity = garmin_product_page_identity_record(final_url, html);
    let publisher_text = match garmin_product_page_identity
        .as_ref()
        .map(GarminProductPageIdentityRecord::literal_evidence_text)
    {
        Some(metadata) if publisher_text.is_empty() => metadata,
        Some(metadata) => format!("{metadata} {publisher_text}"),
        None => publisher_text,
    };
    CleanedPublicSourceHtml {
        publisher_text,
        garmin_product_page_identity,
    }
}

async fn fetch_public_source_document_inner(
    requested_url: Url,
    timeout: Duration,
    max_bytes: usize,
    required_origin: Option<Url>,
) -> GeminiInteractionsResult<FetchedSourceDocument> {
    let mut current_url = requested_url;
    for redirect_count in 0..=MAX_REDIRECTS {
        let resolver = pinned_public_client(&current_url, timeout).await?;
        let mut response = resolver
            .get(current_url.clone())
            .header(
                ACCEPT,
                "text/html, application/xhtml+xml, application/pdf;q=0.9, text/plain;q=0.8",
            )
            .send()
            .await
            .map_err(GeminiInteractionsError::Transport)?;
        ensure_public_peer(&response)?;

        if redirect_status(response.status()) {
            if redirect_count == MAX_REDIRECTS {
                return Err(GeminiInteractionsError::InvalidUrl(format!(
                    "source redirect count exceeds {MAX_REDIRECTS}"
                )));
            }
            let location = response
                .headers()
                .get(LOCATION)
                .ok_or_else(|| {
                    GeminiInteractionsError::InvalidUrl(format!(
                        "redirect from {current_url} has no Location header"
                    ))
                })?
                .to_str()
                .map_err(|_| {
                    GeminiInteractionsError::InvalidUrl(format!(
                        "redirect from {current_url} has a non-UTF-8 Location header"
                    ))
                })?;
            current_url =
                source_document_redirect_target(&current_url, location, required_origin.as_ref())?;
            continue;
        }

        if !response.status().is_success() {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "public source document {current_url} returned HTTP {}",
                response.status()
            )));
        }
        let document_kind = source_document_kind(response.headers())?;
        validate_source_content_length(response.headers(), response.content_length(), max_bytes)?;

        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(GeminiInteractionsError::Transport)?
        {
            append_source_body_chunk(&mut body, &chunk, max_bytes)?;
        }
        if body.is_empty() {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "public source document {current_url} returned an empty body"
            )));
        }
        let content_sha256 = format!("{:x}", Sha256::digest(&body));
        let (publisher_text, garmin_product_page_identity) = match document_kind {
            SourceDocumentKind::Html | SourceDocumentKind::PlainText => {
                let body = String::from_utf8(body).map_err(|_| {
                    GeminiInteractionsError::InvalidResponse(format!(
                        "public source document {current_url} is not valid UTF-8 text"
                    ))
                })?;
                match document_kind {
                    SourceDocumentKind::Html => {
                        let cleaned = clean_public_source_html(&current_url, &body);
                        (cleaned.publisher_text, cleaned.garmin_product_page_identity)
                    }
                    SourceDocumentKind::PlainText => {
                        (body.split_whitespace().collect::<Vec<_>>().join(" "), None)
                    }
                    SourceDocumentKind::Pdf => unreachable!("PDF handled by the outer match"),
                }
            }
            SourceDocumentKind::Pdf => (
                tokio::task::spawn_blocking(move || extract_source_pdf_publisher_text(&body))
                    .await
                    .map_err(|_| {
                        GeminiInteractionsError::InvalidResponse(
                            "public source PDF extraction worker failed".to_string(),
                        )
                    })??,
                None,
            ),
        };
        if publisher_text.is_empty() {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "public source document {current_url} has no publisher text"
            )));
        }
        current_url.set_fragment(None);
        return Ok(FetchedSourceDocument {
            final_url: current_url,
            content_sha256,
            publisher_text,
            garmin_product_page_identity,
        });
    }
    Err(GeminiInteractionsError::InvalidUrl(
        "public source document fetch ended unexpectedly".to_string(),
    ))
}

fn source_document_redirect_target(
    current_url: &Url,
    location: &str,
    required_origin: Option<&Url>,
) -> GeminiInteractionsResult<Url> {
    let mut target = current_url.join(location).map_err(|error| {
        GeminiInteractionsError::InvalidUrl(format!(
            "invalid redirect target {location:?}: {error}"
        ))
    })?;
    target.set_fragment(None);
    match required_origin {
        Some(seed_url) => validate_public_same_origin_source_document_url(&target, seed_url)?,
        None => validate_public_http_url(&target)?,
    }
    Ok(target)
}

#[derive(Clone, Copy)]
struct SourcePdfLimits {
    max_pages: usize,
    max_page_decompressed_bytes: usize,
    max_total_text_bytes: usize,
}

const SOURCE_PDF_LIMITS: SourcePdfLimits = SourcePdfLimits {
    max_pages: MAX_SOURCE_PDF_PAGES,
    max_page_decompressed_bytes: MAX_SOURCE_PDF_PAGE_DECOMPRESSED_BYTES,
    max_total_text_bytes: MAX_SOURCE_PDF_TEXT_BYTES,
};

fn extract_source_pdf_publisher_text(pdf: &[u8]) -> GeminiInteractionsResult<String> {
    extract_source_pdf_publisher_text_with_limits(pdf, SOURCE_PDF_LIMITS)
}

fn extract_source_pdf_publisher_text_with_limits(
    pdf: &[u8],
    limits: SourcePdfLimits,
) -> GeminiInteractionsResult<String> {
    if limits.max_pages == 0
        || limits.max_page_decompressed_bytes == 0
        || limits.max_total_text_bytes == 0
    {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source PDF extraction limits must be positive".to_string(),
        ));
    }
    if !pdf.starts_with(b"%PDF-") {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source PDF is missing the PDF signature".to_string(),
        ));
    }
    let document = Document::load_mem_with_options(
        pdf,
        LoadOptions {
            strict: true,
            max_decompressed_size: Some(limits.max_page_decompressed_bytes),
            ..LoadOptions::default()
        },
    )
    .map_err(|_| {
        GeminiInteractionsError::InvalidResponse(
            "public source PDF failed strict parsing".to_string(),
        )
    })?;
    if document.is_encrypted() || document.was_encrypted() {
        return Err(GeminiInteractionsError::InvalidResponse(
            "encrypted public source PDFs are not allowed".to_string(),
        ));
    }
    let pages = document.get_pages();
    if pages.is_empty() || pages.len() > limits.max_pages {
        return Err(GeminiInteractionsError::InvalidResponse(format!(
            "public source PDF has {} pages; expected 1..={}",
            pages.len(),
            limits.max_pages
        )));
    }

    let mut extracted = String::new();
    let mut total_text_bytes = 0usize;
    for page_number in pages.keys().copied() {
        let text = document
            .extract_text_with_limit(&[page_number], limits.max_page_decompressed_bytes)
            .map_err(|_| {
                GeminiInteractionsError::InvalidResponse(format!(
                    "public source PDF page {page_number} text extraction failed"
                ))
            })?;
        total_text_bytes = total_text_bytes.checked_add(text.len()).ok_or_else(|| {
            GeminiInteractionsError::InvalidResponse(
                "public source PDF extracted text exceeded its byte cap".to_string(),
            )
        })?;
        if total_text_bytes > limits.max_total_text_bytes {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "public source PDF extracted text exceeds {} bytes",
                limits.max_total_text_bytes
            )));
        }
        if !extracted.is_empty() {
            extracted.push('\n');
        }
        extracted.push_str(&text);
    }

    let publisher_text = extracted
        .chars()
        .map(|character| {
            if character.is_control() && !character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if publisher_text.is_empty() {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source PDF has no extractable publisher text".to_string(),
        ));
    }
    Ok(publisher_text)
}

fn source_document_kind(headers: &HeaderMap) -> GeminiInteractionsResult<SourceDocumentKind> {
    let values = headers.get_all(CONTENT_TYPE).iter().collect::<Vec<_>>();
    if values.len() != 1 {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source document must have exactly one Content-Type header".to_string(),
        ));
    }
    let content_type = values[0].to_str().map_err(|_| {
        GeminiInteractionsError::InvalidResponse(
            "public source document has a non-UTF-8 Content-Type header".to_string(),
        )
    })?;
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match media_type.as_str() {
        "text/html" | "application/xhtml+xml" => Ok(SourceDocumentKind::Html),
        "text/plain" => Ok(SourceDocumentKind::PlainText),
        "application/pdf" => Ok(SourceDocumentKind::Pdf),
        _ => Err(GeminiInteractionsError::InvalidResponse(format!(
            "public source document Content-Type {media_type:?} is not allowed"
        ))),
    }
}

fn validate_source_content_length(
    headers: &HeaderMap,
    decoded_length: Option<u64>,
    max_bytes: usize,
) -> GeminiInteractionsResult<()> {
    let lengths = headers.get_all(CONTENT_LENGTH).iter().collect::<Vec<_>>();
    if lengths.len() > 1 {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source document has multiple Content-Length headers".to_string(),
        ));
    }
    if let Some(value) = lengths.first() {
        let declared = value
            .to_str()
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .ok_or_else(|| {
                GeminiInteractionsError::InvalidResponse(
                    "public source document has an invalid Content-Length header".to_string(),
                )
            })?;
        if declared > max_bytes as u64 {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "public source document Content-Length exceeds {max_bytes} bytes"
            )));
        }
    }
    if decoded_length.is_some_and(|length| length > max_bytes as u64) {
        return Err(GeminiInteractionsError::InvalidResponse(format!(
            "public source document decoded length exceeds {max_bytes} bytes"
        )));
    }
    Ok(())
}

fn append_source_body_chunk(
    body: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
) -> GeminiInteractionsResult<()> {
    let next_length = body.len().checked_add(chunk.len()).ok_or_else(|| {
        GeminiInteractionsError::InvalidResponse(
            "public source document body length overflowed".to_string(),
        )
    })?;
    if next_length > max_bytes {
        return Err(GeminiInteractionsError::InvalidResponse(format!(
            "public source document streamed body exceeds {max_bytes} bytes"
        )));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn redirect_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

async fn pinned_public_client(url: &Url, timeout: Duration) -> GeminiInteractionsResult<Client> {
    validate_public_http_url(url)?;
    let port = url.port_or_known_default().ok_or_else(|| {
        GeminiInteractionsError::InvalidUrl(format!("URL {url} has no usable port"))
    })?;
    let Some(host) = url.host() else {
        return Err(GeminiInteractionsError::InvalidUrl(
            "URL must have a host".to_string(),
        ));
    };

    let mut builder = Client::builder()
        .timeout(timeout)
        .redirect(Policy::none())
        .no_proxy();
    if let Host::Domain(domain) = host {
        let lookup = tokio::time::timeout(timeout, tokio::net::lookup_host((domain, port)))
            .await
            .map_err(|_| {
                GeminiInteractionsError::InvalidUrl(format!(
                    "DNS resolution timed out for {domain}"
                ))
            })?
            .map_err(|error| {
                GeminiInteractionsError::InvalidUrl(format!(
                    "DNS resolution failed for {domain}: {error}"
                ))
            })?;
        let addresses = lookup.collect::<Vec<SocketAddr>>();
        if addresses.is_empty() {
            return Err(GeminiInteractionsError::InvalidUrl(format!(
                "DNS returned no addresses for {domain}"
            )));
        }
        if let Some(address) = addresses.iter().find(|address| !public_ip(address.ip())) {
            return Err(GeminiInteractionsError::InvalidUrl(format!(
                "DNS for {domain} returned non-public address {}",
                address.ip()
            )));
        }
        builder = builder.resolve_to_addrs(domain, &addresses);
    }
    builder.build().map_err(GeminiInteractionsError::Transport)
}

fn ensure_public_peer(response: &reqwest::Response) -> GeminiInteractionsResult<()> {
    let peer = response.remote_addr().ok_or_else(|| {
        GeminiInteractionsError::InvalidUrl(
            "source response did not expose its remote address".to_string(),
        )
    })?;
    if !public_ip(peer.ip()) {
        return Err(GeminiInteractionsError::InvalidUrl(format!(
            "source connected to non-public address {}",
            peer.ip()
        )));
    }
    Ok(())
}

pub fn validate_public_http_url(url: &Url) -> GeminiInteractionsResult<()> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(GeminiInteractionsError::InvalidUrl(format!(
            "unsupported URL scheme {}",
            url.scheme()
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(GeminiInteractionsError::InvalidUrl(
            "embedded credentials are not allowed".to_string(),
        ));
    }
    match url.host() {
        Some(Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            if domain == "localhost"
                || domain.ends_with(".localhost")
                || domain.ends_with(".local")
                || domain.ends_with(".internal")
            {
                return Err(GeminiInteractionsError::InvalidUrl(format!(
                    "local hostname {domain} is not allowed"
                )));
            }
        }
        Some(Host::Ipv4(address)) if !public_ipv4(address) => {
            return Err(GeminiInteractionsError::InvalidUrl(format!(
                "non-public IPv4 address {address} is not allowed"
            )));
        }
        Some(Host::Ipv6(address)) if !public_ipv6(address) => {
            return Err(GeminiInteractionsError::InvalidUrl(format!(
                "non-public IPv6 address {address} is not allowed"
            )));
        }
        Some(_) => {}
        None => {
            return Err(GeminiInteractionsError::InvalidUrl(
                "URL must have a host".to_string(),
            ));
        }
    }
    Ok(())
}

fn public_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    !(address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_broadcast()
        || address.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 198 && (18..=19).contains(&octets[1]))
        || octets[0] >= 240)
}

fn public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    let is_ipv4_compatible = segments[..6].iter().all(|segment| *segment == 0);
    let is_nat64_well_known = segments[0] == 0x0064
        && segments[1] == 0xff9b
        && segments[2..6].iter().all(|segment| *segment == 0);
    if address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || segments[0] == 0x2002
        || is_nat64_well_known
        || is_ipv4_compatible
    {
        return false;
    }
    address.to_ipv4_mapped().map(public_ipv4).unwrap_or(true)
}

fn public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => public_ipv4(address),
        IpAddr::V6(address) => public_ipv6(address),
    }
}

fn is_transient_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn is_transient_transport_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_body()
}

fn parse_retry_after(value: Option<&HeaderValue>) -> Option<Duration> {
    value?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn http_error(status: StatusCode, bytes: &[u8]) -> GeminiInteractionsError {
    #[derive(Deserialize)]
    struct ErrorEnvelope {
        error: Option<ApiError>,
    }
    #[derive(Deserialize)]
    struct ApiError {
        message: Option<String>,
        status: Option<String>,
    }

    let parsed = serde_json::from_slice::<ErrorEnvelope>(bytes)
        .ok()
        .and_then(|envelope| envelope.error);
    GeminiInteractionsError::Http {
        status,
        api_status: parsed.as_ref().and_then(|error| error.status.clone()),
        message: parsed
            .as_ref()
            .and_then(|error| error.message.clone())
            .unwrap_or_default(),
        body_excerpt: body_excerpt(bytes),
    }
}

fn body_excerpt(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_ERROR_BODY_BYTES)]).into_owned()
}

fn request_fingerprint(body: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(body))
}

fn interaction_usage_outcome(
    response: &InteractionResponse,
    requested_model: &str,
    service_tier: &str,
    tools: &[InteractionTool],
) -> UsageOutcome {
    let metrics = response
        .interaction
        .usage
        .as_ref()
        .map(|_| interaction_usage_metrics(response))
        .unwrap_or_default();
    let cost = response.interaction.usage.as_ref().and_then(|_| {
        estimate_paid_list_cost(
            requested_model,
            service_tier,
            interaction_tool_use_billing(tools),
            &metrics,
        )
        .ok()
    });
    let status = match &response.interaction.status {
        InteractionStatus::Completed => UsageStatus::Completed,
        InteractionStatus::RequiresAction => UsageStatus::RequiresAction,
        InteractionStatus::Failed => UsageStatus::Failed,
        InteractionStatus::Cancelled => UsageStatus::Cancelled,
        InteractionStatus::Incomplete | InteractionStatus::InProgress => UsageStatus::Incomplete,
        InteractionStatus::BudgetExceeded => UsageStatus::BudgetExceeded,
        InteractionStatus::Other(_) => UsageStatus::Incomplete,
    };
    let mut outcome = UsageOutcome::completed(metrics);
    outcome.status = status;
    outcome.provider_request_id = response.interaction.id.clone();
    outcome.attempt_count = u32::from(response.attempts.max(1));
    outcome.retry_count = outcome.attempt_count.saturating_sub(1);
    outcome.error = match &response.interaction.status {
        InteractionStatus::Completed => None,
        InteractionStatus::Failed => response
            .interaction
            .error
            .as_ref()
            .map(Value::to_string)
            .or_else(|| {
                Some("provider returned failed status without an error payload".to_string())
            }),
        InteractionStatus::Other(value) => response
            .interaction
            .error
            .as_ref()
            .map(Value::to_string)
            .or_else(|| Some(format!("unrecognized provider interaction status {value}"))),
        _ => response.interaction.error.as_ref().map(Value::to_string),
    };
    outcome.cost = cost;
    outcome
}

fn interaction_tool_use_billing(tools: &[InteractionTool]) -> ToolUseBilling {
    let has_google_search = tools
        .iter()
        .any(|tool| matches!(tool, InteractionTool::GoogleSearch));
    let has_chargeable_tool = tools.iter().any(|tool| {
        matches!(
            tool,
            InteractionTool::UrlContext | InteractionTool::Function { .. }
        )
    });
    match (has_google_search, has_chargeable_tool) {
        (false, false) => ToolUseBilling::NoTools,
        (false, true) => ToolUseBilling::ChargeAsUncachedInput,
        (true, false) => ToolUseBilling::GoogleSearchContextExcluded,
        (true, true) => ToolUseBilling::AmbiguousMixed,
    }
}

fn interaction_usage_metrics(response: &InteractionResponse) -> UsageMetrics {
    let usage = response.raw.get("usage");
    let counter = |name: &str| {
        usage
            .and_then(|usage| usage.get(name))
            .and_then(Value::as_u64)
    };
    let modality_sum = |name: &str| {
        usage
            .and_then(|usage| usage.get(name))
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.get("tokens").and_then(Value::as_u64))
                    .sum()
            })
    };
    let search_query_count = usage
        .and_then(|usage| usage.get("grounding_tool_count"))
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| {
                    entry
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind.eq_ignore_ascii_case("google_search"))
                })
                .filter_map(|entry| entry.get("count").and_then(Value::as_u64))
                .sum()
        })
        .or_else(|| {
            // Gemini omits grounding_tool_count entirely for requests that did
            // not use Search. The trace lets us distinguish that known zero
            // from an unknown/malformed counter on a Search response.
            (!response.interaction.steps.iter().any(|step| {
                matches!(
                    step,
                    InteractionStep::GoogleSearchCall(_) | InteractionStep::GoogleSearchResult(_)
                )
            }))
            .then_some(0)
        });
    UsageMetrics {
        input_tokens: counter("total_input_tokens")
            .or_else(|| modality_sum("input_tokens_by_modality")),
        output_tokens: counter("total_output_tokens")
            .or_else(|| modality_sum("output_tokens_by_modality")),
        thought_tokens: counter("total_thought_tokens"),
        cached_tokens: counter("total_cached_tokens")
            .or_else(|| modality_sum("cached_tokens_by_modality")),
        tool_tokens: counter("total_tool_use_tokens")
            .or_else(|| modality_sum("tool_use_tokens_by_modality")),
        search_query_count,
    }
}

#[derive(Clone, Debug)]
pub struct InteractionResponse {
    pub interaction: Interaction,
    /// Exact decoded response for durable audit storage and forward-compatible
    /// inspection of fields not yet modeled by this module.
    pub raw: Value,
    pub attempts: u8,
}

impl std::ops::Deref for InteractionResponse {
    type Target = Interaction;

    fn deref(&self) -> &Self::Target {
        &self.interaction
    }
}

impl InteractionResponse {
    /// Validates the final answer and all tool activity across a stateless
    /// history. `history_before_response` may omit this response; its steps are
    /// always included by this method.
    pub fn require_stateless_curation_output(
        &self,
        history_before_response: &StatelessHistory,
        requirements: &CurationRequirements,
    ) -> GeminiInteractionsResult<String> {
        requirements.validate()?;
        let mut steps = parse_history_steps(history_before_response.steps())?;
        steps.extend(self.interaction.steps.iter().cloned());
        require_tool_activity(&steps, requirements)?;
        self.interaction
            .require_final_output(requirements.grounding != GroundingRequirement::None)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Interaction {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub object: Option<String>,
    pub status: InteractionStatus,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub steps: Vec<InteractionStep>,
    #[serde(default)]
    pub usage: Option<InteractionUsage>,
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Interaction {
    pub fn validate_wire_shape(&self) -> GeminiInteractionsResult<()> {
        if self.id.as_deref().is_some_and(|id| id.trim().is_empty()) {
            return Err(GeminiInteractionsError::InvalidResponse(
                "interaction id is present but empty".to_string(),
            ));
        }
        if self
            .object
            .as_deref()
            .is_some_and(|object| object != "interaction")
        {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "unexpected object type {:?}",
                self.object
            )));
        }

        #[derive(Clone, Copy)]
        enum CallKind<'a> {
            GoogleSearch,
            UrlContext,
            Function(&'a str),
        }

        let mut call_ids = HashMap::new();
        let mut result_call_ids = HashSet::new();
        for step in &self.steps {
            match step {
                InteractionStep::GoogleSearchCall(step) => {
                    validate_step_id("google_search_call", &step.id)?;
                    if call_ids
                        .insert(step.id.as_str(), CallKind::GoogleSearch)
                        .is_some()
                    {
                        return duplicate_step_id(&step.id);
                    }
                }
                InteractionStep::UrlContextCall(step) => {
                    validate_step_id("url_context_call", &step.id)?;
                    if call_ids
                        .insert(step.id.as_str(), CallKind::UrlContext)
                        .is_some()
                    {
                        return duplicate_step_id(&step.id);
                    }
                }
                InteractionStep::FunctionCall(step) => {
                    validate_step_id("function_call", &step.id)?;
                    if step.name.trim().is_empty() {
                        return Err(GeminiInteractionsError::InvalidResponse(
                            "function_call name is empty".to_string(),
                        ));
                    }
                    if !step.arguments.is_object() {
                        return Err(GeminiInteractionsError::InvalidResponse(format!(
                            "function_call {} arguments must be a JSON object",
                            step.id
                        )));
                    }
                    if call_ids
                        .insert(step.id.as_str(), CallKind::Function(step.name.as_str()))
                        .is_some()
                    {
                        return duplicate_step_id(&step.id);
                    }
                }
                InteractionStep::GoogleSearchResult(step) => {
                    validate_step_id("google_search_result.call_id", &step.call_id)?;
                    if !matches!(
                        call_ids.get(step.call_id.as_str()),
                        Some(CallKind::GoogleSearch)
                    ) {
                        return Err(GeminiInteractionsError::InvalidResponse(format!(
                            "google_search_result references an unknown, later, or differently typed call id {}",
                            step.call_id
                        )));
                    }
                    if !result_call_ids.insert(step.call_id.as_str()) {
                        return duplicate_result_id(&step.call_id);
                    }
                }
                InteractionStep::UrlContextResult(step) => {
                    validate_step_id("url_context_result.call_id", &step.call_id)?;
                    if !matches!(
                        call_ids.get(step.call_id.as_str()),
                        Some(CallKind::UrlContext)
                    ) {
                        return Err(GeminiInteractionsError::InvalidResponse(format!(
                            "url_context_result references an unknown, later, or differently typed call id {}",
                            step.call_id
                        )));
                    }
                    if !result_call_ids.insert(step.call_id.as_str()) {
                        return duplicate_result_id(&step.call_id);
                    }
                }
                InteractionStep::FunctionResult(step) => {
                    validate_step_id("function_result.call_id", &step.call_id)?;
                    let Some(CallKind::Function(call_name)) =
                        call_ids.get(step.call_id.as_str()).copied()
                    else {
                        return Err(GeminiInteractionsError::InvalidResponse(format!(
                            "function_result references an unknown, later, or differently typed call id {}",
                            step.call_id
                        )));
                    };
                    if step
                        .name
                        .as_deref()
                        .is_some_and(|result_name| result_name != call_name)
                    {
                        return Err(GeminiInteractionsError::InvalidResponse(format!(
                            "function_result name does not match call {}",
                            step.call_id
                        )));
                    }
                    if !result_call_ids.insert(step.call_id.as_str()) {
                        return duplicate_result_id(&step.call_id);
                    }
                }
                InteractionStep::ModelOutput(step) => {
                    for content in &step.content {
                        if let ContentBlock::Text(text) = content {
                            validate_annotations(text)?;
                        }
                    }
                }
                InteractionStep::UserInput(step) => {
                    for content in &step.content {
                        if let ContentBlock::Text(text) = content {
                            validate_annotations(text)?;
                        }
                    }
                }
                InteractionStep::Thought(_) | InteractionStep::Unknown { .. } => {}
            }
        }
        Ok(())
    }

    pub fn output_text(&self) -> String {
        self.steps
            .iter()
            .filter_map(|step| match step {
                InteractionStep::ModelOutput(output) => Some(&output.content),
                _ => None,
            })
            .flatten()
            .filter_map(|content| match content {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                ContentBlock::Unknown { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn url_citations(&self) -> Vec<UrlCitationRef<'_>> {
        let mut citations = Vec::new();
        for step in &self.steps {
            let InteractionStep::ModelOutput(output) = step else {
                continue;
            };
            for content in &output.content {
                let ContentBlock::Text(text) = content else {
                    continue;
                };
                for annotation in &text.annotations {
                    if let Annotation::UrlCitation(citation) = annotation {
                        citations.push(UrlCitationRef {
                            text: &text.text,
                            citation,
                        });
                    }
                }
            }
        }
        citations
    }

    pub fn function_calls(&self) -> Vec<&FunctionCallStep> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                InteractionStep::FunctionCall(call) => Some(call),
                _ => None,
            })
            .collect()
    }

    pub fn has_function_call(&self, name: &str) -> bool {
        self.function_calls().iter().any(|call| call.name == name)
    }

    pub fn has_successful_google_search(&self) -> bool {
        successful_tool_pair(
            &self.steps,
            |step| match step {
                InteractionStep::GoogleSearchCall(call) => Some(call.id.as_str()),
                _ => None,
            },
            |step| match step {
                InteractionStep::GoogleSearchResult(result) if !result.is_error => {
                    Some(result.call_id.as_str())
                }
                _ => None,
            },
        )
    }

    pub fn has_successful_url_context(&self) -> bool {
        successful_tool_pair(
            &self.steps,
            |step| match step {
                InteractionStep::UrlContextCall(call) => Some(call.id.as_str()),
                _ => None,
            },
            |step| match step {
                InteractionStep::UrlContextResult(result) if !result.is_error => {
                    Some(result.call_id.as_str())
                }
                _ => None,
            },
        )
    }

    pub fn require_curation_output(
        &self,
        grounding: GroundingRequirement,
    ) -> GeminiInteractionsResult<String> {
        let output = self.require_final_output(grounding != GroundingRequirement::None)?;
        require_tool_activity(
            &self.steps,
            &CurationRequirements {
                grounding,
                required_any_function_names: Vec::new(),
            },
        )?;
        Ok(output)
    }

    fn require_final_output(&self, require_citations: bool) -> GeminiInteractionsResult<String> {
        if self.status != InteractionStatus::Completed {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "curation interaction status is {}, not completed",
                self.status
            )));
        }
        if self
            .steps
            .iter()
            .any(|step| matches!(step, InteractionStep::Unknown { .. }))
        {
            return Err(GeminiInteractionsError::InvalidResponse(
                "curation interaction contains an unrecognized step type".to_string(),
            ));
        }
        if has_unknown_output_parts(&self.steps) {
            return Err(GeminiInteractionsError::InvalidResponse(
                "curation interaction contains an unrecognized output content or annotation type"
                    .to_string(),
            ));
        }
        if self.steps.iter().any(
            |step| matches!(step, InteractionStep::ModelOutput(output) if output.error.is_some()),
        ) {
            return Err(GeminiInteractionsError::InvalidResponse(
                "curation interaction contains a model_output error".to_string(),
            ));
        }
        let output = self.output_text();
        if output.trim().is_empty() {
            return Err(GeminiInteractionsError::InvalidResponse(
                "curation interaction has no model output text".to_string(),
            ));
        }
        if require_citations {
            let citations = self.url_citations();
            if citations.is_empty() {
                return Err(GeminiInteractionsError::InvalidResponse(
                    "grounded curation output has no URL citations".to_string(),
                ));
            }
            for citation in citations {
                citation.require_complete()?;
            }
        }
        Ok(output)
    }
}

fn duplicate_step_id(id: &str) -> GeminiInteractionsResult<()> {
    Err(GeminiInteractionsError::InvalidResponse(format!(
        "duplicate tool call id {id}"
    )))
}

fn duplicate_result_id(id: &str) -> GeminiInteractionsResult<()> {
    Err(GeminiInteractionsError::InvalidResponse(format!(
        "duplicate tool result for call id {id}"
    )))
}

fn validate_step_id(field: &str, id: &str) -> GeminiInteractionsResult<()> {
    if id.trim().is_empty() {
        return Err(GeminiInteractionsError::InvalidResponse(format!(
            "{field} is empty"
        )));
    }
    Ok(())
}

fn validate_annotations(text: &TextContent) -> GeminiInteractionsResult<()> {
    for annotation in &text.annotations {
        let Annotation::UrlCitation(citation) = annotation else {
            continue;
        };
        if let (Some(start), Some(end)) = (citation.start_index, citation.end_index) {
            if start > end || text.text.get(start..end).is_none() {
                return Err(GeminiInteractionsError::InvalidResponse(format!(
                    "URL citation range {start}..{end} is invalid for {} response bytes",
                    text.text.len()
                )));
            }
        }
    }
    Ok(())
}

fn successful_tool_pair<'a>(
    steps: &'a [InteractionStep],
    call_id: impl Fn(&'a InteractionStep) -> Option<&'a str>,
    result_call_id: impl Fn(&'a InteractionStep) -> Option<&'a str>,
) -> bool {
    let calls = steps.iter().filter_map(call_id).collect::<HashSet<_>>();
    steps
        .iter()
        .filter_map(result_call_id)
        .any(|result_id| calls.contains(result_id))
}

fn successful_function_pair(steps: &[InteractionStep], allowed_names: &HashSet<&str>) -> bool {
    let calls = steps
        .iter()
        .filter_map(|step| match step {
            InteractionStep::FunctionCall(call) if allowed_names.contains(call.name.as_str()) => {
                Some((call.id.as_str(), call.name.as_str()))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    steps.iter().any(|step| {
        let InteractionStep::FunctionResult(result) = step else {
            return false;
        };
        if result.is_error {
            return false;
        }
        let Some(call_name) = calls.get(result.call_id.as_str()) else {
            return false;
        };
        result
            .name
            .as_deref()
            .is_none_or(|result_name| result_name == *call_name)
    })
}

fn has_unknown_output_parts(steps: &[InteractionStep]) -> bool {
    steps.iter().any(|step| {
        let InteractionStep::ModelOutput(output) = step else {
            return false;
        };
        output.content.iter().any(|content| match content {
            ContentBlock::Unknown { .. } => true,
            ContentBlock::Text(text) => text
                .annotations
                .iter()
                .any(|annotation| matches!(annotation, Annotation::Unknown { .. })),
        })
    })
}

fn require_tool_activity(
    steps: &[InteractionStep],
    requirements: &CurationRequirements,
) -> GeminiInteractionsResult<()> {
    if steps
        .iter()
        .any(|step| matches!(step, InteractionStep::Unknown { .. }))
        || has_unknown_output_parts(steps)
    {
        return Err(GeminiInteractionsError::InvalidResponse(
            "curation trace contains an unrecognized step type".to_string(),
        ));
    }
    if requirements.grounding.requires_google_search()
        && !successful_tool_pair(
            steps,
            |step| match step {
                InteractionStep::GoogleSearchCall(call) => Some(call.id.as_str()),
                _ => None,
            },
            |step| match step {
                InteractionStep::GoogleSearchResult(result) if !result.is_error => {
                    Some(result.call_id.as_str())
                }
                _ => None,
            },
        )
    {
        return Err(GeminiInteractionsError::InvalidResponse(
            "curation trace lacks a successful Google Search call/result pair".to_string(),
        ));
    }
    if requirements.grounding.requires_url_context()
        && !successful_tool_pair(
            steps,
            |step| match step {
                InteractionStep::UrlContextCall(call) => Some(call.id.as_str()),
                _ => None,
            },
            |step| match step {
                InteractionStep::UrlContextResult(result) if !result.is_error => {
                    Some(result.call_id.as_str())
                }
                _ => None,
            },
        )
    {
        return Err(GeminiInteractionsError::InvalidResponse(
            "curation trace lacks a successful URL Context call/result pair".to_string(),
        ));
    }
    if !requirements.required_any_function_names.is_empty() {
        let allowed_names = requirements
            .required_any_function_names
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        if !successful_function_pair(steps, &allowed_names) {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "curation trace lacks a successful local function call/result pair for any of: {}",
                requirements.required_any_function_names.join(", ")
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroundingRequirement {
    None,
    GoogleSearch,
    UrlContext,
    GoogleSearchAndUrlContext,
}

impl GroundingRequirement {
    fn requires_google_search(self) -> bool {
        matches!(self, Self::GoogleSearch | Self::GoogleSearchAndUrlContext)
    }

    fn requires_url_context(self) -> bool {
        matches!(self, Self::UrlContext | Self::GoogleSearchAndUrlContext)
    }
}

impl Default for GroundingRequirement {
    fn default() -> Self {
        Self::None
    }
}

/// Tool evidence that must exist before a curation result is accepted. The
/// function list is an allow-list with "at least one" semantics; acceptance
/// requires both a matching call and a non-error result.
#[derive(Clone, Debug, Default)]
pub struct CurationRequirements {
    pub grounding: GroundingRequirement,
    pub required_any_function_names: Vec<String>,
}

impl CurationRequirements {
    pub fn new(grounding: GroundingRequirement) -> Self {
        Self {
            grounding,
            required_any_function_names: Vec::new(),
        }
    }

    pub fn google_search_and_catalog_functions<I, S>(function_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            grounding: GroundingRequirement::GoogleSearch,
            required_any_function_names: function_names.into_iter().map(Into::into).collect(),
        }
    }

    pub fn with_required_any_function_names<I, S>(mut self, function_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_any_function_names = function_names.into_iter().map(Into::into).collect();
        self
    }

    pub fn validate(&self) -> GeminiInteractionsResult<()> {
        let mut names = HashSet::new();
        for name in &self.required_any_function_names {
            if !valid_function_name(name) {
                return Err(GeminiInteractionsError::InvalidRequest(format!(
                    "invalid required function name {name:?}"
                )));
            }
            if !names.insert(name.as_str()) {
                return Err(GeminiInteractionsError::InvalidRequest(format!(
                    "duplicate required function name {name}"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InteractionStatus {
    InProgress,
    RequiresAction,
    Completed,
    Failed,
    Cancelled,
    Incomplete,
    BudgetExceeded,
    Other(String),
}

impl fmt::Display for InteractionStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InProgress => "in_progress",
            Self::RequiresAction => "requires_action",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Incomplete => "incomplete",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Other(value) => value,
        })
    }
}

impl<'de> Deserialize<'de> for InteractionStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "in_progress" => Self::InProgress,
            "requires_action" => Self::RequiresAction,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "incomplete" => Self::Incomplete,
            "budget_exceeded" => Self::BudgetExceeded,
            _ => Self::Other(value),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct InteractionUsage {
    #[serde(default)]
    pub cached_tokens_by_modality: Vec<ModalityTokens>,
    #[serde(default)]
    pub grounding_tool_count: Vec<GroundingToolCount>,
    #[serde(default)]
    pub input_tokens_by_modality: Vec<ModalityTokens>,
    #[serde(default)]
    pub output_tokens_by_modality: Vec<ModalityTokens>,
    #[serde(default)]
    pub tool_use_tokens_by_modality: Vec<ModalityTokens>,
    #[serde(default)]
    pub total_cached_tokens: u64,
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
    #[serde(default)]
    pub total_thought_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub total_tool_use_tokens: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModalityTokens {
    #[serde(default)]
    pub modality: String,
    #[serde(default)]
    pub tokens: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct GroundingToolCount {
    #[serde(rename = "type", default)]
    pub tool_type: String,
    #[serde(default)]
    pub count: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub enum InteractionStep {
    UserInput(UserInputStep),
    ModelOutput(ModelOutputStep),
    Thought(ThoughtStep),
    GoogleSearchCall(GoogleSearchCallStep),
    GoogleSearchResult(GoogleSearchResultStep),
    UrlContextCall(UrlContextCallStep),
    UrlContextResult(UrlContextResultStep),
    FunctionCall(FunctionCallStep),
    FunctionResult(FunctionResultStep),
    Unknown { step_type: String, raw: Value },
}

impl<'de> Deserialize<'de> for InteractionStep {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        let step_type = discriminator(&raw).map_err(D::Error::custom)?.to_string();
        match step_type.as_str() {
            "user_input" => serde_json::from_value::<UserInputStep>(raw)
                .map(Self::UserInput)
                .map_err(D::Error::custom),
            "model_output" => serde_json::from_value::<ModelOutputStep>(raw)
                .map(Self::ModelOutput)
                .map_err(D::Error::custom),
            "thought" => serde_json::from_value::<ThoughtStep>(raw)
                .map(Self::Thought)
                .map_err(D::Error::custom),
            "google_search_call" => serde_json::from_value::<GoogleSearchCallStep>(raw)
                .map(Self::GoogleSearchCall)
                .map_err(D::Error::custom),
            "google_search_result" => serde_json::from_value::<GoogleSearchResultStep>(raw)
                .map(Self::GoogleSearchResult)
                .map_err(D::Error::custom),
            "url_context_call" => serde_json::from_value::<UrlContextCallStep>(raw)
                .map(Self::UrlContextCall)
                .map_err(D::Error::custom),
            "url_context_result" => serde_json::from_value::<UrlContextResultStep>(raw)
                .map(Self::UrlContextResult)
                .map_err(D::Error::custom),
            "function_call" => serde_json::from_value::<FunctionCallStep>(raw)
                .map(Self::FunctionCall)
                .map_err(D::Error::custom),
            "function_result" => serde_json::from_value::<FunctionResultStep>(raw)
                .map(Self::FunctionResult)
                .map_err(D::Error::custom),
            _ => Ok(Self::Unknown { step_type, raw }),
        }
    }
}

fn discriminator(raw: &Value) -> Result<&str, &'static str> {
    raw.as_object()
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str)
        .ok_or("typed Gemini object is missing a string type discriminator")
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserInputStep {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelOutputStep {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ThoughtStep {
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub summary: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct GoogleSearchArguments {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub queries: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl GoogleSearchArguments {
    pub fn all_queries(&self) -> Vec<&str> {
        self.query
            .iter()
            .map(String::as_str)
            .chain(self.queries.iter().map(String::as_str))
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct GoogleSearchCallStep {
    pub id: String,
    #[serde(default)]
    pub arguments: GoogleSearchArguments,
    #[serde(default)]
    pub search_type: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GoogleSearchResultStep {
    pub call_id: String,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct UrlContextArguments {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UrlContextCallStep {
    pub id: String,
    #[serde(default)]
    pub arguments: UrlContextArguments,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UrlContextResultStep {
    pub call_id: String,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FunctionCallStep {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FunctionResultStep {
    pub call_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    pub result: Value,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub enum ContentBlock {
    Text(TextContent),
    Unknown { content_type: String, raw: Value },
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        let content_type = discriminator(&raw).map_err(D::Error::custom)?.to_string();
        match content_type.as_str() {
            "text" => serde_json::from_value(raw)
                .map(Self::Text)
                .map_err(D::Error::custom),
            _ => Ok(Self::Unknown { content_type, raw }),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct TextContent {
    pub text: String,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub enum Annotation {
    UrlCitation(UrlCitation),
    Unknown { annotation_type: String, raw: Value },
}

impl<'de> Deserialize<'de> for Annotation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        let annotation_type = discriminator(&raw).map_err(D::Error::custom)?.to_string();
        match annotation_type.as_str() {
            "url_citation" => serde_json::from_value(raw)
                .map(Self::UrlCitation)
                .map_err(D::Error::custom),
            _ => Ok(Self::Unknown {
                annotation_type,
                raw,
            }),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct UrlCitation {
    #[serde(default)]
    pub start_index: Option<usize>,
    #[serde(default)]
    pub end_index: Option<usize>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Copy, Debug)]
pub struct UrlCitationRef<'a> {
    pub text: &'a str,
    pub citation: &'a UrlCitation,
}

impl UrlCitationRef<'_> {
    pub fn cited_text(&self) -> GeminiInteractionsResult<&str> {
        let start = self.citation.start_index.ok_or_else(|| {
            GeminiInteractionsError::InvalidResponse(
                "URL citation is missing start_index".to_string(),
            )
        })?;
        let end = self.citation.end_index.ok_or_else(|| {
            GeminiInteractionsError::InvalidResponse(
                "URL citation is missing end_index".to_string(),
            )
        })?;
        self.text.get(start..end).ok_or_else(|| {
            GeminiInteractionsError::InvalidResponse(format!(
                "URL citation range {start}..{end} is not a valid UTF-8 byte range"
            ))
        })
    }

    pub fn require_complete(&self) -> GeminiInteractionsResult<()> {
        let url = self.citation.url.as_deref().ok_or_else(|| {
            GeminiInteractionsError::InvalidResponse("URL citation is missing url".to_string())
        })?;
        let parsed = Url::parse(url).map_err(|error| {
            GeminiInteractionsError::InvalidResponse(format!(
                "URL citation has invalid url {url:?}: {error}"
            ))
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(GeminiInteractionsError::InvalidResponse(format!(
                "URL citation has unsupported scheme {}",
                parsed.scheme()
            )));
        }
        let _ = self.cited_text()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Object, Stream};
    use serde_json::json;

    fn response_from(raw: Value) -> InteractionResponse {
        let interaction: Interaction = serde_json::from_value(raw.clone()).unwrap();
        interaction.validate_wire_shape().unwrap();
        InteractionResponse {
            interaction,
            raw,
            attempts: 1,
        }
    }

    fn grounded_fixture() -> Value {
        json!({
            "id": "interaction-1",
            "model": "gemini-3.5-flash",
            "object": "interaction",
            "status": "completed",
            "steps": [
                {
                    "type": "google_search_call",
                    "id": "search-1",
                    "arguments": {"queries": ["Cessna 182T 2023 standard avionics"]}
                },
                {
                    "type": "google_search_result",
                    "call_id": "search-1",
                    "result": {"search_suggestions": "<div>sources</div>"}
                },
                {
                    "type": "url_context_call",
                    "id": "url-1",
                    "arguments": {"urls": ["https://cessna.txtav.com/"]}
                },
                {
                    "type": "url_context_result",
                    "call_id": "url-1",
                    "result": [{"url": "https://cessna.txtav.com/", "status": "success"}]
                },
                {
                    "type": "thought",
                    "summary": [{"type": "text", "text": "Checking the factory source."}]
                },
                {
                    "type": "model_output",
                    "content": [{
                        "type": "text",
                        "text": "The 182T uses G1000 NXi.",
                        "annotations": [{
                            "type": "url_citation",
                            "start_index": 14,
                            "end_index": 23,
                            "url": "https://cessna.txtav.com/",
                            "title": "Cessna Skylane"
                        }]
                    }]
                }
            ],
            "usage": {
                "cached_tokens_by_modality": [{"modality": "text", "tokens": 5}],
                "grounding_tool_count": [{"type": "google_search", "count": 1}],
                "input_tokens_by_modality": [{"modality": "text", "tokens": 100}],
                "output_tokens_by_modality": [{"modality": "text", "tokens": 25}],
                "tool_use_tokens_by_modality": [{"modality": "text", "tokens": 15}],
                "total_input_tokens": 100,
                "total_output_tokens": 25,
                "total_thought_tokens": 10,
                "total_tool_use_tokens": 15,
                "total_tokens": 150
            }
        })
    }

    #[test]
    fn serializes_validated_high_resolution_inline_image_input() {
        let input = InteractionInput::multimodal(vec![
            InteractionInputItem::text("Read the visible tail number.").unwrap(),
            InteractionInputItem::inline_image("image/jpeg", &[0xff, 0xd8, 0xff]).unwrap(),
        ])
        .unwrap();
        let request = CreateInteractionRequest::new("gemini-3.6-flash", input);
        request.validate().unwrap();
        let wire = serde_json::to_value(request.wire_request()).unwrap();
        assert_eq!(wire["input"][0]["type"], "text");
        assert_eq!(wire["input"][1]["type"], "image");
        assert_eq!(wire["input"][1]["mime_type"], "image/jpeg");
        assert_eq!(wire["input"][1]["data"], "/9j/");
        assert_eq!(wire["input"][1]["resolution"], "high");
    }

    #[test]
    fn serializes_service_tier_but_never_accounting_context() {
        let request = CreateInteractionRequest::new("gemini-3.5-flash", "Inspect this aircraft")
            .with_service_tier("flex")
            .with_accounting_context(
                InteractionAccountingContext::new(
                    GeminiTask::AircraftSearchGrounding,
                    "fixture_search",
                )
                .with_correlation_id("fixture-1")
                .with_listing_id(42)
                .with_source("aircraft_hierarchy_case", "case-1"),
            );
        request.validate().unwrap();
        let wire = serde_json::to_value(request.wire_request()).unwrap();
        assert_eq!(wire["service_tier"], "flex");
        assert!(wire.get("accounting_context").is_none());

        let omitted = CreateInteractionRequest::new("gemini-3.5-flash", "input")
            .with_service_tier("unspecified");
        let omitted_wire = serde_json::to_value(omitted.wire_request()).unwrap();
        assert!(omitted_wire.get("service_tier").is_none());
    }

    #[test]
    fn maps_full_interaction_usage_and_withholds_ambiguous_mixed_tool_cost() {
        let response = response_from(grounded_fixture());
        let metrics = interaction_usage_metrics(&response);
        assert_eq!(metrics.input_tokens, Some(100));
        assert_eq!(metrics.output_tokens, Some(25));
        assert_eq!(metrics.thought_tokens, Some(10));
        assert_eq!(metrics.cached_tokens, Some(5));
        assert_eq!(metrics.tool_tokens, Some(15));
        assert_eq!(metrics.search_query_count, Some(1));

        let outcome = interaction_usage_outcome(
            &response,
            "gemini-3.5-flash",
            "standard",
            &[InteractionTool::GoogleSearch, InteractionTool::UrlContext],
        );
        assert_eq!(outcome.status, UsageStatus::Completed);
        assert_eq!(
            outcome.provider_request_id.as_deref(),
            Some("interaction-1")
        );
        assert!(outcome.cost.is_none());
    }

    #[test]
    fn distinguishes_search_url_and_custom_function_tool_billing() {
        let response = response_from(grounded_fixture());

        let search_only = interaction_usage_outcome(
            &response,
            "gemini-3.5-flash",
            "standard",
            &[InteractionTool::GoogleSearch],
        )
        .cost
        .unwrap();
        assert_eq!(
            search_only.pricing_snapshot["tool_use_billing"],
            "google_search_context_excluded"
        );
        assert_eq!(
            search_only.pricing_snapshot["billable_usage"]["billed_tool_use_input_tokens"],
            0
        );
        assert_eq!(
            search_only.pricing_snapshot["billable_usage"]["excluded_google_search_context_tokens"],
            15
        );

        let url_context = interaction_usage_outcome(
            &response,
            "gemini-3.5-flash",
            "standard",
            &[InteractionTool::UrlContext],
        )
        .cost
        .unwrap();
        assert_eq!(
            url_context.pricing_snapshot["tool_use_billing"],
            "charge_as_uncached_input"
        );
        assert_eq!(
            url_context.pricing_snapshot["billable_usage"]["uncached_input_tokens"],
            110
        );
        assert_eq!(
            url_context.pricing_snapshot["billable_usage"]["billed_tool_use_input_tokens"],
            15
        );
        assert!(url_context.total_microusd > search_only.total_microusd);

        let function = InteractionTool::function(
            "lookup_product",
            "Look up one product",
            json!({"type": "object", "properties": {}}),
        )
        .unwrap();
        assert_eq!(
            interaction_tool_use_billing(&[function]),
            ToolUseBilling::ChargeAsUncachedInput
        );
    }

    #[test]
    fn accounts_non_search_interactions_with_zero_search_queries() {
        let response = response_from(json!({
            "id": "interaction-structure",
            "model": "gemini-3.5-flash",
            "object": "interaction",
            "status": "completed",
            "steps": [
                {"type": "model_output", "content": [{"type": "text", "text": "{}"}]}
            ],
            "usage": {
                "total_input_tokens": 100,
                "total_output_tokens": 25,
                "total_thought_tokens": 0,
                "total_cached_tokens": 0,
                "total_tool_use_tokens": 0
            }
        }));
        let metrics = interaction_usage_metrics(&response);
        assert_eq!(metrics.search_query_count, Some(0));
        assert!(
            interaction_usage_outcome(&response, "gemini-3.5-flash", "standard", &[])
                .cost
                .is_some()
        );
    }

    #[test]
    fn accepts_image_content_echoed_in_completed_interaction_history() {
        let response = response_from(json!({
            "id": "interaction-vision",
            "model": "gemini-3.6-flash",
            "object": "interaction",
            "status": "completed",
            "steps": [
                {
                    "type": "user_input",
                    "content": [
                        {"type": "text", "text": "Read the tail number."},
                        {
                            "type": "image",
                            "mime_type": "image/jpeg",
                            "data": "/9j/",
                            "resolution": "high"
                        }
                    ]
                },
                {
                    "type": "model_output",
                    "content": [{"type": "text", "text": "{\"status\":\"no_explicit_identifier_visible\"}"}]
                }
            ]
        }));
        assert_eq!(
            response
                .interaction
                .require_curation_output(GroundingRequirement::None)
                .unwrap(),
            "{\"status\":\"no_explicit_identifier_visible\"}"
        );
    }

    #[test]
    fn serializes_stateless_curation_request() {
        let function = InteractionTool::function(
            "find_aircraft_variants",
            "Returns read-only approved candidates.",
            json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
                "additionalProperties": false
            }),
        )
        .unwrap();
        let request = CreateInteractionRequest::for_aircraft_curation("Resolve this aircraft")
            .with_system_instruction("Use only cited evidence.")
            .with_tool(InteractionTool::GoogleSearch)
            .with_tool(InteractionTool::UrlContext)
            .with_tool(function)
            .with_response_format(
                ResponseFormat::json(json!({
                    "type": "object",
                    "properties": {"decision": {"type": "string"}},
                    "required": ["decision"],
                    "additionalProperties": false
                }))
                .unwrap(),
            )
            .with_generation_config(GenerationConfig {
                max_output_tokens: Some(2048),
                thinking_level: Some(ThinkingLevel::Low),
                tool_choice: Some(ToolChoice::Validated),
                ..GenerationConfig::default()
            });
        request.validate().unwrap();

        let wire = serde_json::to_value(request.wire_request()).unwrap();
        assert_eq!(wire["store"], false);
        assert_eq!(wire["stream"], false);
        assert_eq!(wire["background"], false);
        assert_eq!(wire["model"], DEFAULT_GEMINI_CURATION_MODEL);
        assert_eq!(wire["generation_config"]["tool_choice"], "validated");
        assert_eq!(wire["tools"][0]["type"], "google_search");
        assert_eq!(wire["tools"][1]["type"], "url_context");
        assert_eq!(wire["tools"][2]["type"], "function");
        assert_eq!(wire["response_format"]["mime_type"], "application/json");
        assert!(wire.get("previous_interaction_id").is_none());
    }

    #[test]
    fn preserves_stateless_steps_and_enforces_search_plus_catalog_function() {
        let mut history = StatelessHistory::new("Resolve this Cessna 182 listing").unwrap();
        let first = response_from(json!({
            "id": "interaction-tools",
            "model": "gemini-3.5-flash",
            "object": "interaction",
            "status": "requires_action",
            "steps": [
                {
                    "type": "thought",
                    "signature": "thought-signature",
                    "summary": [{"type": "text", "text": "Check current sources and catalog."}]
                },
                {
                    "type": "google_search_call",
                    "id": "search-1",
                    "arguments": {"queries": ["Cessna 182T 2023 equipment"]},
                    "signature": "search-call-signature"
                },
                {
                    "type": "google_search_result",
                    "call_id": "search-1",
                    "result": {"search_suggestions": "sources"},
                    "signature": "search-result-signature"
                },
                {
                    "type": "function_call",
                    "id": "catalog-1",
                    "name": "find_aircraft_variants",
                    "arguments": {"maker": "Cessna", "model": "182T"},
                    "signature": "catalog-call-signature"
                }
            ]
        }));

        history.append_response(&first).unwrap();
        assert_eq!(history.steps()[1]["signature"], "thought-signature");
        assert_eq!(history.steps()[4]["signature"], "catalog-call-signature");

        let requirements = CurationRequirements::google_search_and_catalog_functions([
            "find_aircraft_variants",
            "get_aircraft_entity",
        ]);
        assert!(history.require_tool_activity(&requirements).is_err());

        let call = first.function_calls()[0].clone();
        history
            .append_function_result(&call, json!({"id": 42, "canonical_variant": "182T"}))
            .unwrap();
        history.require_tool_activity(&requirements).unwrap();

        let continuation = CreateInteractionRequest::for_aircraft_curation(&history)
            .with_tool(InteractionTool::GoogleSearch)
            .with_tool(
                InteractionTool::function(
                    "find_aircraft_variants",
                    "Returns approved aircraft variant candidates.",
                    json!({
                        "type": "object",
                        "properties": {"query": {"type": "string"}},
                        "required": ["query"]
                    }),
                )
                .unwrap(),
            )
            .with_tool(
                InteractionTool::function(
                    "get_aircraft_entity",
                    "Returns one approved catalog entity.",
                    json!({
                        "type": "object",
                        "properties": {"id": {"type": "integer"}},
                        "required": ["id"]
                    }),
                )
                .unwrap(),
            );
        continuation.validate_for_curation(&requirements).unwrap();
        let wire = serde_json::to_value(continuation.wire_request()).unwrap();
        assert!(wire["input"].is_array());
        assert_eq!(wire["input"][1]["signature"], "thought-signature");
        assert_eq!(wire["input"][5]["type"], "function_result");
        assert_eq!(wire["input"][5]["result"][0]["type"], "text");

        let final_response = response_from(json!({
            "id": "interaction-final",
            "model": "gemini-3.5-flash",
            "object": "interaction",
            "status": "completed",
            "steps": [{
                "type": "model_output",
                "content": [{
                    "type": "text",
                    "text": "Factory G1000 NXi.",
                    "annotations": [{
                        "type": "url_citation",
                        "start_index": 8,
                        "end_index": 17,
                        "url": "https://cessna.txtav.com/",
                        "title": "Cessna"
                    }]
                }]
            }]
        }));
        assert_eq!(
            final_response
                .require_stateless_curation_output(&history, &requirements)
                .unwrap(),
            "Factory G1000 NXi."
        );
    }

    #[test]
    fn rejects_invalid_or_duplicate_stateless_function_results() {
        let call: FunctionCallStep = serde_json::from_value(json!({
            "id": "catalog-1",
            "name": "find_aircraft_variants",
            "arguments": {"query": "182T"}
        }))
        .unwrap();
        let mut missing_call = StatelessHistory::new("Resolve aircraft").unwrap();
        assert!(missing_call
            .append_function_result(&call, json!({"matches": []}))
            .is_err());

        let first = response_from(json!({
            "id": "interaction-call",
            "object": "interaction",
            "status": "requires_action",
            "steps": [{
                "type": "function_call",
                "id": "catalog-1",
                "name": "find_aircraft_variants",
                "arguments": {"query": "182T"},
                "signature": "signed"
            }]
        }));
        let mut history = StatelessHistory::new("Resolve aircraft").unwrap();
        history.append_response(&first).unwrap();
        history
            .append_function_result(&call, json!({"matches": []}))
            .unwrap();
        assert!(history
            .append_function_result(&call, json!({"matches": []}))
            .is_err());
    }

    #[test]
    fn rejects_unpinned_model_and_duplicate_tools() {
        let latest = CreateInteractionRequest::new("gemini-flash-latest", "input");
        assert!(latest.validate().is_err());

        let duplicate = CreateInteractionRequest::new("gemini-3.5-flash", "input")
            .with_tool(InteractionTool::GoogleSearch)
            .with_tool(InteractionTool::GoogleSearch);
        assert!(duplicate.validate().is_err());

        let combined_auto = CreateInteractionRequest::new("gemini-3.5-flash", "input")
            .with_tool(InteractionTool::GoogleSearch)
            .with_tool(
                InteractionTool::function(
                    "find_aircraft_variants",
                    "Find variants.",
                    json!({"type": "object", "properties": {}}),
                )
                .unwrap(),
            )
            .with_generation_config(GenerationConfig {
                tool_choice: Some(ToolChoice::Auto),
                ..GenerationConfig::default()
            });
        assert!(combined_auto.validate().is_err());
    }

    #[test]
    fn parses_grounding_steps_citations_status_and_usage() {
        let interaction: Interaction = serde_json::from_value(grounded_fixture()).unwrap();
        interaction.validate_wire_shape().unwrap();
        assert_eq!(interaction.status, InteractionStatus::Completed);
        assert!(interaction.has_successful_google_search());
        assert!(interaction.has_successful_url_context());
        assert_eq!(
            interaction
                .require_curation_output(GroundingRequirement::GoogleSearchAndUrlContext)
                .unwrap(),
            "The 182T uses G1000 NXi."
        );
        let citations = interaction.url_citations();
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].cited_text().unwrap(), "G1000 NXi");
        let usage = interaction.usage.unwrap();
        assert_eq!(usage.total_tokens, 150);
        assert_eq!(usage.output_tokens_by_modality[0].tokens, 25);
        assert_eq!(usage.grounding_tool_count[0].count, 1);
    }

    #[test]
    fn parses_function_call_without_treating_it_as_completed() {
        let interaction: Interaction = serde_json::from_value(json!({
            "id": "interaction-2",
            "model": "gemini-3.5-flash",
            "object": "interaction",
            "status": "requires_action",
            "steps": [{
                "type": "function_call",
                "id": "call-1",
                "name": "find_aircraft_variants",
                "arguments": {"query": "182 I"}
            }]
        }))
        .unwrap();
        interaction.validate_wire_shape().unwrap();
        assert_eq!(interaction.function_calls().len(), 1);
        assert!(interaction
            .require_curation_output(GroundingRequirement::None)
            .is_err());
    }

    #[test]
    fn rejects_malformed_known_steps_and_citation_ranges() {
        let missing_id = json!({
            "id": "bad-step",
            "object": "interaction",
            "status": "completed",
            "steps": [{
                "type": "google_search_call",
                "arguments": {"query": "query"}
            }]
        });
        assert!(serde_json::from_value::<Interaction>(missing_id).is_err());

        let mut bad_range = grounded_fixture();
        bad_range["steps"][5]["content"][0]["annotations"][0]["end_index"] = json!(500);
        let interaction: Interaction = serde_json::from_value(bad_range).unwrap();
        assert!(interaction.validate_wire_shape().is_err());
    }

    #[test]
    fn grounding_requires_a_call_result_pair_and_citations() {
        let mut missing_result = grounded_fixture();
        missing_result["steps"].as_array_mut().unwrap().remove(1);
        let interaction: Interaction = serde_json::from_value(missing_result).unwrap();
        assert!(!interaction.has_successful_google_search());
        assert!(interaction
            .require_curation_output(GroundingRequirement::GoogleSearch)
            .is_err());

        let mut missing_citation = grounded_fixture();
        missing_citation["steps"][5]["content"][0]["annotations"] = json!([]);
        let interaction: Interaction = serde_json::from_value(missing_citation).unwrap();
        assert!(interaction
            .require_curation_output(GroundingRequirement::GoogleSearch)
            .is_err());
    }

    #[test]
    fn retry_policy_is_bounded_and_retries_only_transient_statuses() {
        let policy =
            RetryPolicy::new(4, Duration::from_millis(100), Duration::from_millis(250)).unwrap();
        assert_eq!(policy.delay_after(1, None), Duration::from_millis(100));
        assert_eq!(policy.delay_after(2, None), Duration::from_millis(200));
        assert_eq!(policy.delay_after(3, None), Duration::from_millis(250));
        assert_eq!(
            policy.delay_after(1, Some(Duration::from_secs(30))),
            Duration::from_millis(250)
        );
        assert!(is_transient_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_transient_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!is_transient_status(StatusCode::BAD_REQUEST));
        assert!(!is_transient_status(StatusCode::FORBIDDEN));
    }

    #[test]
    fn source_url_validation_blocks_common_ssrf_targets() {
        for source in [
            "file:///etc/passwd",
            "http://localhost/admin",
            "http://127.0.0.1/admin",
            "http://10.0.0.1/admin",
            "http://240.0.0.1/admin",
            "http://255.0.0.1/admin",
            "http://[::1]/admin",
            "http://[::c0a8:1]/admin",
            "http://[64:ff9b::7f00:1]/admin",
            "http://[2002:7f00:1::]/admin",
            "http://[fec0::1]/admin",
            "http://[::ffff:127.0.0.1]/admin",
            "https://user:password@example.com/",
        ] {
            let url = Url::parse(source).unwrap();
            assert!(validate_public_http_url(&url).is_err(), "{source}");
        }
        assert!(validate_public_http_url(&Url::parse("https://www.faa.gov/").unwrap()).is_ok());
        assert!(
            validate_public_http_url(&Url::parse("https://[::ffff:8.8.8.8]/").unwrap()).is_ok()
        );
    }

    #[test]
    fn source_document_content_types_accept_bounded_pdf_and_reject_unknown_media() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        assert_eq!(
            source_document_kind(&headers).unwrap(),
            SourceDocumentKind::Html
        );

        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/xhtml+xml"),
        );
        assert_eq!(
            source_document_kind(&headers).unwrap(),
            SourceDocumentKind::Html
        );

        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert_eq!(
            source_document_kind(&headers).unwrap(),
            SourceDocumentKind::PlainText
        );

        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/pdf"));
        assert_eq!(
            source_document_kind(&headers).unwrap(),
            SourceDocumentKind::Pdf
        );
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        assert!(source_document_kind(&headers).is_err());
        headers.remove(CONTENT_TYPE);
        assert!(source_document_kind(&headers).is_err());
    }

    #[test]
    fn garmin_product_page_metadata_is_prepended_as_one_literal_bounded_pair() {
        let url = Url::parse("https://www.garmin.com/en-US/p/14664/").unwrap();
        let html = r#"
            <html>
              <head>
                <meta name="product_display_name" content="GTS™ 800 TAS">
                <meta name="sku0" content="010-00519-00">
                <script type="application/ld+json">
                  {"name":"DO_NOT_INGEST_SCRIPT_PRODUCT","sku":"DO-NOT-INGEST"}
                </script>
              </head>
              <body>Visible Garmin product description.</body>
            </html>
        "#;

        let cleaned = clean_public_source_html(&url, html);
        let record = cleaned
            .garmin_product_page_identity
            .expect("the unique exact-origin pair should become a typed record");

        assert!(cleaned.publisher_text.starts_with(
            "Garmin product page metadata: product_display_name GTS™ 800 TAS; sku0 010-00519-00."
        ));
        assert!(cleaned
            .publisher_text
            .contains("Visible Garmin product description."));
        assert!(!cleaned.publisher_text.contains("DO_NOT_INGEST"));
        assert_eq!(record.product_display_name, "GTS™ 800 TAS");
        assert_eq!(record.sku0, "010-00519-00");
        assert!(
            garmin_product_page_metadata_sentence(&url, html)
                .unwrap()
                .len()
                <= MAX_GARMIN_PRODUCT_METADATA_SENTENCE_BYTES
        );
    }

    #[test]
    fn garmin_product_page_metadata_rejects_duplicate_or_conflicting_fields() {
        let url = Url::parse("https://www.garmin.com/en-US/p/14664/").unwrap();
        let duplicate = r#"
            <meta name="product_display_name" content="GTS™ 800 TAS">
            <meta name="product_display_name" content="GTS™ 800 TAS">
            <meta name="sku0" content="010-00519-00">
        "#;
        let conflicting = r#"
            <meta name="product_display_name" content="GTS™ 800 TAS">
            <meta name="sku0" content="010-00519-00">
            <meta name="sku0" content="010-99999-99">
        "#;

        assert!(garmin_product_page_metadata_sentence(&url, duplicate).is_none());
        assert!(garmin_product_page_metadata_sentence(&url, conflicting).is_none());
    }

    #[test]
    fn garmin_product_page_metadata_requires_both_nonempty_bounded_fields() {
        let url = Url::parse("https://www.garmin.com/en-US/p/14664/").unwrap();
        for html in [
            r#"<meta name="product_display_name" content="GTS™ 800 TAS">"#,
            r#"<meta name="sku0" content="010-00519-00">"#,
            r#"
                <meta name="product_display_name" content="">
                <meta name="sku0" content="010-00519-00">
            "#,
            r#"
                <meta name="product_display_name" content="GTS™ 800 TAS">
                <meta name="sku0">
            "#,
        ] {
            assert!(
                garmin_product_page_metadata_sentence(&url, html).is_none(),
                "{html}"
            );
        }
        let oversized_name = format!(
            r#"<meta name="product_display_name" content="{}"><meta name="sku0" content="010-00519-00">"#,
            "X".repeat(MAX_GARMIN_PRODUCT_DISPLAY_NAME_BYTES + 1)
        );
        assert!(garmin_product_page_metadata_sentence(&url, &oversized_name).is_none());
    }

    #[test]
    fn garmin_product_page_metadata_never_applies_outside_the_exact_https_origin() {
        let html = r#"
            <meta name="product_display_name" content="GTS™ 800 TAS">
            <meta name="sku0" content="010-00519-00">
            <body>Visible publisher text.</body>
        "#;
        for url in [
            "http://www.garmin.com/en-US/p/14664/",
            "https://garmin.com/en-US/p/14664/",
            "https://aviation.garmin.com/en-US/p/14664/",
            "https://www.garmin.com.evil.example/en-US/p/14664/",
            "https://www.garmin.com:444/en-US/p/14664/",
        ] {
            let url = Url::parse(url).unwrap();
            assert!(
                garmin_product_page_metadata_sentence(&url, html).is_none(),
                "{url}"
            );
            let cleaned = clean_public_source_html(&url, html);
            assert_eq!(cleaned.publisher_text, "Visible publisher text.");
            assert!(cleaned.garmin_product_page_identity.is_none());
        }
    }

    fn source_pdf(pages: &[&[&str]]) -> Vec<u8> {
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
        let mut page_ids = Vec::new();
        for lines in pages {
            let mut operations = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 10.into()]),
                Operation::new("Td", vec![50.into(), 740.into()]),
            ];
            for line in *lines {
                operations.push(Operation::new("Tj", vec![Object::string_literal(*line)]));
                operations.push(Operation::new("T*", vec![]));
            }
            operations.push(Operation::new("ET", vec![]));
            let content_id = document.add_object(Stream::new(
                dictionary! {},
                Content { operations }.encode().unwrap(),
            ));
            page_ids.push(document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
            }));
        }
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
                "Count" => page_ids.len() as i64,
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
    fn source_pdf_extracts_bounded_normalized_publisher_text() {
        let pdf = source_pdf(&[
            &["GARMIN GIA 63W", "PART NUMBER 011-00870-00"],
            &["GIA 63W Installation Manual"],
        ]);

        let text = extract_source_pdf_publisher_text(&pdf).unwrap();

        assert!(text.contains("GARMIN GIA 63W"));
        assert!(text.contains("PART NUMBER 011-00870-00"));
        assert!(text.contains("GIA 63W Installation Manual"));
        assert!(!text.contains('\n'));
    }

    #[test]
    fn source_pdf_fails_closed_on_malformed_empty_page_and_resource_limits() {
        assert!(extract_source_pdf_publisher_text(b"%PDF-not-a-document").is_err());

        let empty = source_pdf(&[&[]]);
        assert!(extract_source_pdf_publisher_text(&empty).is_err());

        let two_pages = source_pdf(&[&["Garmin GIA 63"], &["Garmin GIA 63W"]]);
        assert!(extract_source_pdf_publisher_text_with_limits(
            &two_pages,
            SourcePdfLimits {
                max_pages: 1,
                ..SOURCE_PDF_LIMITS
            },
        )
        .is_err());
        assert!(extract_source_pdf_publisher_text_with_limits(
            &two_pages,
            SourcePdfLimits {
                max_total_text_bytes: 4,
                ..SOURCE_PDF_LIMITS
            },
        )
        .is_err());
    }

    #[test]
    fn source_pdf_fails_closed_when_an_encrypt_dictionary_is_present() {
        let pdf = source_pdf(&[&["Garmin GIA 63W"]]);
        let mut document = Document::load_mem(&pdf).unwrap();
        let encrypt_id = document.add_object(dictionary! {
            "Filter" => "Standard",
            "V" => 1,
            "R" => 2,
            "Length" => 40,
            "O" => Object::string_literal("owner"),
            "U" => Object::string_literal("user"),
            "P" => -4,
        });
        document.trailer.set("Encrypt", encrypt_id);
        let mut encrypted = Vec::new();
        document.save_to(&mut encrypted).unwrap();

        assert!(extract_source_pdf_publisher_text(&encrypted).is_err());
    }

    #[test]
    fn source_document_length_and_stream_caps_are_both_enforced() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("9"));
        validate_source_content_length(&headers, Some(9), 10).unwrap();
        assert!(validate_source_content_length(&headers, Some(11), 10).is_err());

        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("11"));
        assert!(validate_source_content_length(&headers, None, 10).is_err());
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("invalid"));
        assert!(validate_source_content_length(&headers, None, 10).is_err());

        let mut body = vec![1, 2, 3];
        append_source_body_chunk(&mut body, &[4, 5], 5).unwrap();
        assert_eq!(body, vec![1, 2, 3, 4, 5]);
        assert!(append_source_body_chunk(&mut body, &[6], 5).is_err());
    }

    #[test]
    fn source_index_content_types_are_strict_and_role_specific() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        validate_source_index_content_type(&headers, SourceIndexDocumentKind::Robots).unwrap();
        assert!(
            validate_source_index_content_type(&headers, SourceIndexDocumentKind::Sitemap).is_err()
        );

        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/xml; charset=utf-8"),
        );
        validate_source_index_content_type(&headers, SourceIndexDocumentKind::Sitemap).unwrap();
        assert!(
            validate_source_index_content_type(&headers, SourceIndexDocumentKind::Robots).is_err()
        );

        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/xml"));
        validate_source_index_content_type(&headers, SourceIndexDocumentKind::Sitemap).unwrap();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
        assert!(
            validate_source_index_content_type(&headers, SourceIndexDocumentKind::Sitemap).is_err()
        );
        headers.remove(CONTENT_TYPE);
        assert!(
            validate_source_index_content_type(&headers, SourceIndexDocumentKind::Sitemap).is_err()
        );
    }

    #[test]
    fn source_index_urls_require_exact_https_origin_without_fragments() {
        let seed = Url::parse("https://media.example.com/releases/182").unwrap();
        for accepted in [
            "https://media.example.com/sitemap.xml",
            "https://media.example.com:443/releases/182",
        ] {
            validate_public_source_index_url(&Url::parse(accepted).unwrap(), &seed).unwrap();
        }
        for rejected in [
            "http://media.example.com/sitemap.xml",
            "https://www.media.example.com/sitemap.xml",
            "https://media.example.com:444/sitemap.xml",
            "https://media.example.com/sitemap.xml#section",
            "https://user:password@media.example.com/sitemap.xml",
            "https://127.0.0.1/sitemap.xml",
        ] {
            assert!(
                validate_public_source_index_url(&Url::parse(rejected).unwrap(), &seed).is_err(),
                "{rejected}"
            );
        }
        let insecure_seed = Url::parse("http://media.example.com/releases/182").unwrap();
        assert!(validate_public_source_index_url(&insecure_seed, &insecure_seed).is_err());
        let private_seed = Url::parse("https://10.0.0.1/releases/182").unwrap();
        assert!(validate_public_source_index_url(&private_seed, &private_seed).is_err());
    }

    #[test]
    fn index_discovered_source_redirects_are_rejected_before_leaving_the_seed_origin() {
        let seed = Url::parse("https://media.example.com/releases/182").unwrap();
        let same_origin =
            source_document_redirect_target(&seed, "/history/skylane#section", Some(&seed))
                .unwrap();
        assert_eq!(
            same_origin.as_str(),
            "https://media.example.com/history/skylane"
        );

        for location in [
            "https://other.example.com/history/skylane",
            "//other.example.com/history/skylane",
            "http://media.example.com/history/skylane",
            "https://media.example.com:444/history/skylane",
            "https://user:password@media.example.com/history/skylane",
        ] {
            assert!(
                source_document_redirect_target(&seed, location, Some(&seed)).is_err(),
                "{location}"
            );
        }

        assert!(
            source_document_redirect_target(
                &seed,
                "https://other.example.com/history/skylane",
                None,
            )
            .is_ok(),
            "ordinary Search citations preserve public cross-origin redirects"
        );
        assert!(validate_public_same_origin_source_document_url(
            &Url::parse("https://media.example.com/history/skylane#section").unwrap(),
            &seed,
        )
        .is_err());
    }

    #[test]
    fn robots_sitemap_discovery_is_bounded_deduplicated_and_same_origin() {
        let seed = Url::parse("https://media.example.com/releases/182").unwrap();
        let robots = r#"
            # publisher index
            sItEmAp: https://media.example.com/sitemap.xml # canonical
            Sitemap: https://media.example.com/sitemap.xml
            Sitemap: http://media.example.com/insecure.xml
            Sitemap: https://other.example.com/foreign.xml
            Sitemap: https://user:password@media.example.com/credential.xml
            Sitemap: https://media.example.com/fragment.xml#unsafe
            Sitemap: https://media.example.com/second.xml
            Sitemap: https://media.example.com/third.xml
            Sitemap: https://media.example.com/fourth.xml
            Sitemap: https://media.example.com/fifth.xml
        "#;
        let urls = parse_robots_sitemap_urls(robots, &seed);
        assert_eq!(urls.len(), MAX_ROBOTS_SITEMAPS);
        assert_eq!(urls[0].as_str(), "https://media.example.com/sitemap.xml");
        assert_eq!(urls[1].as_str(), "https://media.example.com/second.xml");
        assert_eq!(urls[3].as_str(), "https://media.example.com/fourth.xml");
    }

    #[test]
    fn sitemap_parser_extracts_only_structural_locs_and_decodes_xml() {
        let seed = Url::parse("https://media.example.com/releases/182").unwrap();
        let parsed = parse_sitemap(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
              <url>
                <loc>https://media.example.com/releases/182?view=full&amp;lang=en</loc>
                <lastmod>2026-07-24</lastmod>
              </url>
              <url><loc><![CDATA[https://other.example.com/foreign]]></loc></url>
              <metadata><loc>https://media.example.com/not-an-entry</loc></metadata>
            </urlset>"#,
        )
        .unwrap();
        assert_eq!(parsed.kind, SitemapDocumentKind::UrlSet);
        assert_eq!(parsed.locations.len(), 2);
        assert_eq!(
            parse_same_origin_source_index_url(&parsed.locations[0], &seed)
                .unwrap()
                .as_str(),
            "https://media.example.com/releases/182?view=full&lang=en"
        );
        assert!(parse_same_origin_source_index_url(&parsed.locations[1], &seed).is_none());
    }

    #[test]
    fn sitemap_index_parser_is_bounded_and_rejects_active_xml_constructs() {
        let parsed = parse_sitemap(
            r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
                <sitemap><loc>https://media.example.com/one.xml</loc></sitemap>
                <sitemap><loc>https://media.example.com/two.xml</loc></sitemap>
            </sitemapindex>"#,
        )
        .unwrap();
        assert_eq!(parsed.kind, SitemapDocumentKind::SitemapIndex);
        assert_eq!(
            parsed.locations,
            [
                "https://media.example.com/one.xml",
                "https://media.example.com/two.xml"
            ]
        );

        let entity_expansion = r#"<?xml version="1.0"?>
            <!DOCTYPE urlset [
              <!ENTITY a "aaaaaaaaaa">
              <!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
            ]>
            <urlset><url><loc>https://media.example.com/&b;</loc></url></urlset>"#;
        assert!(parse_sitemap(entity_expansion).is_err());
        assert!(parse_sitemap(
            r#"<urlset><?publisher command="fetch"?>
                <url><loc>https://media.example.com/one</loc></url>
            </urlset>"#
        )
        .is_err());
        assert!(parse_sitemap(
            r#"<urlset><url><loc>https://media.example.com/<nested/></loc></url></urlset>"#
        )
        .is_err());
    }

    #[test]
    fn sitemap_location_count_is_capped() {
        let mut sitemap = String::from("<urlset>");
        for index in 0..=MAX_SOURCE_INDEX_URLS {
            sitemap.push_str(&format!(
                "<url><loc>https://media.example.com/{index}</loc></url>"
            ));
        }
        sitemap.push_str("</urlset>");
        let parsed = parse_sitemap(&sitemap).unwrap();
        assert_eq!(parsed.locations.len(), MAX_SOURCE_INDEX_URLS);
    }

    #[test]
    fn sitemap_xml_depth_and_event_count_are_capped() {
        let mut deep = String::from("<urlset>");
        for _ in 0..MAX_SOURCE_INDEX_XML_DEPTH {
            deep.push_str("<metadata>");
        }
        for _ in 0..MAX_SOURCE_INDEX_XML_DEPTH {
            deep.push_str("</metadata>");
        }
        deep.push_str("</urlset>");
        assert!(parse_sitemap(&deep).is_err());

        let mut busy = String::from("<urlset>");
        for _ in 0..=(MAX_SOURCE_INDEX_XML_EVENTS / 2) {
            busy.push_str("<metadata/>");
        }
        busy.push_str("</urlset>");
        assert!(parse_sitemap(&busy).is_err());
    }

    #[test]
    fn preserves_unknown_steps_for_audit_but_curation_fails_closed() {
        let interaction: Interaction = serde_json::from_value(json!({
            "id": "future-step",
            "object": "interaction",
            "status": "completed",
            "steps": [
                {"type": "future_grounding_call", "id": "future-1"},
                {"type": "model_output", "content": [{"type": "text", "text": "answer"}]}
            ]
        }))
        .unwrap();
        assert!(matches!(
            interaction.steps[0],
            InteractionStep::Unknown { .. }
        ));
        assert!(interaction
            .require_curation_output(GroundingRequirement::None)
            .is_err());
    }

    #[test]
    fn client_uses_the_versioned_multimodal_endpoint() {
        let client = GeminiInteractionsClient::new("test-key").unwrap();
        assert_eq!(client.endpoint.as_str(), GEMINI_INTERACTIONS_ENDPOINT);

        let test_endpoint = Url::parse("http://127.0.0.1:1/interactions").unwrap();
        let test_client = GeminiInteractionsClient::with_test_endpoint(
            "test-key",
            test_endpoint.clone(),
            RetryPolicy::default(),
        )
        .unwrap();
        assert_eq!(test_client.endpoint, test_endpoint);
    }
}
