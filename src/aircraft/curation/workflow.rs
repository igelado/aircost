//! Read-only Gemini hierarchy-curation workflow.
//!
//! Persistence is intentionally separate. Running this workflow cannot create
//! or approve canonical aircraft identities.

use std::collections::BTreeSet;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::aircraft::curation::{
    build_hierarchy_adjudication_prompt, build_hierarchy_verification_prompt,
    build_identity_evidence_prompt, build_reviewable_aircraft_hierarchy,
    hierarchy_adjudication_response_schema, hierarchy_verification_response_schema,
    identity_evidence_response_schema, search_approved_aircraft_catalog,
    validate_aircraft_hierarchy_adjudication, AircraftCatalogSearchRequest,
    AircraftCatalogSearchResponse, AircraftHierarchyAdjudication, AircraftHierarchyVerification,
    AircraftIdentityEvidenceResearch, CatalogCandidateRegistry, GroundingAudit,
    ReviewableAircraftHierarchy,
};
use crate::aircraft::faa::{
    lookup_current, require_eligible, require_listing_admission, AircraftGrounding, Eligibility,
    LookupOutcome, Snapshot,
};
use crate::aircraft::observations::{
    group_observations_by_cluster, load_aircraft_identity_observations, AircraftIdentityObservation,
};
use crate::db::AppDb;
use crate::gemini::config::{
    GeminiRuntimeConfig, GeminiTask, TaskRoute, ThinkingLevel as ConfigThinkingLevel,
};
use crate::gemini::interactions::{
    CreateInteractionRequest, FunctionCallStep, GeminiInteractionsClient, GenerationConfig,
    GroundingRequirement, InteractionAccountingContext, InteractionInput, InteractionResponse,
    InteractionStatus, InteractionStep, InteractionTool, ResponseFormat, StatelessHistory,
    ThinkingLevel, ToolChoice,
};

const CATALOG_ADJUDICATION_STAGES: usize = 2;
const MAX_URL_CONTEXT_URLS: usize = 20;
const FAA_LOOKUP_FUNCTION_NAME: &str = "lookup_faa_aircraft_registry";
const CATALOG_SEARCH_FUNCTION_NAME: &str = "search_aircraft_catalog";

