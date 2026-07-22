//! Evidence-first aircraft hierarchy curation.
//!
//! This module contains the deterministic contract around Gemini. The model
//! may research and propose identities, but it cannot approve catalog rows.
//! Mechanical normalization is used only to retrieve candidates; evidence,
//! exact identifiers, and an independent verification pass determine whether
//! a proposal is reviewable.

pub mod profile;
pub mod visual;
pub mod workflow;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use url::Url;

use crate::aircraft::catalog::{
    validate_aircraft_hierarchy_proposal, AircraftHierarchyProposal, CatalogEntityProposal,
    EvidenceClaimProposal, ValidationErrors, ValidationIssue,
};
use crate::aircraft::observations::AircraftIdentityObservation;
use crate::db::{AppDb, DatabaseBackend};

pub const AIRCRAFT_IDENTITY_PROMPT_VERSION: &str = "aircraft-identity-v1";
pub const AIRCRAFT_IDENTITY_SCHEMA_VERSION: &str = "aircraft-identity-schema-v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CurationConfidence {
    Low,
    Medium,
    High,
    VeryHigh,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HierarchyEntityKind {
    Make,
    Family,
    Designation,
    Generation,
    Package,
}

impl HierarchyEntityKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Make => "make",
            Self::Family => "family",
            Self::Designation => "designation",
            Self::Generation => "generation",
            Self::Package => "package",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityResolutionAction {
    MatchExisting,
    ProposeNew,
    NotApplicable,
    Unresolved,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogEntityDecision {
    pub action: EntityResolutionAction,
    pub existing_catalog_id: Option<i64>,
    pub display_name: Option<String>,
    pub authoritative_designator: Option<String>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub rationale: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AircraftIdentityEvidenceResearch {
    pub subject_summary: String,
    pub claims: Vec<EvidenceClaimProposal>,
    #[serde(default)]
    pub contradictions: Vec<String>,
    #[serde(default)]
    pub unresolved_questions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AircraftHierarchyAdjudication {
    pub confidence: CurationConfidence,
    pub make: CatalogEntityDecision,
    pub family: CatalogEntityDecision,
    pub designation: CatalogEntityDecision,
    pub generation: CatalogEntityDecision,
    pub package: CatalogEntityDecision,
    #[serde(default)]
    pub material_distinctions: Vec<String>,
    #[serde(default)]
    pub unresolved_questions: Vec<String>,
    pub rationale: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationVerdict {
    Confirm,
    Reject,
    Ambiguous,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DifferentiationCheck {
    pub compared_labels: Vec<String>,
    pub conclusion: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AircraftHierarchyVerification {
    pub verdict: VerificationVerdict,
    pub confidence: CurationConfidence,
    #[serde(default)]
    pub verified_evidence_ids: Vec<String>,
    #[serde(default)]
    pub differentiation_checks: Vec<DifferentiationCheck>,
    #[serde(default)]
    pub errors: Vec<String>,
    pub rationale: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct GroundingAudit {
    pub google_search_call_count: usize,
    pub url_context_call_count: usize,
    pub citation_urls: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CatalogCandidateRegistry {
    pub ids_by_kind: BTreeMap<HierarchyEntityKind, BTreeSet<i64>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AircraftCatalogSearchRequest {
    pub observed_make: String,
    pub observed_family: String,
    pub observed_designation: String,
    pub observed_generation: Option<String>,
    pub observed_package: Option<String>,
    pub model_year: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AircraftCatalogCandidate {
    pub entity_kind: HierarchyEntityKind,
    pub catalog_id: i64,
    pub display_name: String,
    pub authoritative_designator: Option<String>,
    pub parent_catalog_id: Option<i64>,
    pub aliases: Vec<String>,
    pub identifiers: Vec<String>,
    pub retrieval_score: f64,
    pub retrieval_reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AircraftCatalogSearchResponse {
    pub catalog_revision: String,
    pub catalog_is_empty: bool,
    pub allowed_existing_ids_by_kind: BTreeMap<HierarchyEntityKind, Vec<i64>>,
    pub candidates: Vec<AircraftCatalogCandidate>,
    pub warning: String,
}

impl AircraftCatalogSearchResponse {
    pub fn candidate_registry(&self) -> CatalogCandidateRegistry {
        let mut registry = CatalogCandidateRegistry::default();
        for candidate in &self.candidates {
            registry.insert(candidate.entity_kind, candidate.catalog_id);
        }
        registry
    }
}

#[derive(Clone, Debug, FromRow, Serialize)]
struct AircraftCatalogBaseRow {
    entity_kind: String,
    entity_id: i64,
    parent_id: Option<i64>,
    display_name: String,
    authoritative_designator: Option<String>,
    normalized_name: String,
}

#[derive(Clone, Debug, FromRow, Serialize)]
struct AircraftCatalogLookupRow {
    entity_kind: String,
    entity_id: i64,
    lookup_kind: String,
    display_value: String,
    normalized_value: String,
}

/// Search the approved catalog for candidates. Scores are deliberately only
/// retrieval hints. Same-family designation siblings are returned even with a
/// low score so collision-prone identities remain visible to the adjudicator.
pub async fn search_approved_aircraft_catalog(
    db: &AppDb,
    request: &AircraftCatalogSearchRequest,
) -> Result<AircraftCatalogSearchResponse, sqlx::Error> {
    let base_rows = load_catalog_base_rows(db).await?;
    let lookup_rows = load_catalog_lookup_rows(db).await?;
    let catalog_revision = catalog_revision(&base_rows, &lookup_rows);
    let mut lookups = BTreeMap::<(String, i64), Vec<&AircraftCatalogLookupRow>>::new();
    for lookup in &lookup_rows {
        lookups
            .entry((lookup.entity_kind.clone(), lookup.entity_id))
            .or_default()
            .push(lookup);
    }

    let query_by_kind = BTreeMap::from([
        (HierarchyEntityKind::Make, request.observed_make.as_str()),
        (
            HierarchyEntityKind::Family,
            request.observed_family.as_str(),
        ),
        (
            HierarchyEntityKind::Designation,
            request.observed_designation.as_str(),
        ),
        (
            HierarchyEntityKind::Generation,
            request.observed_generation.as_deref().unwrap_or(""),
        ),
        (
            HierarchyEntityKind::Package,
            request.observed_package.as_deref().unwrap_or(""),
        ),
    ]);

    let family_matches = base_rows
        .iter()
        .filter(|row| row.entity_kind == "family")
        .filter(|row| {
            retrieval_score(
                request.observed_family.as_str(),
                &row.display_name,
                row.authoritative_designator.as_deref(),
                lookups.get(&(row.entity_kind.clone(), row.entity_id)),
            )
            .0 >= 0.74
        })
        .map(|row| row.entity_id)
        .collect::<BTreeSet<_>>();

    let mut candidates = Vec::new();
    for row in &base_rows {
        let Some(kind) = parse_entity_kind(&row.entity_kind) else {
            continue;
        };
        let query = query_by_kind.get(&kind).copied().unwrap_or_default();
        let row_lookups = lookups.get(&(row.entity_kind.clone(), row.entity_id));
        let (score, mut reasons) = retrieval_score(
            query,
            &row.display_name,
            row.authoritative_designator.as_deref(),
            row_lookups,
        );
        let same_family_sibling = matches!(
            kind,
            HierarchyEntityKind::Designation
                | HierarchyEntityKind::Generation
                | HierarchyEntityKind::Package
        ) && row.parent_id.is_some_and(|id| family_matches.contains(&id));
        if score <= 0.0 && !same_family_sibling {
            continue;
        }
        if same_family_sibling {
            reasons.push("same_family_collision_candidate".to_string());
        }
        let aliases = row_lookups
            .into_iter()
            .flatten()
            .filter(|lookup| lookup.lookup_kind == "alias")
            .map(|lookup| lookup.display_value.clone())
            .collect::<Vec<_>>();
        let identifiers = row_lookups
            .into_iter()
            .flatten()
            .filter(|lookup| lookup.lookup_kind == "identifier")
            .map(|lookup| lookup.display_value.clone())
            .collect::<Vec<_>>();
        candidates.push(AircraftCatalogCandidate {
            entity_kind: kind,
            catalog_id: row.entity_id,
            display_name: row.display_name.clone(),
            authoritative_designator: row.authoritative_designator.clone(),
            parent_catalog_id: row.parent_id,
            aliases,
            identifiers,
            retrieval_score: score,
            retrieval_reasons: reasons,
        });
    }
    candidates.sort_by(|left, right| {
        left.entity_kind
            .cmp(&right.entity_kind)
            .then_with(|| right.retrieval_score.total_cmp(&left.retrieval_score))
            .then_with(|| left.catalog_id.cmp(&right.catalog_id))
    });
    let mut per_kind = BTreeMap::<HierarchyEntityKind, usize>::new();
    candidates.retain(|candidate| {
        let count = per_kind.entry(candidate.entity_kind).or_default();
        *count += 1;
        *count <= 50
    });

    let mut allowed_existing_ids_by_kind = BTreeMap::new();
    for candidate in &candidates {
        allowed_existing_ids_by_kind
            .entry(candidate.entity_kind)
            .or_insert_with(Vec::new)
            .push(candidate.catalog_id);
    }
    Ok(AircraftCatalogSearchResponse {
        catalog_revision,
        catalog_is_empty: base_rows.is_empty(),
        allowed_existing_ids_by_kind,
        candidates,
        warning: "Candidate retrieval is not identity evidence and never authorizes a merge. `match_existing` is forbidden unless the exact ID appears in allowed_existing_ids_by_kind for that entity kind; use `propose_new` when the list is empty."
            .to_string(),
    })
}

fn retrieval_score(
    query: &str,
    display_name: &str,
    designator: Option<&str>,
    lookups: Option<&Vec<&AircraftCatalogLookupRow>>,
) -> (f64, Vec<String>) {
    if query.trim().is_empty() {
        return (0.0, Vec::new());
    }
    let normalized_query = crate::aircraft::catalog::normalize_aircraft_retrieval_text(query);
    let designator_query =
        crate::aircraft::catalog::normalize_aircraft_designator_retrieval_key(query);
    let mut score = 0.0_f64;
    let mut reasons = Vec::new();
    let normalized_display =
        crate::aircraft::catalog::normalize_aircraft_retrieval_text(display_name);
    if normalized_query == normalized_display {
        score = 1.0;
        reasons.push("exact_display_retrieval_key".to_string());
    } else if normalized_display.contains(&normalized_query)
        || normalized_query.contains(&normalized_display)
    {
        score = score.max(0.75);
        reasons.push("display_substring_retrieval_key".to_string());
    } else {
        let overlap = token_overlap(&normalized_query, &normalized_display);
        if overlap > 0.0 {
            score = score.max(overlap * 0.7);
            reasons.push("display_token_overlap".to_string());
        }
    }
    if designator.is_some_and(|value| {
        crate::aircraft::catalog::normalize_aircraft_designator_retrieval_key(value)
            == designator_query
    }) {
        score = 1.0;
        reasons.push("exact_authoritative_designator_key".to_string());
    }
    for lookup in lookups.into_iter().flatten() {
        if lookup.normalized_value == normalized_query
            || crate::aircraft::catalog::normalize_aircraft_designator_retrieval_key(
                &lookup.display_value,
            ) == designator_query
        {
            score = 1.0;
            reasons.push(format!("exact_{}_retrieval_key", lookup.lookup_kind));
        }
    }
    reasons.sort();
    reasons.dedup();
    (score, reasons)
}

fn token_overlap(left: &str, right: &str) -> f64 {
    let left = left.split_whitespace().collect::<BTreeSet<_>>();
    let right = right.split_whitespace().collect::<BTreeSet<_>>();
    let union = left.union(&right).count();
    if union == 0 {
        0.0
    } else {
        left.intersection(&right).count() as f64 / union as f64
    }
}

fn parse_entity_kind(value: &str) -> Option<HierarchyEntityKind> {
    match value {
        "make" => Some(HierarchyEntityKind::Make),
        "family" => Some(HierarchyEntityKind::Family),
        "designation" => Some(HierarchyEntityKind::Designation),
        "generation" => Some(HierarchyEntityKind::Generation),
        "package" => Some(HierarchyEntityKind::Package),
        _ => None,
    }
}

fn catalog_revision(
    base_rows: &[AircraftCatalogBaseRow],
    lookup_rows: &[AircraftCatalogLookupRow],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        serde_json::to_vec(&(base_rows, lookup_rows)).expect("catalog rows serialize for hashing"),
    );
    format!("sha256:{:x}", hasher.finalize())
}

async fn load_catalog_base_rows(db: &AppDb) -> Result<Vec<AircraftCatalogBaseRow>, sqlx::Error> {
    let sqlite_sql = r#"
        SELECT 'make' AS entity_kind, id AS entity_id, NULL AS parent_id,
               name AS display_name, NULL AS authoritative_designator,
               normalized_name
        FROM aircraft_makes
        UNION ALL
        SELECT 'family', id, aircraft_make_id, name, NULL, normalized_name
        FROM aircraft_model_families
        UNION ALL
        SELECT 'designation', id, aircraft_model_family_id, display_name,
               official_designation, normalized_official_designation
        FROM aircraft_designations
        UNION ALL
        SELECT 'generation', id, aircraft_model_family_id, name, NULL, normalized_name
        FROM aircraft_generations
        UNION ALL
        SELECT 'package', id, aircraft_model_family_id, name, NULL, normalized_name
        FROM aircraft_factory_packages
        ORDER BY entity_kind, entity_id
    "#;
    let postgres_sql = r#"
        SELECT 'make'::TEXT AS entity_kind, id AS entity_id, NULL::BIGINT AS parent_id,
               name AS display_name, NULL::TEXT AS authoritative_designator,
               normalized_name
        FROM aircraft_makes
        UNION ALL
        SELECT 'family'::TEXT, id, aircraft_make_id, name, NULL::TEXT, normalized_name
        FROM aircraft_model_families
        UNION ALL
        SELECT 'designation'::TEXT, id, aircraft_model_family_id, display_name,
               official_designation, normalized_official_designation
        FROM aircraft_designations
        UNION ALL
        SELECT 'generation'::TEXT, id, aircraft_model_family_id, name, NULL::TEXT, normalized_name
        FROM aircraft_generations
        UNION ALL
        SELECT 'package'::TEXT, id, aircraft_model_family_id, name, NULL::TEXT, normalized_name
        FROM aircraft_factory_packages
        ORDER BY entity_kind, entity_id
    "#;
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, AircraftCatalogBaseRow>(sqlite_sql)
                .fetch_all(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, AircraftCatalogBaseRow>(postgres_sql)
                .fetch_all(pool)
                .await
        }
    }
}

async fn load_catalog_lookup_rows(
    db: &AppDb,
) -> Result<Vec<AircraftCatalogLookupRow>, sqlx::Error> {
    let sql = r#"
        SELECT 'make' AS entity_kind, aircraft_make_id AS entity_id,
               'alias' AS lookup_kind, alias AS display_value,
               normalized_alias AS normalized_value
        FROM aircraft_make_aliases
        UNION ALL
        SELECT 'family', aircraft_model_family_id, 'alias', alias, normalized_alias
        FROM aircraft_family_aliases
        UNION ALL
        SELECT 'designation', aircraft_designation_id, 'alias', alias, normalized_alias
        FROM aircraft_designation_aliases
        UNION ALL
        SELECT 'designation', aircraft_designation_id, 'identifier', identifier_value,
               normalized_identifier_value
        FROM aircraft_designation_identifiers
        ORDER BY entity_kind, entity_id, lookup_kind, normalized_value
    "#;
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, AircraftCatalogLookupRow>(sql)
                .fetch_all(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, AircraftCatalogLookupRow>(sql)
                .fetch_all(pool)
                .await
        }
    }
}

impl CatalogCandidateRegistry {
    pub fn insert(&mut self, kind: HierarchyEntityKind, id: i64) {
        self.ids_by_kind.entry(kind).or_default().insert(id);
    }

    fn contains(&self, kind: HierarchyEntityKind, id: i64) -> bool {
        self.ids_by_kind
            .get(&kind)
            .is_some_and(|ids| ids.contains(&id))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ReviewableAircraftHierarchy {
    pub proposal: AircraftHierarchyProposal,
    pub adjudication: AircraftHierarchyAdjudication,
    pub verification: AircraftHierarchyVerification,
}

pub fn validate_identity_evidence_research(
    research: &AircraftIdentityEvidenceResearch,
    grounding: &GroundingAudit,
) -> Result<(), ValidationErrors> {
    let mut issues = Vec::new();
    if grounding.google_search_call_count == 0 {
        issues.push(issue(
            "google_search_not_observed",
            "the evidence pass did not execute Google Search",
        ));
    }
    if grounding.url_context_call_count == 0 {
        issues.push(issue(
            "url_context_not_observed",
            "the evidence pass did not inspect selected sources with URL Context",
        ));
    }
    if research.claims.is_empty() {
        issues.push(issue(
            "missing_evidence_claims",
            "the evidence pass returned no claims",
        ));
    }
    let mut evidence_ids = BTreeSet::new();
    for (index, claim) in research.claims.iter().enumerate() {
        if !evidence_ids.insert(claim.evidence_id.trim()) {
            issues.push(issue(
                "duplicate_evidence_id",
                format!("evidence claim {index} reuses id {}", claim.evidence_id),
            ));
        }
        if !citation_matches(&grounding.citation_urls, &claim.source_url) {
            issues.push(issue(
                "uncited_evidence_url",
                format!(
                    "evidence claim {} uses a URL absent from model-output citations",
                    claim.evidence_id
                ),
            ));
        }
        if matches!(
            claim.source_kind,
            crate::aircraft::catalog::EvidenceSourceKind::Regulator
                | crate::aircraft::catalog::EvidenceSourceKind::TypeCertificate
        ) && !is_faa_source_url(&claim.source_url)
        {
            issues.push(issue(
                "non_faa_regulator_source",
                format!(
                    "evidence claim {} labels a non-FAA host as regulator/type-certificate authority",
                    claim.evidence_id
                ),
            ));
        }
    }
    validation_result(issues)
}

fn is_faa_source_url(value: &str) -> bool {
    Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "faa.gov" || host.ends_with(".faa.gov"))
}

pub(super) fn validate_aircraft_hierarchy_adjudication(
    research: &AircraftIdentityEvidenceResearch,
    evidence_grounding: &GroundingAudit,
    adjudication: &AircraftHierarchyAdjudication,
    catalog_candidates: &CatalogCandidateRegistry,
    catalog_function_call_count: usize,
) -> Result<AircraftHierarchyProposal, ValidationErrors> {
    let mut issues = Vec::new();
    if let Err(errors) = validate_identity_evidence_research(research, evidence_grounding) {
        issues.extend(errors.0);
    }
    if catalog_function_call_count == 0 {
        issues.push(issue(
            "catalog_function_not_called",
            "the adjudication pass did not call the live aircraft catalog search function",
        ));
    }
    if adjudication.confidence != CurationConfidence::VeryHigh {
        issues.push(issue(
            "adjudication_confidence_too_low",
            "hierarchy proposals are reviewable only at very_high confidence",
        ));
    }

    let make = resolved_entity(
        HierarchyEntityKind::Make,
        &adjudication.make,
        false,
        catalog_candidates,
        research,
        &mut issues,
    );
    let family = resolved_entity(
        HierarchyEntityKind::Family,
        &adjudication.family,
        false,
        catalog_candidates,
        research,
        &mut issues,
    );
    let designation = resolved_entity(
        HierarchyEntityKind::Designation,
        &adjudication.designation,
        false,
        catalog_candidates,
        research,
        &mut issues,
    );
    let generation = resolved_entity(
        HierarchyEntityKind::Generation,
        &adjudication.generation,
        true,
        catalog_candidates,
        research,
        &mut issues,
    );
    let tier = resolved_entity(
        HierarchyEntityKind::Package,
        &adjudication.package,
        true,
        catalog_candidates,
        research,
        &mut issues,
    );

    if !issues.is_empty() {
        return Err(ValidationErrors::from_unsorted(issues));
    }
    let proposal = AircraftHierarchyProposal {
        manufacturer: make.expect("required make validated"),
        model_family: family.expect("required family validated"),
        certified_variant: designation.expect("required designation validated"),
        generation,
        tier,
        evidence: research.claims.clone(),
    };
    validate_aircraft_hierarchy_proposal(&proposal)?;
    Ok(proposal)
}

fn build_reviewable_aircraft_hierarchy(
    research: &AircraftIdentityEvidenceResearch,
    evidence_grounding: &GroundingAudit,
    adjudication: AircraftHierarchyAdjudication,
    catalog_candidates: &CatalogCandidateRegistry,
    catalog_function_call_count: usize,
    verification: AircraftHierarchyVerification,
    verification_grounding: &GroundingAudit,
) -> Result<ReviewableAircraftHierarchy, ValidationErrors> {
    let proposal = validate_aircraft_hierarchy_adjudication(
        research,
        evidence_grounding,
        &adjudication,
        catalog_candidates,
        catalog_function_call_count,
    )?;
    let mut issues = Vec::new();

    if verification_grounding.google_search_call_count == 0
        || verification_grounding.url_context_call_count == 0
    {
        issues.push(issue(
            "verifier_grounding_not_observed",
            "the independent verifier did not execute Search and URL Context",
        ));
    }
    if verification.verdict != VerificationVerdict::Confirm
        || verification.confidence != CurationConfidence::VeryHigh
    {
        issues.push(issue(
            "independent_verification_failed",
            "a fresh verifier must confirm the proposal at very_high confidence",
        ));
    }
    let research_ids = research
        .claims
        .iter()
        .map(|claim| claim.evidence_id.as_str())
        .collect::<BTreeSet<_>>();
    for id in &verification.verified_evidence_ids {
        if !research_ids.contains(id.as_str()) {
            issues.push(issue(
                "verifier_unknown_evidence",
                format!("the verifier referenced unknown evidence id {id}"),
            ));
        }
    }

    if !issues.is_empty() {
        return Err(ValidationErrors::from_unsorted(issues));
    }
    Ok(ReviewableAircraftHierarchy {
        proposal,
        adjudication,
        verification,
    })
}

fn resolved_entity(
    kind: HierarchyEntityKind,
    decision: &CatalogEntityDecision,
    optional: bool,
    candidates: &CatalogCandidateRegistry,
    research: &AircraftIdentityEvidenceResearch,
    issues: &mut Vec<ValidationIssue>,
) -> Option<CatalogEntityProposal> {
    let known_evidence = research
        .claims
        .iter()
        .map(|claim| claim.evidence_id.as_str())
        .collect::<BTreeSet<_>>();
    if decision.evidence_ids.is_empty()
        && !matches!(decision.action, EntityResolutionAction::NotApplicable)
    {
        issues.push(issue(
            "entity_missing_evidence",
            format!("{} decision has no evidence ids", kind.as_str()),
        ));
    }
    for id in &decision.evidence_ids {
        if !known_evidence.contains(id.as_str()) {
            issues.push(issue(
                "entity_unknown_evidence",
                format!(
                    "{} decision references unknown evidence id {id}",
                    kind.as_str()
                ),
            ));
        }
    }
    if matches!(
        kind,
        HierarchyEntityKind::Make | HierarchyEntityKind::Family | HierarchyEntityKind::Designation
    ) {
        let has_faa_identity_evidence = decision.evidence_ids.iter().any(|id| {
            research.claims.iter().any(|claim| {
                claim.evidence_id == *id
                    && matches!(
                        claim.source_kind,
                        crate::aircraft::catalog::EvidenceSourceKind::Regulator
                            | crate::aircraft::catalog::EvidenceSourceKind::TypeCertificate
                    )
                    && claim
                        .supports
                        .contains(&crate::aircraft::catalog::EvidenceClaimKind::HierarchyIdentity)
                    && is_faa_source_url(&claim.source_url)
            })
        });
        if !has_faa_identity_evidence {
            issues.push(issue(
                "missing_faa_identity_evidence",
                format!(
                    "{} decision must cite FAA regulator/type-certificate hierarchy evidence",
                    kind.as_str()
                ),
            ));
        }
    }

    match decision.action {
        EntityResolutionAction::MatchExisting => {
            let Some(id) = decision.existing_catalog_id else {
                issues.push(issue(
                    "missing_existing_catalog_id",
                    format!("{} match has no catalog id", kind.as_str()),
                ));
                return None;
            };
            if !candidates.contains(kind, id) {
                issues.push(issue(
                    "catalog_id_not_retrieved",
                    format!(
                        "{} catalog id {id} was not returned by the live catalog",
                        kind.as_str()
                    ),
                ));
            }
            Some(CatalogEntityProposal {
                existing_catalog_id: Some(id),
                display_name: required_label(kind, decision, issues),
                authoritative_designator: decision.authoritative_designator.clone(),
            })
        }
        EntityResolutionAction::ProposeNew => {
            if decision.existing_catalog_id.is_some() {
                issues.push(issue(
                    "new_entity_has_catalog_id",
                    format!(
                        "new {} proposal unexpectedly has a catalog id",
                        kind.as_str()
                    ),
                ));
            }
            Some(CatalogEntityProposal {
                existing_catalog_id: None,
                display_name: required_label(kind, decision, issues),
                authoritative_designator: decision.authoritative_designator.clone(),
            })
        }
        EntityResolutionAction::NotApplicable if optional => None,
        EntityResolutionAction::NotApplicable => {
            issues.push(issue(
                "required_entity_not_applicable",
                format!("{} cannot be not_applicable", kind.as_str()),
            ));
            None
        }
        EntityResolutionAction::Unresolved => {
            issues.push(issue(
                "unresolved_hierarchy_dimension",
                format!("{} remains unresolved", kind.as_str()),
            ));
            None
        }
    }
}

fn required_label(
    kind: HierarchyEntityKind,
    decision: &CatalogEntityDecision,
    issues: &mut Vec<ValidationIssue>,
) -> String {
    match decision
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        Some(label) => label.to_string(),
        None => {
            issues.push(issue(
                "missing_entity_label",
                format!("{} decision has no display name", kind.as_str()),
            ));
            String::new()
        }
    }
}

fn citation_matches(citations: &BTreeSet<String>, claim_url: &str) -> bool {
    let normalized_claim = normalize_url_for_citation_match(claim_url);
    citations
        .iter()
        .any(|citation| normalize_url_for_citation_match(citation) == normalized_claim)
}

fn normalize_url_for_citation_match(value: &str) -> &str {
    value.trim().trim_end_matches('/')
}

fn issue(code: impl Into<String>, message: impl Into<String>) -> ValidationIssue {
    ValidationIssue::new(code, message)
}

fn validation_result(issues: Vec<ValidationIssue>) -> Result<(), ValidationErrors> {
    if issues.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_unsorted(issues))
    }
}

pub fn build_identity_evidence_prompt(observations: &[&AircraftIdentityObservation]) -> String {
    let observation_json = serde_json::to_string_pretty(observations)
        .expect("aircraft observations serialize for prompt construction");
    format!(
        r#"Research the authoritative aircraft identity represented by the retained listing observations below.

This is an evidence-discovery pass, not a normalization pass and not permission to update a database. You must execute Google Search on this first attempt. Prefer manufacturer publications, regulator type-certificate data, approved flight manuals, and manufacturer service publications. Use URL Context to inspect the exact primary pages you select. For a U.S. N-number, FAA registry evidence is controlling over the listing and model memory for registration, manufacturer serial number, year manufactured, FAA make/model/series, and FAA engine code. A conflict with FAA data must be returned as a contradiction, never silently reconciled. Marketplace listings may explain what was observed but cannot establish canonical identity, production applicability, factory package, or standard equipment.

Identify separately: legal/manufacturer make, model family, exact certified designation, marketing generation (if one actually exists), and factory tier/package (if one actually exists). Preserve material prefixes and suffixes: 182T is not T182T, SR22 is not SR22T, G6 is a generation, and GTS is a package/tier. Treat “Skylane” as a possible marketing/popular name, not automatically as the certified designation. Treat suspicious OCR or extraction labels such as “182I” as unresolved unless primary evidence proves them.

Every returned claim must use a direct http(s) source URL that appears in this response's citations, a concise source excerpt, and explicit supported claim kinds. If authoritative sources disagree, return the contradiction. Do not fill gaps from model memory.

Retained observations:
{observation_json}"#
    )
}

pub fn build_hierarchy_adjudication_prompt(
    observations: &[&AircraftIdentityObservation],
    research: &AircraftIdentityEvidenceResearch,
) -> String {
    let observations = serde_json::to_string_pretty(observations)
        .expect("aircraft observations serialize for adjudication prompt");
    let evidence = serde_json::to_string_pretty(research)
        .expect("aircraft evidence serializes for adjudication prompt");
    format!(
        r#"Resolve one aircraft hierarchy from retained literal observations and an evidence bundle.

Before deciding, you must call search_aircraft_catalog exactly as provided. It returns the current approved catalog; only IDs returned by that function may be selected. Similar spelling is candidate retrieval, never proof of identity. Choose match_existing only when authoritative evidence establishes the same entity. Choose propose_new only after reviewing all returned collision candidates. Choose unresolved whenever evidence does not clearly distinguish designation, generation, or package. Choose not_applicable only when primary evidence establishes that the optional generation or package dimension does not apply.

Make, family, and exact certified designation are required. Generation and package are independent optional dimensions. Do not put a generation or package into the certified designation merely because the listing combines the words. Confidence may be very_high only when every selected dimension has direct primary evidence and no unresolved collision.

Retained observations:
{observations}

Validated evidence bundle:
{evidence}"#
    )
}

pub fn build_hierarchy_verification_prompt(
    observations: &[&AircraftIdentityObservation],
    research: &AircraftIdentityEvidenceResearch,
    adjudication: &AircraftHierarchyAdjudication,
) -> String {
    let payload = json!({
        "retained_observations": observations,
        "evidence_bundle": research,
        "proposed_adjudication": adjudication,
    });
    format!(
        r#"Independently audit the proposed aircraft hierarchy below. This is a fresh verification pass: do not defer to the first adjudicator's confidence or rationale.

Execute Google Search and use URL Context on the primary sources you rely on. Check every evidence URL, every existing/new decision, exact designation characters, model-year applicability, and the separation of certified designation from generation and package. Explicitly compare collision-prone pairs when relevant (182T/T182T, SR22/SR22T, generation/tier, and popular name/certified model). Confirm at very_high confidence only if the proposal is fully proved by primary evidence. Otherwise reject or return ambiguous. Reference only evidence IDs present in the supplied bundle; report newly discovered contradictions as errors rather than silently replacing the evidence bundle.

Audit payload:
{}"#,
        serde_json::to_string_pretty(&payload).expect("verification payload serializes")
    )
}

pub fn identity_evidence_response_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "subject_summary": {"type": "string"},
            "claims": {
                "type": "array",
                "items": evidence_claim_schema()
            },
            "contradictions": {"type": "array", "items": {"type": "string"}},
            "unresolved_questions": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["subject_summary", "claims", "contradictions", "unresolved_questions"]
    })
}

fn evidence_claim_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "evidence_id": {"type": "string"},
            "source_url": {"type": "string"},
            "source_title": {"type": "string"},
            "evidence_excerpt": {"type": "string"},
            "source_kind": {
                "type": "string",
                "enum": [
                    "manufacturer", "regulator", "type_certificate",
                    "approved_flight_manual", "manufacturer_service_publication",
                    "recognized_secondary", "marketplace_listing"
                ]
            },
            "supports": {
                "type": "array",
                "items": {
                    "type": "string",
                    "enum": [
                        "hierarchy_identity", "production_applicability",
                        "market_applicability", "factory_configuration",
                        "reference_price", "component_identity", "material_feature"
                    ]
                },
                "uniqueItems": true
            }
        },
        "required": [
            "evidence_id", "source_url", "source_title", "evidence_excerpt",
            "source_kind", "supports"
        ]
    })
}

