//! Forced Search -> URL Context -> structured JSON grounding.
//!
//! Domain modules own prompts, schemas, catalog decisions, and persistence.
//! This module owns only provider trace validation and citation provenance.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use url::Url;

use crate::gemini::config::{
    GeminiRuntimeConfig, GeminiTask, TaskRoute, ThinkingLevel as ConfigThinkingLevel,
};
use crate::gemini::interactions::{
    CreateInteractionRequest, GeminiInteractionsClient, GenerationConfig, GroundingRequirement,
    InteractionAccountingContext, InteractionResponse, InteractionStep, InteractionTool,
    ResponseFormat, ThinkingLevel, ToolChoice,
};

pub const MAX_URL_CONTEXT_URLS: usize = 20;
const LOGICAL_ATTEMPTS: usize = 2;

#[derive(Clone, Debug)]
pub struct GroundedJsonPassRequest {
    pub prompt: String,
    pub schema: Value,
    pub purpose: String,
    pub schema_version: String,
    pub search_task: GeminiTask,
    pub url_context_task: GeminiTask,
    pub structure_task: GeminiTask,
}

impl GroundedJsonPassRequest {
    pub fn new(
        prompt: impl Into<String>,
        schema: Value,
        purpose: impl Into<String>,
        schema_version: impl Into<String>,
        search_task: GeminiTask,
        url_context_task: GeminiTask,
        structure_task: GeminiTask,
    ) -> Self {
        Self {
            prompt: prompt.into(),
            schema,
            purpose: purpose.into(),
            schema_version: schema_version.into(),
            search_task,
            url_context_task,
            structure_task,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.prompt.trim().is_empty() {
            bail!("grounded curation prompt must not be blank");
        }
        if self.purpose.trim().is_empty() {
            bail!("grounded curation purpose must not be blank");
        }
        if self.schema_version.trim().is_empty() {
            bail!("grounded curation schema version must not be blank");
        }
        if !self.schema.is_object() {
            bail!("grounded curation response schema must be an object");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct GroundingTrace {
    pub google_search_call_count: usize,
    pub url_context_call_count: usize,
    pub citation_urls: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VerifiedCitation {
    pub raw_url: String,
    pub final_url: String,
    pub title: String,
    pub cited_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GroundingSource {
    pub chunk_index: usize,
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GroundingSupport {
    pub text: String,
    pub source_indices: Vec<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InteractionAudit {
    pub purpose: String,
    pub request_json: Value,
    pub interaction_id: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub successful_google_search_calls: usize,
    pub successful_url_context_calls: usize,
    pub function_calls: usize,
    pub citation_urls: Vec<String>,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub raw_response: Value,
}

#[derive(Clone, Debug)]
pub struct GroundedJsonPass {
    pub value: Value,
    pub output: String,
    pub grounding: GroundingTrace,
    pub verified_citations: Vec<VerifiedCitation>,
    pub grounding_sources: Vec<GroundingSource>,
    pub grounding_supports: Vec<GroundingSupport>,
    pub interactions: Vec<InteractionAudit>,
}

/// Run a three-stage, stateless grounding pass.
///
/// `accounting` is called once per provider request so the caller can attach a
/// shared correlation/listing/source scope without coupling this module to a
/// domain-specific case type.
pub async fn run_grounded_json_pass<F>(
    client: &GeminiInteractionsClient,
    config: &GeminiRuntimeConfig,
    request: GroundedJsonPassRequest,
    accounting: F,
) -> Result<GroundedJsonPass>
where
    F: Fn(GeminiTask, String) -> InteractionAccountingContext,
{
    request.validate()?;
    let mut interactions = Vec::new();

    let mut search_result = None;
    let mut search_error = None;
    for attempt in 1..=LOGICAL_ATTEMPTS {
        let search_prompt = build_search_prompt(&request.prompt, attempt, search_error.as_deref());
        let interaction_request = configured_request(
            config,
            request.search_task,
            search_prompt.clone(),
            ToolChoice::Validated,
            accounting(
                request.search_task,
                format!("{}_search_attempt_{attempt}", request.purpose),
            ),
        )
        .with_tool(InteractionTool::GoogleSearch);
        let request_json = json!({
            "model": interaction_request.model,
            "service_tier": interaction_request.service_tier,
            "input": search_prompt,
            "tools": ["google_search"],
            "tool_choice": "validated",
            "attempt": attempt,
            "store": false,
        });
        let response = match client.create(&interaction_request).await {
            Ok(response) => response,
            Err(error) => {
                search_error = Some(format!("Gemini Search discovery request failed: {error}"));
                continue;
            }
        };
        let output_result = response
            .interaction
            .require_curation_output(GroundingRequirement::GoogleSearch)
            .map_err(anyhow::Error::from)
            .and_then(|output| {
                require_exclusive_stage_trace(&response, GroundingStage::Search)?;
                Ok(output)
            });
        let citations_result = resolve_citations(client, &response).await;
        interactions.push(interaction_audit(
            &response,
            &format!("{}_search", request.purpose),
            request_json,
            citations_result.as_deref().unwrap_or_default(),
        ));
        match (output_result, citations_result) {
            (Ok(output), Ok(citations)) if !citations.is_empty() => {
                search_result = Some((output, citations, grounding_trace(&response)));
                break;
            }
            (output, citations) => {
                let message = match (output.err(), citations.err()) {
                    (Some(output), Some(citations)) => {
                        format!("{output}; citation resolution failed: {citations:#}")
                    }
                    (Some(output), None) => output.to_string(),
                    (None, Some(citations)) => {
                        format!("citation resolution failed: {citations:#}")
                    }
                    (None, None) => "Search returned no resolvable citations".to_string(),
                };
                search_error = Some(message);
            }
        }
    }
    let (search_output, search_citations, search_trace) = search_result.ok_or_else(|| {
        anyhow!(
            "Search discovery failed grounding gates after {LOGICAL_ATTEMPTS} attempts: {}",
            search_error.as_deref().unwrap_or("unknown failure")
        )
    })?;
    let candidate_urls = search_citations
        .iter()
        .map(|citation| citation.final_url.clone())
        .collect::<BTreeSet<_>>();
    if candidate_urls.is_empty() || candidate_urls.len() > MAX_URL_CONTEXT_URLS {
        bail!(
            "Search discovery returned {} distinct URLs; URL Context requires 1..={MAX_URL_CONTEXT_URLS}",
            candidate_urls.len()
        );
    }

    let mut url_result = None;
    let mut url_error = None;
    for attempt in 1..=LOGICAL_ATTEMPTS {
        let url_prompt = build_url_context_prompt(
            &request.prompt,
            &search_output,
            &candidate_urls,
            attempt,
            url_error.as_deref(),
        )?;
        let interaction_request = configured_request(
            config,
            request.url_context_task,
            url_prompt.clone(),
            ToolChoice::Validated,
            accounting(
                request.url_context_task,
                format!("{}_url_context_attempt_{attempt}", request.purpose),
            ),
        )
        .with_tool(InteractionTool::UrlContext);
        let request_json = json!({
            "model": interaction_request.model,
            "service_tier": interaction_request.service_tier,
            "input": url_prompt,
            "tools": ["url_context"],
            "tool_choice": "validated",
            "attempt": attempt,
            "store": false,
        });
        let response = match client.create(&interaction_request).await {
            Ok(response) => response,
            Err(error) => {
                url_error = Some(format!(
                    "Gemini URL Context verification request failed: {error}"
                ));
                continue;
            }
        };
        let output_result = response
            .interaction
            .require_curation_output(GroundingRequirement::UrlContext)
            .map_err(anyhow::Error::from)
            .and_then(|output| {
                require_exclusive_stage_trace(&response, GroundingStage::UrlContext)?;
                Ok(output)
            });
        let trace_result = validate_url_context_trace(&response, &candidate_urls);
        let citations_result = resolve_citations(client, &response).await;
        interactions.push(interaction_audit(
            &response,
            &format!("{}_url_context", request.purpose),
            request_json,
            citations_result.as_deref().unwrap_or_default(),
        ));
        match (output_result, trace_result, citations_result) {
            (Ok(output), Ok(successful_urls), Ok(citations)) if !citations.is_empty() => {
                if let Err(error) = require_citations_from_successful_urls(
                    &citations,
                    &candidate_urls,
                    &successful_urls,
                ) {
                    url_error = Some(error.to_string());
                    continue;
                }
                url_result = Some((output, citations, grounding_trace(&response)));
                break;
            }
            (output, trace, citations) => {
                let mut errors = Vec::new();
                if let Err(error) = output {
                    errors.push(error.to_string());
                }
                if let Err(error) = trace {
                    errors.push(error.to_string());
                }
                match citations {
                    Err(error) => errors.push(format!("citation resolution failed: {error:#}")),
                    Ok(citations) if citations.is_empty() => {
                        errors.push("URL Context returned no citations".to_string())
                    }
                    Ok(_) => {}
                }
                url_error = Some(errors.join("; "));
            }
        }
    }
    let (url_output, verified_citations, url_trace) = url_result.ok_or_else(|| {
        anyhow!(
            "URL Context verification failed grounding gates after {LOGICAL_ATTEMPTS} attempts: {}",
            url_error.as_deref().unwrap_or("unknown failure")
        )
    })?;

    let (grounding_sources, grounding_supports) = citation_evidence(&verified_citations);
    let verified_urls = grounding_sources
        .iter()
        .map(|source| source.url.clone())
        .collect::<BTreeSet<_>>();
    let mut structure_result = None;
    let mut structure_error = None;
    for attempt in 1..=LOGICAL_ATTEMPTS {
        let structure_prompt = build_structure_prompt(
            &request.prompt,
            &url_output,
            &verified_citations,
            &verified_urls,
            attempt,
            structure_error.as_deref(),
        )?;
        let interaction_request = configured_request(
            config,
            request.structure_task,
            structure_prompt.clone(),
            ToolChoice::None,
            accounting(
                request.structure_task,
                format!("{}_structure_attempt_{attempt}", request.purpose),
            ),
        )
        .with_response_format(ResponseFormat::json(request.schema.clone())?);
        let request_json = json!({
            "model": interaction_request.model,
            "service_tier": interaction_request.service_tier,
            "input": structure_prompt,
            "tools": [],
            "tool_choice": "none",
            "attempt": attempt,
            "response_schema_version": request.schema_version,
            "store": false,
        });
        let response = match client.create(&interaction_request).await {
            Ok(response) => response,
            Err(error) => {
                structure_error = Some(format!("Gemini structure-only request failed: {error}"));
                continue;
            }
        };
        let parsed = response
            .interaction
            .require_curation_output(GroundingRequirement::None)
            .map_err(anyhow::Error::from)
            .and_then(|output| {
                require_exclusive_stage_trace(&response, GroundingStage::Structure)?;
                let value = serde_json::from_str::<Value>(&output)
                    .context("structure-only output was not valid JSON")?;
                require_verified_source_urls(&value, &verified_urls)?;
                require_verified_evidence_spans(&value, &verified_citations)?;
                Ok((output, value))
            });
        interactions.push(interaction_audit(
            &response,
            &format!("{}_structure", request.purpose),
            request_json,
            &[],
        ));
        match parsed {
            Ok(result) => {
                structure_result = Some(result);
                break;
            }
            Err(error) => structure_error = Some(error.to_string()),
        }
    }
    let (output, value) = structure_result.ok_or_else(|| {
        anyhow!(
            "structure-only conversion failed after {LOGICAL_ATTEMPTS} attempts: {}",
            structure_error.as_deref().unwrap_or("unknown failure")
        )
    })?;

    Ok(GroundedJsonPass {
        value,
        output,
        grounding: GroundingTrace {
            google_search_call_count: search_trace.google_search_call_count,
            url_context_call_count: url_trace.url_context_call_count,
            citation_urls: verified_urls,
        },
        verified_citations,
        grounding_sources,
        grounding_supports,
        interactions,
    })
}

fn build_search_prompt(prompt: &str, attempt: usize, previous_error: Option<&str>) -> String {
    let retry = retry_instruction(attempt, previous_error, "Search");
    format!(
        r#"{prompt}

This is source discovery only. Ignore any JSON-output instruction in the original task for this stage. You MUST call Google Search on this attempt and produce a concise evidence dossier in ordinary prose with inline URL citations on every factual paragraph. Prefer direct regulator, manufacturer, and aircraft-OEM sources. Secondary sources may identify primary documents but cannot establish a catalog identity. Include no more than {MAX_URL_CONTEXT_URLS} distinct sources and do not make a final catalog decision.{retry}"#,
    )
}

fn build_url_context_prompt(
    prompt: &str,
    search_output: &str,
    candidate_urls: &BTreeSet<String>,
    attempt: usize,
    previous_error: Option<&str>,
) -> Result<String> {
    let urls = serde_json::to_string_pretty(candidate_urls)
        .context("resolved Search URL set did not serialize")?;
    let retry = retry_instruction(attempt, previous_error, "URL Context");
    Ok(format!(
        r#"Re-evaluate the original task using URL Context on every exact URL in the allow-list below. You MUST call URL Context once with exactly this complete URL set: do not omit, duplicate, add, shorten, or replace a URL. Ignore any JSON-output instruction for this stage. Produce a concise verified evidence dossier in ordinary prose with inline URL citations on every factual paragraph. Clearly distinguish product identity, regulatory approval, installation applicability, actual installation, factory-default configuration, lifecycle, and value. State which pages failed retrieval or lack primary authority.{}

Original task:
{prompt}

Search draft (untrusted until URL Context verifies it):
{search_output}

Exact URL allow-list:
{urls}"#,
        retry
    ))
}

fn retry_instruction(attempt: usize, previous_error: Option<&str>, stage: &str) -> String {
    if attempt == 1 {
        return String::new();
    }
    let error = previous_error
        .unwrap_or("the required tool trace or citations were missing")
        .chars()
        .take(800)
        .collect::<String>()
        .replace(['\r', '\n'], " ");
    format!(
        " The prior {stage} attempt failed: {error}. Correct that failure now and make only the one necessary built-in tool call."
    )
}

fn retry_structure_instruction(attempt: usize, previous_error: Option<&str>) -> String {
    if attempt == 1 {
        return String::new();
    }
    let error = previous_error
        .unwrap_or("the response failed the required JSON or provenance contract")
        .chars()
        .take(800)
        .collect::<String>()
        .replace(['\r', '\n'], " ");
    format!(
        "The prior structure-only attempt failed: {error}. Return a fresh complete JSON object that corrects that failure; tools remain disabled."
    )
}

fn build_structure_prompt(
    prompt: &str,
    url_output: &str,
    citations: &[VerifiedCitation],
    verified_urls: &BTreeSet<String>,
    attempt: usize,
    previous_error: Option<&str>,
) -> Result<String> {
    let citation_records = serde_json::to_string_pretty(citations)
        .context("verified citation records did not serialize")?;
    let urls = serde_json::to_string_pretty(verified_urls)
        .context("verified URL set did not serialize")?;
    let retry = retry_structure_instruction(attempt, previous_error);
    Ok(format!(
        r#"Convert the verified URL Context dossier below into the requested JSON contract. This is structure-only: tools are disabled, so do not research, infer, repair, or add facts.

Every nonempty field named `source_url` or ending in `_source_url` MUST be copied exactly from the verified URL set. Every evidence/quote field used to authorize an identity or collision decision MUST be copied as an exact normalized substring of `cited_text` from a citation record with that same final URL. Do not expand a short cited span into a broader claim. Preserve contradictions and uncertainty; use the original task's unresolved/reject representation when evidence is insufficient.
{retry}

Original task:
{prompt}

Verified URL Context dossier:
{url_output}

Verified citation records:
{citation_records}

Verified URL set:
{urls}"#
    ))
}

fn configured_request(
    config: &GeminiRuntimeConfig,
    task: GeminiTask,
    input: String,
    tool_choice: ToolChoice,
    accounting: InteractionAccountingContext,
) -> CreateInteractionRequest {
    let route = config.route(task);
    let request = CreateInteractionRequest::new(route.model.clone(), input)
        .with_generation_config(configured_generation(route, tool_choice))
        .with_accounting_context(accounting);
    match route.service_tier.as_deref() {
        Some(service_tier) => request.with_service_tier(service_tier),
        None => request,
    }
}

fn configured_generation(route: &TaskRoute, tool_choice: ToolChoice) -> GenerationConfig {
    GenerationConfig {
        max_output_tokens: Some(route.max_output_tokens),
        thinking_level: match route.thinking_level {
            ConfigThinkingLevel::Disabled => None,
            ConfigThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
            ConfigThinkingLevel::Low => Some(ThinkingLevel::Low),
            ConfigThinkingLevel::Medium => Some(ThinkingLevel::Medium),
            ConfigThinkingLevel::High => Some(ThinkingLevel::High),
        },
        tool_choice: Some(tool_choice),
        ..GenerationConfig::default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroundingStage {
    Search,
    UrlContext,
    Structure,
}

fn require_exclusive_stage_trace(
    response: &InteractionResponse,
    stage: GroundingStage,
) -> Result<()> {
    for step in &response.interaction.steps {
        let allowed = match step {
            InteractionStep::UserInput(_)
            | InteractionStep::ModelOutput(_)
            | InteractionStep::Thought(_) => true,
            InteractionStep::GoogleSearchCall(_) | InteractionStep::GoogleSearchResult(_) => {
                stage == GroundingStage::Search
            }
            InteractionStep::UrlContextCall(_) | InteractionStep::UrlContextResult(_) => {
                stage == GroundingStage::UrlContext
            }
            InteractionStep::FunctionCall(_)
            | InteractionStep::FunctionResult(_)
            | InteractionStep::Unknown { .. } => false,
        };
        if !allowed {
            bail!("{stage:?} stage returned undeclared or cross-stage tool activity");
        }
    }
    if stage == GroundingStage::Structure && !response.interaction.url_citations().is_empty() {
        bail!("structure-only stage returned URL citations despite tools being disabled");
    }
    Ok(())
}

async fn resolve_citations(
    client: &GeminiInteractionsClient,
    response: &InteractionResponse,
) -> Result<Vec<VerifiedCitation>> {
    let mut citation_inputs = Vec::new();
    for citation in response.interaction.url_citations() {
        citation
            .require_complete()
            .context("Gemini returned an incomplete URL citation")?;
        let raw_url = citation
            .citation
            .url
            .clone()
            .expect("complete citation has a URL");
        let cited_text = citation.cited_text()?.trim();
        if cited_text.is_empty() {
            bail!("Gemini returned an empty URL citation span");
        }
        citation_inputs.push((
            raw_url,
            citation.citation.title.clone().unwrap_or_default(),
            cited_text.to_string(),
        ));
    }
    let raw_urls = citation_inputs
        .iter()
        .map(|(url, _, _)| url.clone())
        .collect::<BTreeSet<_>>();
    if raw_urls.len() > MAX_URL_CONTEXT_URLS {
        bail!(
            "Gemini returned {} distinct citation URLs; at most {MAX_URL_CONTEXT_URLS} may be resolved",
            raw_urls.len()
        );
    }
    let mut resolved_urls = BTreeMap::new();
    for raw_url in raw_urls {
        let resolved = client
            .resolve_final_url(&raw_url)
            .await
            .with_context(|| format!("could not resolve Gemini citation {raw_url}"))?;
        resolved_urls.insert(raw_url, resolved.final_url.to_string());
    }
    Ok(citation_inputs
        .into_iter()
        .map(|(raw_url, title, cited_text)| VerifiedCitation {
            final_url: resolved_urls[&raw_url].clone(),
            raw_url,
            title,
            cited_text,
        })
        .collect())
}

fn validate_url_context_trace(
    response: &InteractionResponse,
    expected_urls: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let calls = response
        .interaction
        .steps
        .iter()
        .filter_map(|step| match step {
            InteractionStep::UrlContextCall(call) => Some(call),
            _ => None,
        })
        .collect::<Vec<_>>();
    if calls.len() != 1 {
        bail!(
            "URL Context verification must contain exactly one call; observed {}",
            calls.len()
        );
    }
    let call = calls[0];
    if call.arguments.urls.is_empty() || call.arguments.urls.len() > MAX_URL_CONTEXT_URLS {
        bail!(
            "URL Context call supplied {} URLs; expected 1..={MAX_URL_CONTEXT_URLS}",
            call.arguments.urls.len()
        );
    }
    let supplied = call.arguments.urls.iter().cloned().collect::<BTreeSet<_>>();
    if supplied.len() != call.arguments.urls.len() {
        bail!("URL Context call contained duplicate URLs");
    }
    if &supplied != expected_urls {
        let missing = expected_urls
            .difference(&supplied)
            .cloned()
            .collect::<Vec<_>>();
        let unexpected = supplied
            .difference(expected_urls)
            .cloned()
            .collect::<Vec<_>>();
        bail!(
            "URL Context call did not use the exact Search URL set; missing={missing:?}, unexpected={unexpected:?}"
        );
    }

    let result = response
        .interaction
        .steps
        .iter()
        .filter_map(|step| match step {
            InteractionStep::UrlContextResult(result)
                if result.call_id == call.id && !result.is_error =>
            {
                Some(result)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if result.len() != 1 {
        bail!(
            "URL Context call must have exactly one non-error result; observed {}",
            result.len()
        );
    }
    let result_value = result[0]
        .result
        .as_ref()
        .ok_or_else(|| anyhow!("URL Context result is missing per-URL retrieval metadata"))?;
    // The Interactions reference describes `result` as one UrlContextResult
    // object, while its REST example shows an array of URL records. Accept
    // both documented shapes and apply the same fail-closed URL checks.
    let entries = match result_value {
        Value::Object(_) => vec![result_value],
        Value::Array(entries) => entries.iter().collect::<Vec<_>>(),
        _ => bail!("URL Context retrieval metadata must be an object or array"),
    };
    if entries.is_empty() {
        bail!("URL Context result has no per-URL retrieval metadata");
    }
    let mut seen = HashSet::new();
    let mut successful = BTreeSet::new();
    for entry in entries {
        let url = entry
            .get("url")
            .or_else(|| entry.get("retrieved_url"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| anyhow!("URL Context result entry is missing its URL"))?;
        let key = canonical_url_key(url)?;
        if !seen.insert(key.clone()) {
            bail!("URL Context result repeated retrieval metadata for {url}");
        }
        if !expected_urls
            .iter()
            .any(|expected| canonical_url_key(expected).ok().as_ref() == Some(&key))
        {
            bail!("URL Context result returned an unrequested URL {url}");
        }
        let status = match entry.get("status") {
            None => None,
            Some(Value::String(status)) if !status.trim().is_empty() => {
                Some(status.trim().to_ascii_lowercase())
            }
            Some(_) => bail!("URL Context result entry has a malformed status"),
        };
        let has_retrieved_content = ["title", "snippet", "content", "text"].iter().any(|field| {
            entry
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        });
        if status.is_none() && !has_retrieved_content {
            bail!(
                "URL Context result for {url} omitted both retrieval status and retrieved content"
            );
        }
        if status
            .as_deref()
            .is_none_or(|status| matches!(status, "success" | "url_retrieval_status_success"))
        {
            successful.insert(key);
        }
    }
    let expected_keys = expected_urls
        .iter()
        .map(|url| canonical_url_key(url))
        .collect::<Result<HashSet<_>>>()?;
    if seen != expected_keys {
        let mut missing = expected_keys.difference(&seen).cloned().collect::<Vec<_>>();
        missing.sort();
        bail!("URL Context result omitted requested URLs: {missing:?}");
    }
    if successful.is_empty() {
        bail!("URL Context did not report any successful URL retrieval");
    }
    Ok(successful)
}

fn require_citations_from_successful_urls(
    citations: &[VerifiedCitation],
    expected_urls: &BTreeSet<String>,
    successful_urls: &BTreeSet<String>,
) -> Result<()> {
    for citation in citations {
        let final_key = canonical_url_key(&citation.final_url)?;
        if !expected_urls
            .iter()
            .any(|expected| canonical_url_key(expected).ok().as_ref() == Some(&final_key))
        {
            bail!(
                "URL Context cited URL outside the Search allow-list: {}",
                citation.final_url
            );
        }
        if !successful_urls.contains(&final_key) {
            bail!(
                "URL Context citation is not backed by successful retrieval metadata: {}",
                citation.final_url
            );
        }
    }
    Ok(())
}

fn citation_evidence(
    citations: &[VerifiedCitation],
) -> (Vec<GroundingSource>, Vec<GroundingSupport>) {
    let mut source_by_url = BTreeMap::<String, usize>::new();
    let mut sources = Vec::<GroundingSource>::new();
    let mut supports = Vec::new();
    for citation in citations {
        let index = match source_by_url.get(&citation.final_url) {
            Some(index) => *index,
            None => {
                let index = sources.len();
                source_by_url.insert(citation.final_url.clone(), index);
                sources.push(GroundingSource {
                    chunk_index: index,
                    url: citation.final_url.clone(),
                    title: citation.title.clone(),
                });
                index
            }
        };
        if sources[index].title.is_empty() && !citation.title.is_empty() {
            sources[index].title = citation.title.clone();
        }
        supports.push(GroundingSupport {
            text: citation.cited_text.clone(),
            source_indices: vec![index],
        });
    }
    (sources, supports)
}

fn require_verified_source_urls(value: &Value, verified_urls: &BTreeSet<String>) -> Result<()> {
    fn visit(value: &Value, verified_urls: &BTreeSet<String>, path: &str) -> Result<()> {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    if (key == "source_url" || key.ends_with("_source_url")) && !value.is_null() {
                        let url = value.as_str().ok_or_else(|| {
                            anyhow!("structured output field {child_path} must be a string or null")
                        })?;
                        let url = url.trim();
                        if url.is_empty() {
                            visit(value, verified_urls, &child_path)?;
                            continue;
                        }
                        if !verified_urls.contains(url) {
                            bail!(
                                "structured output field {child_path} uses unverified source URL {url}"
                            );
                        }
                    }
                    visit(value, verified_urls, &child_path)?;
                }
            }
            Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    visit(value, verified_urls, &format!("{path}[{index}]"))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(value, verified_urls, "")
}

fn require_verified_evidence_spans(value: &Value, citations: &[VerifiedCitation]) -> Result<()> {
    fn visit(value: &Value, citations: &[VerifiedCitation], path: &str) -> Result<()> {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    if key == "evidence" || key.ends_with("_evidence") {
                        let evidence = value.as_str().ok_or_else(|| {
                            anyhow!("structured output field {child_path} must be a string")
                        })?;
                        if !evidence.trim().is_empty() {
                            let source_key = if key == "evidence" {
                                "source_url".to_string()
                            } else {
                                format!(
                                    "{}_source_url",
                                    key.strip_suffix("_evidence").expect("suffix checked above")
                                )
                            };
                            let source_url = object
                                .get(&source_key)
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|url| !url.is_empty())
                                .ok_or_else(|| {
                                    anyhow!(
                                        "structured output field {child_path} requires nonblank sibling {source_key}"
                                    )
                                })?;
                            let evidence_key = normalize_evidence_span(evidence);
                            if evidence_key.is_empty()
                                || !citations.iter().any(|citation| {
                                    citation.final_url == source_url
                                        && normalize_evidence_span(&citation.cited_text)
                                            .contains(&evidence_key)
                                })
                            {
                                bail!(
                                    "structured output field {child_path} is not an exact normalized cited span from {source_url}"
                                );
                            }
                        }
                    }
                    visit(value, citations, &child_path)?;
                }
            }
            Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    visit(value, citations, &format!("{path}[{index}]"))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(value, citations, "")
}

fn normalize_evidence_span(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn canonical_url_key(value: &str) -> Result<String> {
    let mut url = Url::parse(value).with_context(|| format!("invalid URL {value:?}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("unsupported URL scheme {}", url.scheme());
    }
    url.set_fragment(None);
    let mut value = url.to_string();
    while value.ends_with('/') && url.path() != "/" {
        value.pop();
    }
    Ok(value)
}

fn grounding_trace(response: &InteractionResponse) -> GroundingTrace {
    let search_calls = response
        .interaction
        .steps
        .iter()
        .filter_map(|step| match step {
            InteractionStep::GoogleSearchCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let url_calls = response
        .interaction
        .steps
        .iter()
        .filter_map(|step| match step {
            InteractionStep::UrlContextCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    GroundingTrace {
        google_search_call_count: response
            .interaction
            .steps
            .iter()
            .filter(|step| {
                matches!(step, InteractionStep::GoogleSearchResult(result) if !result.is_error && search_calls.contains(result.call_id.as_str()))
            })
            .count(),
        url_context_call_count: response
            .interaction
            .steps
            .iter()
            .filter(|step| {
                matches!(step, InteractionStep::UrlContextResult(result) if !result.is_error && url_calls.contains(result.call_id.as_str()))
            })
            .count(),
        citation_urls: response
            .interaction
            .url_citations()
            .into_iter()
            .filter_map(|citation| citation.citation.url.clone())
            .collect(),
    }
}

fn interaction_audit(
    response: &InteractionResponse,
    purpose: &str,
    request_json: Value,
    citations: &[VerifiedCitation],
) -> InteractionAudit {
    let grounding = grounding_trace(response);
    let usage = response.interaction.usage.as_ref();
    InteractionAudit {
        purpose: purpose.to_string(),
        request_json,
        interaction_id: response.interaction.id.clone(),
        model: response.interaction.model.clone(),
        status: response.interaction.status.to_string(),
        successful_google_search_calls: grounding.google_search_call_count,
        successful_url_context_calls: grounding.url_context_call_count,
        function_calls: response.interaction.function_calls().len(),
        citation_urls: citations
            .iter()
            .map(|citation| citation.final_url.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        total_input_tokens: usage.map(|usage| usage.total_input_tokens),
        total_output_tokens: usage.map(|usage| usage.total_output_tokens),
        raw_response: response.raw.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gemini::interactions::Interaction;

    fn response(raw: Value) -> InteractionResponse {
        InteractionResponse {
            interaction: serde_json::from_value::<Interaction>(raw.clone()).unwrap(),
            raw,
            attempts: 1,
        }
    }

    #[test]
    fn url_context_trace_requires_the_exact_allow_list_and_success_metadata() {
        let expected = [
            "https://example.com/manual".to_string(),
            "https://www.faa.gov/tso".to_string(),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let valid = response(json!({
            "id": "url-pass",
            "object": "interaction",
            "status": "completed",
            "steps": [
                {"type": "url_context_call", "id": "url-1", "arguments": {"urls": expected.iter().cloned().collect::<Vec<_>>() }},
                {"type": "url_context_result", "call_id": "url-1", "result": [
                    {"url": "https://example.com/manual", "status": "success"},
                    {"url": "https://www.faa.gov/tso", "status": "unsafe"}
                ]},
                {"type": "model_output", "content": [{"type": "text", "text": "verified"}]}
            ]
        }));
        let successful = validate_url_context_trace(&valid, &expected).unwrap();
        assert_eq!(successful.len(), 1);
        assert!(successful.contains("https://example.com/manual"));

        let injected = response(json!({
            "id": "url-injected",
            "object": "interaction",
            "status": "completed",
            "steps": [
                {"type": "url_context_call", "id": "url-1", "arguments": {"urls": ["https://example.com/manual", "https://attacker.invalid/"]}},
                {"type": "url_context_result", "call_id": "url-1", "result": [{"url": "https://example.com/manual", "status": "success"}]},
                {"type": "model_output", "content": [{"type": "text", "text": "verified"}]}
            ]
        }));
        assert!(validate_url_context_trace(&injected, &expected).is_err());
    }

    #[test]
    fn url_context_trace_accepts_documented_single_object_result() {
        let expected = ["https://example.com/manual".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let response = response(json!({
            "id": "url-object",
            "object": "interaction",
            "status": "completed",
            "steps": [
                {"type": "url_context_call", "id": "url-1", "arguments": {"urls": ["https://example.com/manual"]}},
                {"type": "url_context_result", "call_id": "url-1", "result": {"url": "https://example.com/manual", "status": "success"}},
                {"type": "model_output", "content": [{"type": "text", "text": "verified"}]}
            ]
        }));
        assert_eq!(
            validate_url_context_trace(&response, &expected).unwrap(),
            expected
        );
    }

    #[test]
    fn url_context_trace_accepts_documented_array_without_status() {
        let expected = ["https://example.com/manual".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let response = response(json!({
            "id": "url-array",
            "object": "interaction",
            "status": "completed",
            "steps": [
                {"type": "url_context_call", "id": "url-1", "arguments": {"urls": ["https://example.com/manual"]}},
                {"type": "url_context_result", "call_id": "url-1", "result": [{"url": "https://example.com/manual", "title": "Manual", "snippet": "Product data"}]},
                {"type": "model_output", "content": [{"type": "text", "text": "verified"}]}
            ]
        }));
        assert_eq!(
            validate_url_context_trace(&response, &expected).unwrap(),
            expected
        );
    }

    #[test]
    fn url_context_trace_rejects_present_but_malformed_status() {
        let expected = ["https://example.com/manual".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let response = response(json!({
            "id": "url-bad-status",
            "object": "interaction",
            "status": "completed",
            "steps": [
                {"type": "url_context_call", "id": "url-1", "arguments": {"urls": ["https://example.com/manual"]}},
                {"type": "url_context_result", "call_id": "url-1", "result": {"url": "https://example.com/manual", "status": null}},
                {"type": "model_output", "content": [{"type": "text", "text": "verified"}]}
            ]
        }));
        assert!(validate_url_context_trace(&response, &expected).is_err());
    }

    #[test]
    fn url_context_trace_enforces_the_twenty_url_boundary() {
        fn trace_for(urls: &[String]) -> InteractionResponse {
            response(json!({
                "id": "url-boundary",
                "object": "interaction",
                "status": "completed",
                "steps": [
                    {"type": "url_context_call", "id": "url-1", "arguments": {"urls": urls}},
                    {"type": "url_context_result", "call_id": "url-1", "result": urls.iter().map(|url| json!({"url": url, "status": "success"})).collect::<Vec<_>>()},
                    {"type": "model_output", "content": [{"type": "text", "text": "verified"}]}
                ]
            }))
        }

        let twenty = (0..MAX_URL_CONTEXT_URLS)
            .map(|index| format!("https://example.com/manual-{index}"))
            .collect::<Vec<_>>();
        let expected = twenty.iter().cloned().collect::<BTreeSet<_>>();
        assert_eq!(
            validate_url_context_trace(&trace_for(&twenty), &expected).unwrap(),
            expected
        );

        let twenty_one = (0..=MAX_URL_CONTEXT_URLS)
            .map(|index| format!("https://example.com/manual-{index}"))
            .collect::<Vec<_>>();
        let expected = twenty_one.iter().cloned().collect::<BTreeSet<_>>();
        assert!(validate_url_context_trace(&trace_for(&twenty_one), &expected).is_err());
    }

    #[test]
    fn citations_from_unsuccessful_url_retrievals_are_rejected() {
        let expected = [
            "https://example.com/retrieved".to_string(),
            "https://example.com/paywalled".to_string(),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let successful = ["https://example.com/retrieved".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let citations = vec![VerifiedCitation {
            raw_url: "https://example.com/paywalled".to_string(),
            final_url: "https://example.com/paywalled".to_string(),
            title: "Unavailable manual".to_string(),
            cited_text: "Unverified text".to_string(),
        }];

        assert!(
            require_citations_from_successful_urls(&citations, &expected, &successful).is_err()
        );
    }

    #[test]
    fn structure_retry_keeps_tools_disabled_and_reports_the_prior_failure() {
        let urls = ["https://example.com/manual".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let prompt = build_structure_prompt(
            "Resolve one avionics identity.",
            "Verified dossier.",
            &[],
            &urls,
            2,
            Some("HTTP 400 invalid argument"),
        )
        .unwrap();

        assert!(prompt.contains("prior structure-only attempt failed"));
        assert!(prompt.contains("HTTP 400 invalid argument"));
        assert!(prompt.contains("tools remain disabled"));
    }

    #[test]
    fn structure_stage_rejects_cross_stage_tool_activity() {
        let response = response(json!({
            "id": "structure-with-search",
            "object": "interaction",
            "status": "completed",
            "steps": [
                {"type": "google_search_call", "id": "search-1", "arguments": {"query": "Garmin"}},
                {"type": "google_search_result", "call_id": "search-1", "result": {}},
                {"type": "model_output", "content": [{"type": "text", "text": "{}"}]}
            ]
        }));
        assert!(require_exclusive_stage_trace(&response, GroundingStage::Structure).is_err());
        assert!(require_exclusive_stage_trace(&response, GroundingStage::Search).is_ok());
    }

    #[test]
    fn citation_adapter_keeps_one_source_and_multiple_exact_spans() {
        let citations = vec![
            VerifiedCitation {
                raw_url: "https://search.example/one".to_string(),
                final_url: "https://example.com/manual".to_string(),
                title: "Installation manual".to_string(),
                cited_text: "Model GTX 345R".to_string(),
            },
            VerifiedCitation {
                raw_url: "https://search.example/two".to_string(),
                final_url: "https://example.com/manual".to_string(),
                title: String::new(),
                cited_text: "Part number 011-03378-40".to_string(),
            },
        ];
        let (sources, supports) = citation_evidence(&citations);
        assert_eq!(sources.len(), 1);
        assert_eq!(supports.len(), 2);
        assert_eq!(supports[0].source_indices, vec![0]);
        assert_eq!(supports[1].source_indices, vec![0]);
    }

    #[test]
    fn structured_source_urls_must_be_verified() {
        let verified = ["https://example.com/manual".to_string()]
            .into_iter()
            .collect();
        assert!(require_verified_source_urls(
            &json!({"identity_source_url": "https://example.com/manual"}),
            &verified
        )
        .is_ok());
        assert!(require_verified_source_urls(
            &json!({"reviews": [{"source_url": "https://unverified.invalid/"}]}),
            &verified
        )
        .is_err());
        assert!(require_verified_source_urls(
            &json!({"identity_source_url": {"url": "https://example.com/manual"}}),
            &verified
        )
        .is_err());
    }

    #[test]
    fn structured_evidence_must_be_a_cited_span_from_its_sibling_source() {
        let citations = vec![VerifiedCitation {
            raw_url: "https://example.com/manual".to_string(),
            final_url: "https://example.com/manual".to_string(),
            title: "Installation manual".to_string(),
            cited_text: "The GTX 345R is identified by part number 011-03378-40.".to_string(),
        }];
        assert!(require_verified_evidence_spans(
            &json!({
                "identity_source_url": "https://example.com/manual",
                "identity_evidence": "GTX 345R is identified by part number 011-03378-40"
            }),
            &citations,
        )
        .is_ok());
        assert!(require_verified_evidence_spans(
            &json!({
                "identity_source_url": "https://example.com/manual",
                "identity_evidence": "The GTX 345R also provides unmentioned weather radar"
            }),
            &citations,
        )
        .is_err());
        assert!(require_verified_evidence_spans(
            &json!({
                "identity_source_url": "https://different.example/manual",
                "identity_evidence": "GTX 345R is identified by part number 011-03378-40"
            }),
            &citations,
        )
        .is_err());
    }
}