#[derive(Clone, Debug, Serialize)]
pub struct FaaObservationAudit {
    pub listing_id: i64,
    pub observation_sha256: String,
    pub supplied_registration: Option<String>,
    pub supplied_serial_number: Option<String>,
    /// The listing's model year is retained unchanged. It is never inferred
    /// from or replaced by FAA `YEAR MFR`.
    pub listing_model_year: i64,
    pub faa_year_manufactured: Option<u16>,
    pub model_year_differs_from_year_manufactured: bool,
    pub faa_eligible: bool,
    pub included_in_curation: bool,
    pub lookup_outcome: Option<LookupOutcome>,
    pub eligibility: Option<Eligibility>,
    pub lookup_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FaaRegistryObservationGrounding {
    pub listing_id: i64,
    pub observation_sha256: String,
    pub listing_model_year: i64,
    pub model_year_differs_from_year_manufactured: bool,
    pub grounding: AircraftGrounding,
}

/// The immutable payload returned to Gemini by the local FAA function.
///
/// Registrations are deliberately absent from the function arguments. Gemini
/// can retrieve only the precomputed rows bound to this case token.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FaaRegistryFunctionResult {
    pub case_token: String,
    pub cluster_key: String,
    pub snapshot: Snapshot,
    pub year_manufactured_is_model_year: bool,
    pub observations: Vec<FaaRegistryObservationGrounding>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FaaRegistryFunctionRequest {
    case_token: String,
    cluster_key: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CurationInteractionAudit {
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

#[derive(Clone, Debug, Serialize)]
pub struct AircraftHierarchyCurationCaseReport {
    pub cluster_key: String,
    pub listing_ids: Vec<i64>,
    pub curation_listing_ids: Vec<i64>,
    pub observation_sha256s: Vec<String>,
    pub source_observation_count: usize,
    pub skipped_non_exact_observation_count: usize,
    pub faa_eligible_observation_count: usize,
    pub faa_rejected_observation_count: usize,
    pub faa_snapshot: Option<Snapshot>,
    pub faa_observations: Vec<FaaObservationAudit>,
    pub faa_function_call_count: usize,
    pub faa_function_result_count: usize,
    pub faa_function_results: Vec<FaaRegistryFunctionResult>,
    pub catalog_revision: Option<String>,
    pub research: Option<AircraftIdentityEvidenceResearch>,
    pub adjudication: Option<AircraftHierarchyAdjudication>,
    pub verification: Option<AircraftHierarchyVerification>,
    pub reviewable: Option<ReviewableAircraftHierarchy>,
    pub validation_errors: Vec<String>,
    pub interactions: Vec<CurationInteractionAudit>,
    pub catalog_function_results: Vec<AircraftCatalogSearchResponse>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftHierarchyCurationReport {
    pub listing_observations_loaded: usize,
    pub retained_html_observations: usize,
    pub fallback_observations: usize,
    pub unique_clusters: usize,
    pub attempted_clusters: usize,
    pub reviewable_clusters: usize,
    pub blocked_clusters: usize,
    pub faa_eligible_observations: usize,
    pub faa_rejected_observations: usize,
    pub cases: Vec<AircraftHierarchyCurationCaseReport>,
    pub canonical_catalog_writes: usize,
}

pub async fn curate_aircraft_hierarchy_observations(
    db: &AppDb,
    client: &GeminiInteractionsClient,
    listing_limit: i64,
    listing_id: Option<i64>,
    cluster_limit: usize,
) -> Result<AircraftHierarchyCurationReport> {
    let config = GeminiRuntimeConfig::from_environment()
        .context("could not load runtime Gemini task routing")?;
    curate_aircraft_hierarchy_observations_with_config(
        db,
        client,
        listing_limit,
        listing_id,
        cluster_limit,
        &config,
    )
    .await
}

pub async fn curate_aircraft_hierarchy_observations_with_config(
    db: &AppDb,
    client: &GeminiInteractionsClient,
    listing_limit: i64,
    listing_id: Option<i64>,
    cluster_limit: usize,
    config: &GeminiRuntimeConfig,
) -> Result<AircraftHierarchyCurationReport> {
    if cluster_limit == 0 {
        return Err(anyhow!("cluster_limit must be at least 1"));
    }
    config
        .validate()
        .context("invalid runtime Gemini routing")?;
    let loaded = load_aircraft_identity_observations(db, listing_limit, listing_id)
        .await
        .map_err(|error| anyhow!(error))?;
    let grouped = group_observations_by_cluster(&loaded.observations);
    let mut cases = Vec::new();
    for (cluster_key, observations) in grouped.into_iter().take(cluster_limit) {
        cases.push(curate_cluster(db, client, cluster_key, &observations, config).await);
    }
    let reviewable_clusters = cases
        .iter()
        .filter(|case| case.reviewable.is_some())
        .count();
    let blocked_clusters = cases.len().saturating_sub(reviewable_clusters);
    let faa_eligible_observations = cases
        .iter()
        .map(|case| case.faa_eligible_observation_count)
        .sum();
    let faa_rejected_observations = cases
        .iter()
        .map(|case| case.faa_rejected_observation_count)
        .sum();
    Ok(AircraftHierarchyCurationReport {
        listing_observations_loaded: loaded.observations.len(),
        retained_html_observations: loaded.retained_html_count,
        fallback_observations: loaded.fallback_count,
        unique_clusters: loaded.unique_clusters,
        attempted_clusters: cases.len(),
        reviewable_clusters,
        blocked_clusters,
        faa_eligible_observations,
        faa_rejected_observations,
        cases,
        canonical_catalog_writes: 0,
    })
}

async fn curate_cluster(
    db: &AppDb,
    client: &GeminiInteractionsClient,
    cluster_key: &str,
    observations: &[&AircraftIdentityObservation],
    config: &GeminiRuntimeConfig,
) -> AircraftHierarchyCurationCaseReport {
    let exact = observations
        .iter()
        .copied()
        .filter(|observation| observation.source_excerpt_is_exact)
        .collect::<Vec<_>>();
    let mut report = AircraftHierarchyCurationCaseReport {
        cluster_key: cluster_key.to_string(),
        listing_ids: observations
            .iter()
            .map(|observation| observation.listing_id)
            .collect(),
        curation_listing_ids: Vec::new(),
        observation_sha256s: observations
            .iter()
            .map(|observation| observation.observation_sha256.clone())
            .collect(),
        source_observation_count: exact.len(),
        skipped_non_exact_observation_count: observations.len().saturating_sub(exact.len()),
        faa_eligible_observation_count: 0,
        faa_rejected_observation_count: 0,
        faa_snapshot: None,
        faa_observations: Vec::new(),
        faa_function_call_count: 0,
        faa_function_result_count: 0,
        faa_function_results: Vec::new(),
        catalog_revision: None,
        research: None,
        adjudication: None,
        verification: None,
        reviewable: None,
        validation_errors: Vec::new(),
        interactions: Vec::new(),
        catalog_function_results: Vec::new(),
    };
    if exact.is_empty() {
        report.validation_errors.push(
            "no observation in this cluster had literal hierarchy labels present in retained source text"
                .to_string(),
        );
    }

    // Apply the registration policy to every listing observation, even when a
    // separate retained-source gate will also exclude it. This keeps foreign
    // and missing registrations explicitly visible as FAA-policy rejections.
    let all_faa_grounded =
        match prepare_faa_grounded_case(db, cluster_key, observations, &mut report).await {
            Ok(Some(faa_case)) => faa_case,
            Ok(None) => return report,
            Err(error) => {
                report.validation_errors.push(format!(
                    "mandatory FAA grounding could not be prepared: {error:#}"
                ));
                return report;
            }
        };
    if exact.is_empty() {
        return report;
    }
    let eligible = exact
        .iter()
        .copied()
        .filter(|observation| {
            all_faa_grounded.observations.iter().any(|grounded| {
                grounded.listing_id == observation.listing_id
                    && grounded.observation_sha256 == observation.observation_sha256
            })
        })
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        report.validation_errors.push(
            "faa_grounding_required: no source-exact observation passed the mandatory FAA gate; Gemini was not called"
                .to_string(),
        );
        return report;
    }
    report.curation_listing_ids = eligible
        .iter()
        .map(|observation| observation.listing_id)
        .collect();
    for audit in &mut report.faa_observations {
        audit.included_in_curation = eligible.iter().any(|observation| {
            observation.listing_id == audit.listing_id
                && observation.observation_sha256 == audit.observation_sha256
        });
    }
    let selected_groundings = all_faa_grounded
        .observations
        .into_iter()
        .filter(|grounded| {
            eligible.iter().any(|observation| {
                observation.listing_id == grounded.listing_id
                    && observation.observation_sha256 == grounded.observation_sha256
            })
        })
        .collect::<Vec<_>>();
    let mut selected_snapshot = None;
    for grounded in &selected_groundings {
        if let Err(error) =
            merge_reported_snapshot(&mut selected_snapshot, &grounded.grounding.snapshot)
        {
            report.validation_errors.push(format!(
                "mandatory FAA grounding did not use one release: {error:#}"
            ));
            return report;
        }
    }
    let selected_snapshot = selected_snapshot.expect("eligible observations carry FAA snapshots");
    let faa_case = FaaRegistryFunctionResult {
        case_token: faa_case_token(cluster_key, &selected_snapshot, &selected_groundings),
        cluster_key: cluster_key.to_string(),
        snapshot: selected_snapshot,
        year_manufactured_is_model_year: false,
        observations: selected_groundings,
    };

    let result = curate_exact_cluster(db, client, &eligible, &faa_case, &mut report, config).await;
    if let Err(error) = result {
        report.validation_errors.push(format!("{error:#}"));
    }
    report
}

async fn prepare_faa_grounded_case(
    db: &AppDb,
    cluster_key: &str,
    observations: &[&AircraftIdentityObservation],
    report: &mut AircraftHierarchyCurationCaseReport,
) -> Result<Option<FaaRegistryFunctionResult>> {
    let mut grounded_observations = Vec::new();
    let mut eligible_snapshot: Option<Snapshot> = None;

    for observation in observations {
        let lookup = lookup_current(
            db,
            observation.registration_number.as_deref(),
            observation.serial_number.as_deref(),
        )
        .await;
        let (outcome, eligibility, lookup_error) = match lookup {
            Ok(outcome) => {
                if let Some(snapshot) = snapshot_from_outcome(&outcome) {
                    merge_reported_snapshot(&mut report.faa_snapshot, snapshot)?;
                }
                let eligibility = require_eligible(outcome.clone());
                (Some(outcome), Some(eligibility), None)
            }
            Err(error) => (None, None, Some(format!("{error:#}"))),
        };

        let faa_eligible = match eligibility.as_ref() {
            Some(Eligibility::Eligible { grounding }) => {
                merge_reported_snapshot(&mut eligible_snapshot, &grounding.snapshot)?;
                let model_year_differs_from_year_manufactured = grounding
                    .year_manufactured
                    .is_some_and(|year| i64::from(year) != observation.model_year);
                grounded_observations.push(FaaRegistryObservationGrounding {
                    listing_id: observation.listing_id,
                    observation_sha256: observation.observation_sha256.clone(),
                    listing_model_year: observation.model_year,
                    model_year_differs_from_year_manufactured,
                    grounding: grounding.clone(),
                });
                report.faa_eligible_observation_count += 1;
                true
            }
            Some(Eligibility::Blocked { reason, .. }) => {
                report.faa_rejected_observation_count += 1;
                report.validation_errors.push(format!(
                    "faa_grounding_rejected: listing {} was excluded from curation: {reason:?}",
                    observation.listing_id
                ));
                false
            }
            None => {
                report.faa_rejected_observation_count += 1;
                report.validation_errors.push(format!(
                    "faa_grounding_lookup_failed: listing {} was excluded from curation: {}",
                    observation.listing_id,
                    lookup_error.as_deref().unwrap_or("unknown lookup error")
                ));
                false
            }
        };
        let faa_year_manufactured = outcome.as_ref().and_then(|outcome| match outcome {
            LookupOutcome::Found { grounding } => grounding.year_manufactured,
            _ => None,
        });
        report.faa_observations.push(FaaObservationAudit {
            listing_id: observation.listing_id,
            observation_sha256: observation.observation_sha256.clone(),
            supplied_registration: observation.registration_number.clone(),
            supplied_serial_number: observation.serial_number.clone(),
            listing_model_year: observation.model_year,
            faa_year_manufactured,
            model_year_differs_from_year_manufactured: faa_year_manufactured
                .is_some_and(|year| i64::from(year) != observation.model_year),
            faa_eligible,
            included_in_curation: false,
            lookup_outcome: outcome,
            eligibility,
            lookup_error,
        });
    }

    if grounded_observations.is_empty() {
        report.validation_errors.push(
            "faa_grounding_required: no observation had an eligible current FAA N-number lookup; Gemini was not called"
                .to_string(),
        );
        return Ok(None);
    }
    let snapshot = eligible_snapshot.expect("eligible FAA observations carry a snapshot");
    grounded_observations.sort_by_key(|observation| observation.listing_id);
    let case_token = faa_case_token(cluster_key, &snapshot, &grounded_observations);
    Ok(Some(FaaRegistryFunctionResult {
        case_token,
        cluster_key: cluster_key.to_string(),
        snapshot,
        year_manufactured_is_model_year: false,
        observations: grounded_observations,
    }))
}

fn snapshot_from_outcome(outcome: &LookupOutcome) -> Option<&Snapshot> {
    match outcome {
        LookupOutcome::NotFound { snapshot, .. }
        | LookupOutcome::NotCovered { snapshot, .. }
        | LookupOutcome::Ambiguous { snapshot, .. } => Some(snapshot),
        LookupOutcome::Found { grounding } => Some(&grounding.snapshot),
        LookupOutcome::NotApplicable { .. } | LookupOutcome::NoSnapshot => None,
    }
}

fn merge_reported_snapshot(target: &mut Option<Snapshot>, candidate: &Snapshot) -> Result<()> {
    if let Some(current) = target {
        let same_release = current.snapshot_date == candidate.snapshot_date
            && current.source_url == candidate.source_url
            && current.archive_sha256 == candidate.archive_sha256
            && current.source_manifest_sha256 == candidate.source_manifest_sha256;
        if !same_release {
            return Err(anyhow!(
                "FAA release changed while preparing one curation case (snapshot {} -> {})",
                current.id,
                candidate.id
            ));
        }
        // The same daily archive may have several immutable target-scoped
        // projections. Keep the newest projection as case-level provenance;
        // each observation still retains the exact projection that covered it.
        if candidate.id > current.id {
            *current = candidate.clone();
        }
    } else {
        *target = Some(candidate.clone());
    }
    Ok(())
}

fn faa_case_token(
    cluster_key: &str,
    snapshot: &Snapshot,
    observations: &[FaaRegistryObservationGrounding],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aircost-faa-curation-case-v1\0");
    hasher.update(cluster_key.as_bytes());
    hasher.update(b"\0");
    hasher.update(snapshot.source_manifest_sha256.as_bytes());
    for observation in observations {
        hasher.update(b"\0");
        hasher.update(observation.listing_id.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(observation.observation_sha256.as_bytes());
        hasher.update(b"\0");
        hasher.update(observation.grounding.n_number.as_bytes());
    }
    format!("faa_case_{:x}", hasher.finalize())
}

#[derive(Clone, Debug)]
struct CurationAccountingScope {
    correlation_id: String,
    listing_id: Option<i64>,
    source_id: String,
}

impl CurationAccountingScope {
    fn new(
        observations: &[&AircraftIdentityObservation],
        faa_case: &FaaRegistryFunctionResult,
    ) -> Self {
        Self {
            correlation_id: faa_case.case_token.clone(),
            listing_id: (observations.len() == 1).then(|| observations[0].listing_id),
            source_id: faa_case.case_token.clone(),
        }
    }

    fn request_context(
        &self,
        task: GeminiTask,
        purpose: impl Into<String>,
    ) -> InteractionAccountingContext {
        let context = InteractionAccountingContext::new(task, purpose)
            .with_correlation_id(self.correlation_id.clone())
            .with_source("aircraft_hierarchy_case", self.source_id.clone());
        match self.listing_id {
            Some(listing_id) => context.with_listing_id(listing_id),
            None => context,
        }
    }
}

async fn curate_exact_cluster(
    db: &AppDb,
    client: &GeminiInteractionsClient,
    observations: &[&AircraftIdentityObservation],
    faa_case: &FaaRegistryFunctionResult,
    report: &mut AircraftHierarchyCurationCaseReport,
    config: &GeminiRuntimeConfig,
) -> Result<()> {
    let accounting = CurationAccountingScope::new(observations, faa_case);
    let evidence_prompt = append_faa_grounding_context(
        build_identity_evidence_prompt(observations),
        faa_case,
        "evidence discovery",
    )?;
    let evidence_pass = run_grounded_json_pass(
        client,
        evidence_prompt,
        identity_evidence_response_schema(),
        "identity_evidence",
        GeminiTask::AircraftStructure,
        config,
        &accounting,
    )
    .await
    .context("Gemini identity evidence request failed")?;
    report.interactions.extend(evidence_pass.interactions);
    let evidence_grounding = evidence_pass.grounding;
    let evidence_output = evidence_pass.output;
    let research = serde_json::from_str::<AircraftIdentityEvidenceResearch>(&evidence_output)
        .context("Gemini identity evidence output did not match the response contract")?;
    report.research = Some(research.clone());

    let (adjudication, catalog_results, faa_call_count, faa_results, adjudication_audits) =
        run_catalog_adjudication(
            db,
            client,
            append_faa_grounding_context(
                build_hierarchy_adjudication_prompt(observations, &research),
                faa_case,
                "hierarchy adjudication",
            )?,
            faa_case,
            config,
            &accounting,
        )
        .await?;
    let mut candidate_registry = CatalogCandidateRegistry::default();
    let mut catalog_revision = None;
    for result in &catalog_results {
        if let Some(previous) = &catalog_revision {
            if previous != &result.catalog_revision {
                return Err(anyhow!(
                    "approved aircraft catalog changed during one adjudication"
                ));
            }
        }
        catalog_revision = Some(result.catalog_revision.clone());
        for (kind, ids) in result.candidate_registry().ids_by_kind {
            for id in ids {
                candidate_registry.insert(kind, id);
            }
        }
    }
    report.catalog_revision = catalog_revision;
    report.catalog_function_results = catalog_results;
    report.faa_function_call_count = faa_call_count;
    report.faa_function_result_count = faa_results.len();
    report.faa_function_results = faa_results;
    report.interactions.extend(adjudication_audits);
    report.adjudication = Some(adjudication.clone());

    let faa_trace_satisfied =
        report.faa_function_call_count == 1 && report.faa_function_result_count == 1;
    let catalog_trace_satisfied = report.catalog_function_results.len() == 1;
    let every_included_observation_is_faa_eligible = observations.iter().all(|observation| {
        report.faa_observations.iter().any(|audit| {
            audit.listing_id == observation.listing_id
                && audit.observation_sha256 == observation.observation_sha256
                && audit.faa_eligible
                && audit.included_in_curation
                && matches!(
                    audit.eligibility.as_ref(),
                    Some(Eligibility::Eligible { .. })
                )
        })
    });
    if !faa_trace_satisfied {
        report.validation_errors.push(
            "faa_function_trace_required: adjudication must contain exactly one successful lookup_faa_aircraft_registry call/result pair"
                .to_string(),
        );
    }
    if !catalog_trace_satisfied {
        report.validation_errors.push(
            "catalog_function_trace_required: adjudication must contain exactly one successful search_aircraft_catalog call/result pair"
                .to_string(),
        );
    }
    if !every_included_observation_is_faa_eligible {
        report.validation_errors.push(
            "faa_ineligible_observation_included: every observation supplied to Gemini must pass the current FAA gate"
                .to_string(),
        );
    }
    if !faa_trace_satisfied
        || !catalog_trace_satisfied
        || !every_included_observation_is_faa_eligible
    {
        return Ok(());
    }
    revalidate_faa_case(db, observations, faa_case)
        .await
        .context("FAA case changed after Gemini adjudication")?;
    if let Err(errors) = validate_aircraft_hierarchy_adjudication(
        &research,
        &evidence_grounding,
        &adjudication,
        &candidate_registry,
        report.catalog_function_results.len(),
    ) {
        report.validation_errors.extend(
            errors
                .0
                .into_iter()
                .map(|issue| format!("{}: {}", issue.code, issue.message)),
        );
        return Ok(());
    }

    let verifier_prompt = append_faa_grounding_context(
        build_hierarchy_verification_prompt(observations, &research, &adjudication),
        faa_case,
        "independent verification",
    )?;
    let verifier_pass = run_grounded_json_pass(
        client,
        verifier_prompt,
        hierarchy_verification_response_schema(),
        "identity_verification",
        GeminiTask::AircraftHierarchyVerification,
        config,
        &accounting,
    )
    .await
    .context("Gemini hierarchy verification request failed")?;
    report.interactions.extend(verifier_pass.interactions);
    let verifier_grounding = verifier_pass.grounding;
    let verifier_output = verifier_pass.output;
    let verification = serde_json::from_str::<AircraftHierarchyVerification>(&verifier_output)
        .context("Gemini hierarchy verification output did not match the response contract")?;
    report.verification = Some(verification.clone());

    revalidate_faa_case(db, observations, faa_case)
        .await
        .context("FAA case changed after Gemini verification")?;
    match build_reviewable_aircraft_hierarchy(
        &research,
        &evidence_grounding,
        adjudication,
        &candidate_registry,
        report.catalog_function_results.len(),
        verification,
        &verifier_grounding,
    ) {
        Ok(reviewable) => report.reviewable = Some(reviewable),
        Err(errors) => {
            report.validation_errors.extend(
                errors
                    .0
                    .into_iter()
                    .map(|issue| format!("{}: {}", issue.code, issue.message)),
            );
        }
    }
    Ok(())
}

async fn revalidate_faa_case(
    db: &AppDb,
    observations: &[&AircraftIdentityObservation],
    expected: &FaaRegistryFunctionResult,
) -> Result<()> {
    let mut current_snapshot = None;
    let mut current_observations = Vec::with_capacity(observations.len());
    for observation in observations {
        let grounding = require_listing_admission(db, observation.listing_id)
            .await
            .map_err(|error| anyhow!(error))?;
        merge_reported_snapshot(&mut current_snapshot, &grounding.snapshot)?;
        current_observations.push(FaaRegistryObservationGrounding {
            listing_id: observation.listing_id,
            observation_sha256: observation.observation_sha256.clone(),
            listing_model_year: observation.model_year,
            model_year_differs_from_year_manufactured: grounding
                .year_manufactured
                .is_some_and(|year| i64::from(year) != observation.model_year),
            grounding,
        });
    }
    current_observations.sort_by_key(|observation| observation.listing_id);
    let snapshot = current_snapshot.ok_or_else(|| anyhow!("FAA case has no observations"))?;
    let current = FaaRegistryFunctionResult {
        case_token: faa_case_token(&expected.cluster_key, &snapshot, &current_observations),
        cluster_key: expected.cluster_key.clone(),
        snapshot,
        year_manufactured_is_model_year: false,
        observations: current_observations,
    };
    if &current != expected {
        return Err(anyhow!(
            "the listing identity or newest FAA projection changed during curation"
        ));
    }
    Ok(())
}

fn append_faa_grounding_context(
    prompt: String,
    faa_case: &FaaRegistryFunctionResult,
    phase: &str,
) -> Result<String> {
    let grounding = serde_json::to_string_pretty(faa_case)
        .context("FAA grounding did not serialize for the Gemini prompt")?;
    Ok(format!(
        r#"{prompt}

Mandatory deterministic FAA grounding for {phase}:
The JSON below came from a locally imported, digest-identified snapshot of the FAA releasable registry. Treat it as controlling over listing text and model memory only for facts the FAA publishes for the registered aircraft: N-number, manufacturer serial, FAA aircraft code, FAA make/model/series reference fields, FAA engine code and its joined engine make/model reference, year manufactured, and type-certificate reference fields when present. If listing text conflicts with one of those facts, preserve and report the conflict.

FAA `year_manufactured` is audit-only and MUST NOT replace, infer, increment, decrement, or otherwise alter listing `model_year`. FAA coarse aircraft type, engine type, and category codes can be internally inconsistent; do not infer engine technology or installed configuration from them. Corroborate actual engine configuration with the exact FAA engine make/model and primary manufacturer or TCDS evidence. FAA does not establish marketing generation, factory tier/package, default avionics, installed equipment, historical MSRP, or valuation. Use primary manufacturer evidence for those fields.

The adjudication pass must retrieve this same immutable payload through the local `{FAA_LOOKUP_FUNCTION_NAME}` function before it calls `{CATALOG_SEARCH_FUNCTION_NAME}`. Gemini is not allowed to supply or change a registration number.

Deterministic FAA payload:
{grounding}"#
    ))
}

struct GroundedJsonPass {
    output: String,
    grounding: GroundingAudit,
    interactions: Vec<CurationInteractionAudit>,
}

async fn run_grounded_json_pass(
    client: &GeminiInteractionsClient,
    prompt: String,
    schema: Value,
    purpose: &str,
    structure_task: GeminiTask,
    config: &GeminiRuntimeConfig,
    accounting: &CurationAccountingScope,
) -> Result<GroundedJsonPass> {
    let mut interactions = Vec::new();
    let mut search_result = None;
    let mut search_error = None;
    for attempt in 1..=2 {
        let search_prompt = format!(
            r#"{prompt}

This first stage is source discovery only. Produce a concise evidence dossier in ordinary prose, not JSON. You MUST use Google Search and attach inline URL citations to every factual paragraph. Prefer direct regulator and manufacturer pages. Include complete source URLs in the prose when available, with no more than {MAX_URL_CONTEXT_URLS} distinct sources. Do not make a final catalog decision.{}"#,
            if attempt == 1 {
                ""
            } else {
                " The previous attempt did not produce a verifiable Search trace with URL citations; correct that failure on this attempt."
            }
        );
        let search_request = grounded_tool_request(
            config,
            GeminiTask::AircraftSearchGrounding,
            search_prompt.clone(),
            InteractionTool::GoogleSearch,
            ToolChoice::Validated,
            accounting,
            format!("{purpose}_search_attempt_{attempt}"),
        );
        let search_request_audit = serde_json::json!({
            "model": search_request.model,
            "service_tier": search_request.service_tier,
            "input": search_prompt,
            "tools": ["google_search"],
            "attempt": attempt,
            "store": false
        });
        let search_response = client.create(&search_request).await?;
        let search_grounding = resolved_grounding_audit(client, &search_response)
            .await
            .context("Search citation URLs could not be resolved")?;
        interactions.push(interaction_audit_with_grounding(
            &search_response,
            &format!("{purpose}_search"),
            search_request_audit,
            &search_grounding,
        ));
        match search_response
            .interaction
            .require_curation_output(GroundingRequirement::GoogleSearch)
        {
            Ok(output) if !search_grounding.citation_urls.is_empty() => {
                search_result = Some((output, search_grounding));
                break;
            }
            Ok(_) => {
                search_error =
                    Some("Search discovery returned no resolvable source URLs".to_string())
            }
            Err(error) => search_error = Some(error.to_string()),
        }
    }
    let (search_output, search_grounding) = search_result.ok_or_else(|| {
        anyhow!(
            "Search discovery failed grounding gates after two attempts: {}",
            search_error.as_deref().unwrap_or("unknown failure")
        )
    })?;
    if search_grounding.citation_urls.len() > MAX_URL_CONTEXT_URLS {
        return Err(anyhow!(
            "Search discovery returned {} distinct URLs; URL Context accepts at most {MAX_URL_CONTEXT_URLS}",
            search_grounding.citation_urls.len()
        ));
    }

    let candidate_urls = serde_json::to_string_pretty(&search_grounding.citation_urls)
        .context("resolved Search URLs did not serialize")?;
    let url_context_prompt = format!(
        r#"Re-evaluate the original grounded task using URL Context on the exact resolved URLs below. You MUST invoke URL Context; do not rely on the Search draft or model memory. Produce a concise verified evidence dossier in ordinary prose, not JSON, with inline URL citations on every factual paragraph. State which candidate pages failed retrieval or lack primary authority. A document copy hosted by an aggregator, marketplace, forum, or document-sharing site is secondary even when it reproduces regulator or manufacturer text; never describe the host as regulator, type-certificate, manufacturer, approved-flight-manual, or manufacturer-service-publication authority.

Original task:
{prompt}

Search draft (untrusted until URL Context verifies it):
{search_output}

Resolved candidate URLs:
{candidate_urls}"#
    );
    let mut url_context_result = None;
    let mut url_context_error = None;
    for attempt in 1..=2 {
        let attempt_prompt = if attempt == 1 {
            url_context_prompt.clone()
        } else {
            format!(
                "{url_context_prompt}\n\nThe previous attempt did not produce a successful URL Context trace with inline citations. Correct both failures on this attempt."
            )
        };
        let url_context_request = grounded_tool_request(
            config,
            GeminiTask::AircraftUrlVerification,
            attempt_prompt.clone(),
            InteractionTool::UrlContext,
            ToolChoice::Validated,
            accounting,
            format!("{purpose}_url_context_attempt_{attempt}"),
        );
        let url_context_request_audit = serde_json::json!({
            "model": url_context_request.model,
            "service_tier": url_context_request.service_tier,
            "input": attempt_prompt,
            "tools": ["url_context"],
            "attempt": attempt,
            "store": false
        });
        let url_context_response = client.create(&url_context_request).await?;
        let url_context_grounding = resolved_grounding_audit(client, &url_context_response)
            .await
            .context("URL Context citation URLs could not be resolved")?;
        interactions.push(interaction_audit_with_grounding(
            &url_context_response,
            &format!("{purpose}_url_context"),
            url_context_request_audit,
            &url_context_grounding,
        ));
        match url_context_response
            .interaction
            .require_curation_output(GroundingRequirement::UrlContext)
        {
            Ok(output) if !url_context_grounding.citation_urls.is_empty() => {
                url_context_result = Some((output, url_context_grounding));
                break;
            }
            Ok(_) => {
                url_context_error =
                    Some("URL Context returned no resolvable source URLs".to_string())
            }
            Err(error) => url_context_error = Some(error.to_string()),
        }
    }
    let (url_context_output, url_context_grounding) = url_context_result.ok_or_else(|| {
        anyhow!(
            "URL Context verification failed grounding gates after two attempts: {}",
            url_context_error.as_deref().unwrap_or("unknown failure")
        )
    })?;

    let verified_urls = serde_json::to_string_pretty(&url_context_grounding.citation_urls)
        .context("verified URL Context citations did not serialize")?;
    let structure_prompt = format!(
        r#"Convert the verified URL Context dossier below into the requested JSON contract. This is a structure-only pass: do not research, infer, or add facts. Every `source_url` must be copied exactly from the verified URL list. Omit claims that the dossier does not directly support. Preserve contradictions and uncertainty.

Original task:
{prompt}

Verified URL Context dossier:
{url_context_output}

Verified source URLs:
{verified_urls}"#
    );
    let structure_request = configured_request(
        config,
        structure_task,
        structure_prompt.clone(),
        ToolChoice::None,
        accounting.request_context(structure_task, format!("{purpose}_structure")),
    )
    .with_response_format(ResponseFormat::json(schema)?);
    let structure_request_audit = serde_json::json!({
        "model": structure_request.model,
        "service_tier": structure_request.service_tier,
        "input": structure_prompt,
        "tools": [],
        "response_schema_version": crate::aircraft::curation::AIRCRAFT_IDENTITY_SCHEMA_VERSION,
        "store": false
    });
    let structure_response = client.create(&structure_request).await?;
    let output = structure_response
        .interaction
        .require_curation_output(GroundingRequirement::None)
        .context("structure-only response failed its output contract")?;
    interactions.push(interaction_audit(
        &structure_response,
        &format!("{purpose}_structure"),
        structure_request_audit,
    ));

    Ok(GroundedJsonPass {
        output,
        grounding: GroundingAudit {
            google_search_call_count: search_grounding.google_search_call_count,
            url_context_call_count: url_context_grounding.url_context_call_count,
            citation_urls: url_context_grounding.citation_urls,
        },
        interactions,
    })
}

fn configured_request(
    config: &GeminiRuntimeConfig,
    task: GeminiTask,
    input: impl Into<InteractionInput>,
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

fn grounded_tool_request(
    config: &GeminiRuntimeConfig,
    task: GeminiTask,
    prompt: String,
    tool: InteractionTool,
    tool_choice: ToolChoice,
    accounting: &CurationAccountingScope,
    purpose: impl Into<String>,
) -> CreateInteractionRequest {
    configured_request(
        config,
        task,
        prompt,
        tool_choice,
        accounting.request_context(task, purpose),
    )
    .with_tool(tool)
}

async fn run_catalog_adjudication(
    db: &AppDb,
    client: &GeminiInteractionsClient,
    prompt: String,
    faa_case: &FaaRegistryFunctionResult,
    config: &GeminiRuntimeConfig,
    accounting: &CurationAccountingScope,
) -> Result<(
    AircraftHierarchyAdjudication,
    Vec<AircraftCatalogSearchResponse>,
    usize,
    Vec<FaaRegistryFunctionResult>,
    Vec<CurationInteractionAudit>,
)> {
    let mut history = StatelessHistory::new(prompt)?;
    let faa_tool = faa_registry_lookup_tool(faa_case)?;
    let catalog_tool = InteractionTool::function(
        CATALOG_SEARCH_FUNCTION_NAME,
        "Search the live approved aircraft catalog for identity and collision candidates. Retrieval never proves identity.",
        crate::aircraft::curation::search_aircraft_catalog_function_declaration()["parameters"]
            .clone(),
    )?;
    let response_format = ResponseFormat::json(hierarchy_adjudication_response_schema())?;
    let mut catalog_results = Vec::new();
    let mut faa_results = Vec::new();
    let mut audits = Vec::new();

    let faa_request = configured_request(
        config,
        GeminiTask::AircraftCatalogAdjudication,
        history.input(),
        ToolChoice::Any,
        accounting.request_context(
            GeminiTask::AircraftCatalogAdjudication,
            "identity_adjudication_faa_lookup",
        ),
    )
    .with_tool(faa_tool)
    .with_response_format(response_format.clone());
    let faa_request_audit = serde_json::json!({
        "model": faa_request.model,
        "service_tier": faa_request.service_tier,
        "input": history.steps(),
        "tools": [FAA_LOOKUP_FUNCTION_NAME],
        "response_schema_version": crate::aircraft::curation::AIRCRAFT_IDENTITY_SCHEMA_VERSION,
        "tool_choice": "any",
        "store": false
    });
    let faa_response = client
        .create(&faa_request)
        .await
        .context("Gemini mandatory FAA lookup request failed")?;
    audits.push(interaction_audit(
        &faa_response,
        "identity_adjudication_faa_lookup",
        faa_request_audit,
    ));
    let faa_calls = faa_response
        .interaction
        .function_calls()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    if faa_calls.len() != 1 {
        return Err(anyhow!(
            "Gemini must call {FAA_LOOKUP_FUNCTION_NAME} exactly once before catalog search; observed {} calls",
            faa_calls.len()
        ));
    }
    let faa_call = &faa_calls[0];
    let faa_result = execute_faa_registry_function(faa_call, faa_case)?;
    history.append_response(&faa_response)?;
    history.append_function_result(
        faa_call,
        serde_json::to_value(&faa_result)
            .context("FAA registry function result did not serialize")?,
    )?;
    faa_results.push(faa_result);

    for stage in 0..CATALOG_ADJUDICATION_STAGES {
        let tool_choice = if stage == 0 {
            ToolChoice::Any
        } else {
            ToolChoice::None
        };
        let request = configured_request(
            config,
            GeminiTask::AircraftCatalogAdjudication,
            history.input(),
            tool_choice.clone(),
            accounting.request_context(
                GeminiTask::AircraftCatalogAdjudication,
                if stage == 0 {
                    "identity_adjudication_catalog_search"
                } else {
                    "identity_adjudication_final"
                },
            ),
        )
        .with_tool(catalog_tool.clone())
        .with_response_format(response_format.clone());
        let request_audit = serde_json::json!({
            "model": request.model,
            "service_tier": request.service_tier,
            "input": history.steps(),
            "tools": [CATALOG_SEARCH_FUNCTION_NAME],
            "response_schema_version": crate::aircraft::curation::AIRCRAFT_IDENTITY_SCHEMA_VERSION,
            "tool_choice": if stage == 0 { "any" } else { "none" },
            "store": false
        });
        let response = client
            .create(&request)
            .await
            .context("Gemini aircraft catalog adjudication request failed")?;
        audits.push(interaction_audit(
            &response,
            "identity_adjudication",
            request_audit,
        ));
        let calls = response
            .interaction
            .function_calls()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        history.append_response(&response)?;
        if stage == 0 {
            if calls.len() != 1 {
                return Err(anyhow!(
                    "Gemini must call {CATALOG_SEARCH_FUNCTION_NAME} exactly once after FAA grounding; observed {} calls",
                    calls.len()
                ));
            }
            let call = &calls[0];
            let result = execute_aircraft_catalog_function(db, call).await?;
            history.append_function_result(
                call,
                serde_json::to_value(&result)
                    .context("aircraft catalog function result did not serialize")?,
            )?;
            catalog_results.push(result);
            continue;
        }
        if !calls.is_empty() {
            return Err(anyhow!(
                "Gemini called {CATALOG_SEARCH_FUNCTION_NAME} after catalog tools were disabled"
            ));
        }
        if response.interaction.status != InteractionStatus::Completed {
            return Err(anyhow!(
                "catalog adjudication ended with status {} and no function call",
                response.interaction.status
            ));
        }
        if catalog_results.is_empty() {
            return Err(anyhow!(
                "Gemini returned an adjudication without querying the live aircraft catalog"
            ));
        }
        let output = response
            .interaction
            .require_curation_output(GroundingRequirement::None)?;
        let adjudication = serde_json::from_str::<AircraftHierarchyAdjudication>(&output)
            .context("Gemini hierarchy adjudication output did not match the response contract")?;
        return Ok((
            adjudication,
            catalog_results,
            faa_calls.len(),
            faa_results,
            audits,
        ));
    }
    Err(anyhow!(
        "Gemini did not return a final adjudication after the forced FAA and catalog stages"
    ))
}

fn faa_registry_lookup_tool(faa_case: &FaaRegistryFunctionResult) -> Result<InteractionTool> {
    InteractionTool::function(
        FAA_LOOKUP_FUNCTION_NAME,
        "Return the fixed-snapshot FAA registry grounding already bound to this curation case. The caller may not provide or alter N-numbers.",
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "case_token": {
                    "type": "string",
                    "enum": [faa_case.case_token]
                },
                "cluster_key": {
                    "type": "string",
                    "enum": [faa_case.cluster_key]
                }
            },
            "required": ["case_token", "cluster_key"]
        }),
    )
    .map_err(Into::into)
}

fn execute_faa_registry_function(
    call: &FunctionCallStep,
    expected: &FaaRegistryFunctionResult,
) -> Result<FaaRegistryFunctionResult> {
    if call.name != FAA_LOOKUP_FUNCTION_NAME {
        return Err(anyhow!("Gemini called unsupported function {}", call.name));
    }
    let request = serde_json::from_value::<FaaRegistryFunctionRequest>(call.arguments.clone())
        .context("Gemini supplied invalid lookup_faa_aircraft_registry arguments")?;
    if request.case_token != expected.case_token || request.cluster_key != expected.cluster_key {
        return Err(anyhow!(
            "Gemini attempted to retrieve FAA grounding for a different curation case"
        ));
    }
    Ok(expected.clone())
}

async fn execute_aircraft_catalog_function(
    db: &AppDb,
    call: &FunctionCallStep,
) -> Result<AircraftCatalogSearchResponse> {
    if call.name != CATALOG_SEARCH_FUNCTION_NAME {
        return Err(anyhow!("Gemini called unsupported function {}", call.name));
    }
    let request = serde_json::from_value::<AircraftCatalogSearchRequest>(call.arguments.clone())
        .context("Gemini supplied invalid search_aircraft_catalog arguments")?;
    search_approved_aircraft_catalog(db, &request)
        .await
        .context("live aircraft catalog search failed")
}

fn grounding_audit(response: &InteractionResponse) -> GroundingAudit {
    let search_calls = response
        .interaction
        .steps
        .iter()
        .filter_map(|step| match step {
            InteractionStep::GoogleSearchCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let url_calls = response
        .interaction
        .steps
        .iter()
        .filter_map(|step| match step {
            InteractionStep::UrlContextCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let successful_google_search_calls = response
        .interaction
        .steps
        .iter()
        .filter(|step| match step {
            InteractionStep::GoogleSearchResult(result) => {
                !result.is_error && search_calls.contains(result.call_id.as_str())
            }
            _ => false,
        })
        .count();
    let successful_url_context_calls = response
        .interaction
        .steps
        .iter()
        .filter(|step| match step {
            InteractionStep::UrlContextResult(result) => {
                !result.is_error && url_calls.contains(result.call_id.as_str())
            }
            _ => false,
        })
        .count();
    GroundingAudit {
        google_search_call_count: successful_google_search_calls,
        url_context_call_count: successful_url_context_calls,
        citation_urls: response
            .interaction
            .url_citations()
            .into_iter()
            .filter_map(|citation| citation.citation.url.clone())
            .collect(),
    }
}

async fn resolved_grounding_audit(
    client: &GeminiInteractionsClient,
    response: &InteractionResponse,
) -> Result<GroundingAudit> {
    let mut audit = grounding_audit(response);
    let mut resolved_urls = BTreeSet::new();
    let mut citation_urls = BTreeSet::new();
    for citation in response.interaction.url_citations() {
        citation
            .require_complete()
            .context("Gemini returned an incomplete URL citation")?;
        citation_urls.insert(
            citation
                .citation
                .url
                .clone()
                .expect("complete citation has a URL"),
        );
    }
    for source_url in citation_urls {
        let resolved = client
            .resolve_final_url(&source_url)
            .await
            .with_context(|| format!("could not resolve Gemini citation {source_url}"))?;
        resolved_urls.insert(resolved.final_url.to_string());
    }
    audit.citation_urls = resolved_urls;
    Ok(audit)
}

fn interaction_audit(
    response: &InteractionResponse,
    purpose: &str,
    request_json: Value,
) -> CurationInteractionAudit {
    let grounding = grounding_audit(response);
    let usage = response.interaction.usage.as_ref();
    CurationInteractionAudit {
        purpose: purpose.to_string(),
        request_json,
        interaction_id: response.interaction.id.clone(),
        model: response.interaction.model.clone(),
        status: response.interaction.status.to_string(),
        successful_google_search_calls: grounding.google_search_call_count,
        successful_url_context_calls: grounding.url_context_call_count,
        function_calls: response.interaction.function_calls().len(),
        citation_urls: grounding.citation_urls.into_iter().collect(),
        total_input_tokens: usage.map(|usage| usage.total_input_tokens),
        total_output_tokens: usage.map(|usage| usage.total_output_tokens),
        raw_response: response.raw.clone(),
    }
}

fn interaction_audit_with_grounding(
    response: &InteractionResponse,
    purpose: &str,
    request_json: Value,
    grounding: &GroundingAudit,
) -> CurationInteractionAudit {
    let usage = response.interaction.usage.as_ref();
    CurationInteractionAudit {
        purpose: purpose.to_string(),
        request_json,
        interaction_id: response.interaction.id.clone(),
        model: response.interaction.model.clone(),
        status: response.interaction.status.to_string(),
        successful_google_search_calls: grounding.google_search_call_count,
        successful_url_context_calls: grounding.url_context_call_count,
        function_calls: response.interaction.function_calls().len(),
        citation_urls: grounding.citation_urls.iter().cloned().collect(),
        total_input_tokens: usage.map(|usage| usage.total_input_tokens),
        total_output_tokens: usage.map(|usage| usage.total_output_tokens),
        raw_response: response.raw.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Map};

    use super::*;
    use crate::aircraft::faa::{BlockReason, NotApplicableReason, SerialMatch};

    fn snapshot() -> Snapshot {
        Snapshot {
            id: 17,
            evidence_source_id: 23,
            snapshot_date: "2026-07-20".to_string(),
            source_url: "https://www.faa.gov/example".to_string(),
            archive_sha256: "a".repeat(64),
            source_manifest_sha256: "b".repeat(64),
            target_set_sha256: "c".repeat(64),
        }
    }

    fn grounding(serial_match: SerialMatch) -> AircraftGrounding {
        AircraftGrounding {
            snapshot: snapshot(),
            n_number: "N123AB".to_string(),
            manufacturer_serial_raw: Some("182-123".to_string()),
            manufacturer_serial_key: Some("182123".to_string()),
            aircraft_code: "2072738".to_string(),
            engine_code: Some("41518".to_string()),
            source_record_sha256: "f".repeat(64),
            year_manufactured: Some(2006),
            aircraft: None,
            engine: None,
            serial_match,
        }
    }

    fn faa_case() -> FaaRegistryFunctionResult {
        FaaRegistryFunctionResult {
            case_token: format!("faa_case_{}", "d".repeat(64)),
            cluster_key: "cessna:182:t:2007".to_string(),
            snapshot: snapshot(),
            year_manufactured_is_model_year: false,
            observations: vec![FaaRegistryObservationGrounding {
                listing_id: 42,
                observation_sha256: "e".repeat(64),
                listing_model_year: 2007,
                model_year_differs_from_year_manufactured: true,
                grounding: grounding(SerialMatch::RawExact),
            }],
        }
    }

    fn function_call(arguments: Value) -> FunctionCallStep {
        FunctionCallStep {
            id: "faa-call-1".to_string(),
            name: FAA_LOOKUP_FUNCTION_NAME.to_string(),
            arguments,
            signature: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn mandatory_gate_blocks_missing_foreign_and_serial_conflict() {
        for (outcome, expected_reason) in [
            (
                LookupOutcome::NotApplicable {
                    supplied_registration: None,
                    reason: NotApplicableReason::MissingRegistration,
                },
                BlockReason::MissingRegistration,
            ),
            (
                LookupOutcome::NotApplicable {
                    supplied_registration: Some("C-GABC".to_string()),
                    reason: NotApplicableReason::ForeignRegistration,
                },
                BlockReason::NonNRegistration,
            ),
            (
                LookupOutcome::Found {
                    grounding: grounding(SerialMatch::Conflict),
                },
                BlockReason::SerialConflict,
            ),
        ] {
            assert!(matches!(
                require_eligible(outcome),
                Eligibility::Blocked { reason, .. } if reason == expected_reason
            ));
        }
    }

    #[test]
    fn faa_function_declaration_accepts_only_the_bound_case() {
        let faa_case = faa_case();
        let declaration = serde_json::to_value(faa_registry_lookup_tool(&faa_case).unwrap())
            .expect("tool declaration serializes");
        assert_eq!(declaration["name"], FAA_LOOKUP_FUNCTION_NAME);
        assert_eq!(
            declaration["parameters"]["properties"]["case_token"]["enum"][0],
            faa_case.case_token
        );
        assert_eq!(
            declaration["parameters"]["properties"]["cluster_key"]["enum"][0],
            faa_case.cluster_key
        );
        assert!(declaration["parameters"]["properties"]
            .get("registration_number")
            .is_none());
        assert_eq!(declaration["parameters"]["additionalProperties"], false);
    }

    #[test]
    fn faa_function_rejects_changed_case_or_registration_arguments() {
        let faa_case = faa_case();
        let accepted = function_call(json!({
            "case_token": faa_case.case_token,
            "cluster_key": faa_case.cluster_key,
        }));
        assert_eq!(
            execute_faa_registry_function(&accepted, &faa_case).unwrap(),
            faa_case
        );

        let changed = function_call(json!({
            "case_token": "faa_case_attacker_selected",
            "cluster_key": faa_case.cluster_key,
        }));
        assert!(execute_faa_registry_function(&changed, &faa_case).is_err());

        let registration_injection = function_call(json!({
            "case_token": faa_case.case_token,
            "cluster_key": faa_case.cluster_key,
            "registration_number": "N99999",
        }));
        assert!(execute_faa_registry_function(&registration_injection, &faa_case).is_err());
    }

    #[test]
    fn faa_year_manufactured_is_never_promoted_to_model_year() {
        let faa_case = faa_case();
        assert!(!faa_case.year_manufactured_is_model_year);
        assert_eq!(faa_case.observations[0].listing_model_year, 2007);
        assert!(faa_case.observations[0].model_year_differs_from_year_manufactured);
        assert_eq!(
            faa_case.observations[0].grounding.year_manufactured,
            Some(2006)
        );
        let prompt =
            append_faa_grounding_context("Audit this aircraft.".to_string(), &faa_case, "test")
                .unwrap();
        assert!(prompt.contains("MUST NOT replace"));
    }

    #[test]
    fn case_accepts_multiple_target_projections_only_for_the_same_faa_release() {
        let mut selected = Some(snapshot());
        let mut expanded = snapshot();
        expanded.id += 1;
        expanded.target_set_sha256 = "d".repeat(64);
        merge_reported_snapshot(&mut selected, &expanded).unwrap();
        assert_eq!(selected.as_ref().map(|snapshot| snapshot.id), Some(18));

        let mut different_release = expanded;
        different_release.id += 1;
        different_release.archive_sha256 = "e".repeat(64);
        assert!(merge_reported_snapshot(&mut selected, &different_release).is_err());
    }

    #[test]
    fn configured_requests_use_the_named_task_route() {
        let mut config = GeminiRuntimeConfig::default();
        let route = config
            .tasks
            .get_mut(&GeminiTask::AircraftUrlVerification)
            .unwrap();
        route.model = "gemini-3.5-flash-lite".to_string();
        route.service_tier = Some("flex".to_string());
        route.thinking_level = ConfigThinkingLevel::Minimal;
        route.max_output_tokens = 3210;
        config.validate().unwrap();

        let request = configured_request(
            &config,
            GeminiTask::AircraftUrlVerification,
            "verify these URLs",
            ToolChoice::Validated,
            InteractionAccountingContext::new(
                GeminiTask::AircraftUrlVerification,
                "fixture_url_verification",
            ),
        );
        assert_eq!(request.model, "gemini-3.5-flash-lite");
        assert_eq!(request.service_tier.as_deref(), Some("flex"));
        let generation = request.generation_config.unwrap();
        assert_eq!(generation.max_output_tokens, Some(3210));
        assert!(matches!(
            generation.thinking_level,
            Some(ThinkingLevel::Minimal)
        ));
        assert!(matches!(
            generation.tool_choice,
            Some(ToolChoice::Validated)
        ));
    }
}