pub fn hierarchy_adjudication_response_schema() -> Value {
    let entity = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "action": {
                "type": "string",
                "enum": ["match_existing", "propose_new", "not_applicable", "unresolved"]
            },
            "existing_catalog_id": {"type": ["integer", "null"]},
            "display_name": {"type": ["string", "null"]},
            "authoritative_designator": {"type": ["string", "null"]},
            "evidence_ids": {"type": "array", "items": {"type": "string"}},
            "rationale": {"type": "string"}
        },
        "required": [
            "action", "existing_catalog_id", "display_name",
            "authoritative_designator", "evidence_ids", "rationale"
        ]
    });
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "confidence": confidence_schema(),
            "make": entity.clone(),
            "family": entity.clone(),
            "designation": entity.clone(),
            "generation": entity.clone(),
            "package": entity,
            "material_distinctions": {"type": "array", "items": {"type": "string"}},
            "unresolved_questions": {"type": "array", "items": {"type": "string"}},
            "rationale": {"type": "string"}
        },
        "required": [
            "confidence", "make", "family", "designation", "generation", "package",
            "material_distinctions", "unresolved_questions", "rationale"
        ]
    })
}

pub fn hierarchy_verification_response_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "verdict": {"type": "string", "enum": ["confirm", "reject", "ambiguous"]},
            "confidence": confidence_schema(),
            "verified_evidence_ids": {"type": "array", "items": {"type": "string"}},
            "differentiation_checks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "compared_labels": {"type": "array", "items": {"type": "string"}},
                        "conclusion": {"type": "string"},
                        "evidence_ids": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["compared_labels", "conclusion", "evidence_ids"]
                }
            },
            "errors": {"type": "array", "items": {"type": "string"}},
            "rationale": {"type": "string"}
        },
        "required": [
            "verdict", "confidence", "verified_evidence_ids",
            "differentiation_checks", "errors", "rationale"
        ]
    })
}

fn confidence_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["low", "medium", "high", "very_high"]
    })
}

pub fn search_aircraft_catalog_function_declaration() -> Value {
    json!({
        "type": "function",
        "name": "search_aircraft_catalog",
        "description": "Search the live approved aircraft catalog for collision candidates. This is retrieval only and never proves that two identities are the same. Call it before resolving any aircraft hierarchy.",
        "parameters": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "observed_make": {"type": "string"},
                "observed_family": {"type": "string"},
                "observed_designation": {"type": "string"},
                "observed_generation": {"type": ["string", "null"]},
                "observed_package": {"type": ["string", "null"]},
                "model_year": {"type": "integer"}
            },
            "required": [
                "observed_make", "observed_family", "observed_designation",
                "observed_generation", "observed_package", "model_year"
            ]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::catalog::{EvidenceClaimKind, EvidenceSourceKind};

    fn claim(id: &str) -> EvidenceClaimProposal {
        EvidenceClaimProposal {
            evidence_id: id.to_string(),
            source_url: format!("https://manufacturer.example/{id}"),
            source_title: "Official model specification".to_string(),
            evidence_excerpt: "Official model identity and applicability statement.".to_string(),
            source_kind: EvidenceSourceKind::Manufacturer,
            supports: [EvidenceClaimKind::HierarchyIdentity].into_iter().collect(),
        }
    }

    fn entity(
        action: EntityResolutionAction,
        id: Option<i64>,
        name: Option<&str>,
    ) -> CatalogEntityDecision {
        CatalogEntityDecision {
            action,
            existing_catalog_id: id,
            display_name: name.map(str::to_string),
            authoritative_designator: name.map(str::to_string),
            evidence_ids: vec!["identity".to_string()],
            rationale: "supported by primary identity evidence".to_string(),
        }
    }

    #[test]
    fn evidence_requires_an_observed_search_and_cited_urls() {
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "Cessna 182T".to_string(),
            claims: vec![claim("identity")],
            contradictions: vec![],
            unresolved_questions: vec![],
        };
        let error =
            validate_identity_evidence_research(&research, &GroundingAudit::default()).unwrap_err();
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "google_search_not_observed"));
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "uncited_evidence_url"));
    }

    #[test]
    fn n_registered_identity_cannot_label_a_non_faa_host_as_regulator_authority() {
        let mut regulator_claim = claim("identity");
        regulator_claim.source_url = "https://regulator.example/identity".to_string();
        regulator_claim.source_kind = EvidenceSourceKind::Regulator;
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "Cessna 182T".to_string(),
            claims: vec![regulator_claim],
            contradictions: vec![],
            unresolved_questions: vec![],
        };
        let grounding = GroundingAudit {
            google_search_call_count: 1,
            url_context_call_count: 1,
            citation_urls: ["https://regulator.example/identity".to_string()]
                .into_iter()
                .collect(),
        };

        let error = validate_identity_evidence_research(&research, &grounding)
            .expect_err("FAA identity authority must come from an FAA host");

        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "non_faa_regulator_source"));
    }

    #[test]
    fn match_existing_must_come_from_live_catalog_results() {
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "Cessna 182T".to_string(),
            claims: vec![claim("identity")],
            contradictions: vec![],
            unresolved_questions: vec![],
        };
        let grounding = GroundingAudit {
            google_search_call_count: 1,
            citation_urls: ["https://manufacturer.example/identity".to_string()]
                .into_iter()
                .collect(),
            ..GroundingAudit::default()
        };
        let adjudication = AircraftHierarchyAdjudication {
            confidence: CurationConfidence::VeryHigh,
            make: entity(
                EntityResolutionAction::MatchExisting,
                Some(999),
                Some("Cessna"),
            ),
            family: entity(EntityResolutionAction::ProposeNew, None, Some("182")),
            designation: entity(EntityResolutionAction::ProposeNew, None, Some("182T")),
            generation: entity(EntityResolutionAction::NotApplicable, None, None),
            package: entity(EntityResolutionAction::NotApplicable, None, None),
            material_distinctions: vec!["182T differs from T182T".to_string()],
            unresolved_questions: vec![],
            rationale: "primary sources agree".to_string(),
        };
        let verification = AircraftHierarchyVerification {
            verdict: VerificationVerdict::Confirm,
            confidence: CurationConfidence::VeryHigh,
            verified_evidence_ids: vec!["identity".to_string()],
            differentiation_checks: vec![],
            errors: vec![],
            rationale: "confirmed".to_string(),
        };
        let error = build_reviewable_aircraft_hierarchy(
            &research,
            &grounding,
            adjudication,
            &CatalogCandidateRegistry::default(),
            1,
            verification,
            &grounding,
        )
        .unwrap_err();
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "catalog_id_not_retrieved"));
    }

    #[test]
    fn unresolved_optional_dimension_blocks_reviewability() {
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "Cirrus SR22 G6".to_string(),
            claims: vec![claim("identity")],
            contradictions: vec![],
            unresolved_questions: vec![],
        };
        let grounding = GroundingAudit {
            google_search_call_count: 1,
            citation_urls: ["https://manufacturer.example/identity".to_string()]
                .into_iter()
                .collect(),
            ..GroundingAudit::default()
        };
        let adjudication = AircraftHierarchyAdjudication {
            confidence: CurationConfidence::VeryHigh,
            make: entity(EntityResolutionAction::ProposeNew, None, Some("Cirrus")),
            family: entity(EntityResolutionAction::ProposeNew, None, Some("SR22")),
            designation: entity(EntityResolutionAction::ProposeNew, None, Some("SR22")),
            generation: entity(EntityResolutionAction::Unresolved, None, None),
            package: entity(EntityResolutionAction::NotApplicable, None, None),
            material_distinctions: vec![],
            unresolved_questions: vec!["whether G6 applies".to_string()],
            rationale: "generation is unclear".to_string(),
        };
        let verification = AircraftHierarchyVerification {
            verdict: VerificationVerdict::Confirm,
            confidence: CurationConfidence::VeryHigh,
            verified_evidence_ids: vec!["identity".to_string()],
            differentiation_checks: vec![],
            errors: vec![],
            rationale: "confirmed other fields".to_string(),
        };
        let error = build_reviewable_aircraft_hierarchy(
            &research,
            &grounding,
            adjudication,
            &CatalogCandidateRegistry::default(),
            1,
            verification,
            &grounding,
        )
        .unwrap_err();
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "unresolved_hierarchy_dimension"));
    }
}
