//! Grounded staging contract for exact-year factory reference profiles.
//!
//! Listings select which identities and model years need research. They are
//! never evidence of standard factory equipment or new-aircraft price.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::aircraft::catalog::{
    EvidenceClaimKind, EvidenceClaimProposal, ValidationIssue, MAX_AIRCRAFT_MODEL_YEAR,
    MIN_AIRCRAFT_MODEL_YEAR,
};
use crate::aircraft::curation::{CurationConfidence, GroundingAudit};
use crate::aircraft::reference::{AircraftComponentKind, FactoryInclusion};

pub const REFERENCE_PROFILE_PROMPT_VERSION: &str = "aircraft-reference-profile-v1";
pub const REFERENCE_PROFILE_SCHEMA_VERSION: &str = "aircraft-reference-profile-schema-v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceProfileResearchTarget {
    pub make: String,
    pub model_family: String,
    pub designation: String,
    pub generation: Option<String>,
    pub package: Option<String>,
    pub model_year: i32,
    pub market_code: String,
    pub serial_number: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchedPriceBasis {
    FullStandardConfiguration,
    BaseAircraftOnly,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResearchedReferencePrice {
    pub amount_usd: i64,
    pub price_reference_year: i32,
    pub basis: ResearchedPriceBasis,
    pub direct_exact_model_year: bool,
    pub evidence_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResearchedFactoryComponent {
    pub research_key: String,
    pub kind: AircraftComponentKind,
    pub manufacturer: String,
    pub model: String,
    pub authoritative_identifier_kind: Option<String>,
    pub authoritative_identifier: Option<String>,
    pub quantity: u32,
    pub inclusion: FactoryInclusion,
    pub evidence_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResearchedFactoryFeature {
    pub feature_key: String,
    pub display_name: String,
    pub value: String,
    pub unit: Option<String>,
    pub evidence_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceConfigurationEvidenceResearch {
    pub target: ReferenceProfileResearchTarget,
    pub evidence: Vec<EvidenceClaimProposal>,
    pub reference_price: Option<ResearchedReferencePrice>,
    pub components: Vec<ResearchedFactoryComponent>,
    pub features: Vec<ResearchedFactoryFeature>,
    #[serde(default)]
    pub contradictions: Vec<String>,
    #[serde(default)]
    pub unresolved_questions: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentCatalogAction {
    MatchExisting,
    ProposeNew,
    RejectObservation,
    Unresolved,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ComponentCatalogDecision {
    pub research_key: String,
    pub kind: AircraftComponentKind,
    pub action: ComponentCatalogAction,
    pub existing_catalog_id: Option<i64>,
    pub canonical_manufacturer: Option<String>,
    pub canonical_model: Option<String>,
    pub authoritative_identifier_kind: Option<String>,
    pub authoritative_identifier: Option<String>,
    pub evidence_ids: Vec<String>,
    pub rationale: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceProfileAdjudication {
    pub confidence: CurationConfidence,
    pub target_is_exact: bool,
    pub reference_price_is_usable: bool,
    pub component_decisions: Vec<ComponentCatalogDecision>,
    #[serde(default)]
    pub unresolved_questions: Vec<String>,
    pub rationale: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileVerificationVerdict {
    Confirm,
    Reject,
    Ambiguous,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceProfileVerification {
    pub verdict: ProfileVerificationVerdict,
    pub confidence: CurationConfidence,
    pub exact_year_price_confirmed: bool,
    pub applicability_confirmed: bool,
    pub standard_configuration_confirmed: bool,
    pub no_listing_evidence_used_as_factory_fact: bool,
    #[serde(default)]
    pub verified_evidence_ids: Vec<String>,
    #[serde(default)]
    pub errors: Vec<String>,
    pub rationale: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ComponentCandidateRegistry {
    pub ids_by_kind: BTreeMap<AircraftComponentKind, BTreeSet<i64>>,
}

impl ComponentCandidateRegistry {
    pub fn insert(&mut self, kind: AircraftComponentKind, id: i64) {
        self.ids_by_kind.entry(kind).or_default().insert(id);
    }

    fn contains(&self, kind: AircraftComponentKind, id: i64) -> bool {
        self.ids_by_kind
            .get(&kind)
            .is_some_and(|ids| ids.contains(&id))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileProposalStatus {
    Reviewable,
    NeedsComponentCuration,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReferenceProfileEvaluation {
    pub status: ProfileProposalStatus,
    pub issues: Vec<ValidationIssue>,
}

pub fn evaluate_reference_profile_proposal(
    research: &ReferenceConfigurationEvidenceResearch,
    research_grounding: &GroundingAudit,
    adjudication: &ReferenceProfileAdjudication,
    catalog_function_call_count: usize,
    candidates: &ComponentCandidateRegistry,
    verification: &ReferenceProfileVerification,
    verification_grounding: &GroundingAudit,
) -> ReferenceProfileEvaluation {
    let mut issues = Vec::new();
    validate_research(research, research_grounding, &mut issues);
    if catalog_function_call_count == 0 {
        issues.push(issue(
            "component_catalog_function_not_called",
            "the adjudicator did not query the live approved component catalog",
        ));
    }
    if adjudication.confidence != CurationConfidence::VeryHigh {
        issues.push(issue(
            "profile_adjudication_confidence_too_low",
            "reference profile adjudication must be very_high confidence",
        ));
    }
    if !adjudication.target_is_exact {
        issues.push(issue(
            "profile_target_not_exact",
            "maker/family/designation/generation/package/year/market target is not exact",
        ));
    }
    if !adjudication.reference_price_is_usable {
        issues.push(issue(
            "reference_price_not_usable",
            "the adjudicator did not confirm an exact-year full-configuration price",
        ));
    }

    let researched_by_key = research
        .components
        .iter()
        .map(|component| (component.research_key.as_str(), component))
        .collect::<BTreeMap<_, _>>();
    let mut seen_keys = BTreeSet::new();
    let mut needs_component_curation = false;
    for decision in &adjudication.component_decisions {
        if !seen_keys.insert(decision.research_key.as_str()) {
            issues.push(issue(
                "duplicate_component_decision",
                format!(
                    "component {} was adjudicated more than once",
                    decision.research_key
                ),
            ));
        }
        let Some(researched) = researched_by_key.get(decision.research_key.as_str()) else {
            issues.push(issue(
                "unknown_component_research_key",
                format!(
                    "component decision {} has no research input",
                    decision.research_key
                ),
            ));
            continue;
        };
        if researched.kind != decision.kind {
            issues.push(issue(
                "component_kind_changed",
                format!(
                    "component {} changed kind during adjudication",
                    decision.research_key
                ),
            ));
        }
        match decision.action {
            ComponentCatalogAction::MatchExisting => match decision.existing_catalog_id {
                Some(id) if candidates.contains(decision.kind, id) => {}
                Some(id) => issues.push(issue(
                    "component_catalog_id_not_retrieved",
                    format!("component catalog id {id} was not returned by the live lookup"),
                )),
                None => issues.push(issue(
                    "component_match_missing_id",
                    format!(
                        "component {} match has no catalog id",
                        decision.research_key
                    ),
                )),
            },
            ComponentCatalogAction::ProposeNew => {
                needs_component_curation = true;
                if decision
                    .authoritative_identifier
                    .as_deref()
                    .is_none_or(str::is_empty)
                {
                    issues.push(issue(
                        "new_component_missing_identifier",
                        format!(
                            "new component {} lacks an authoritative identifier",
                            decision.research_key
                        ),
                    ));
                }
            }
            ComponentCatalogAction::RejectObservation | ComponentCatalogAction::Unresolved => {
                issues.push(issue(
                    "component_unresolved",
                    format!("component {} is not resolved", decision.research_key),
                ));
            }
        }
    }
    for key in researched_by_key.keys() {
        if !seen_keys.contains(key) {
            issues.push(issue(
                "component_not_adjudicated",
                format!("researched component {key} has no catalog decision"),
            ));
        }
    }

    if verification_grounding.google_search_call_count == 0
        || verification_grounding.url_context_call_count == 0
    {
        issues.push(issue(
            "profile_verifier_not_grounded",
            "the independent verifier must successfully use Search and URL Context",
        ));
    }
    if verification.verdict != ProfileVerificationVerdict::Confirm
        || verification.confidence != CurationConfidence::VeryHigh
        || !verification.exact_year_price_confirmed
        || !verification.applicability_confirmed
        || !verification.standard_configuration_confirmed
        || !verification.no_listing_evidence_used_as_factory_fact
    {
        issues.push(issue(
            "profile_independent_verification_failed",
            "the fresh verifier did not confirm every publication gate",
        ));
    }

    let status = if !issues.is_empty() {
        ProfileProposalStatus::Blocked
    } else if needs_component_curation {
        ProfileProposalStatus::NeedsComponentCuration
    } else {
        ProfileProposalStatus::Reviewable
    };
    ReferenceProfileEvaluation { status, issues }
}

fn validate_research(
    research: &ReferenceConfigurationEvidenceResearch,
    grounding: &GroundingAudit,
    issues: &mut Vec<ValidationIssue>,
) {
    if grounding.google_search_call_count == 0 || grounding.url_context_call_count == 0 {
        issues.push(issue(
            "profile_research_not_grounded",
            "profile research must successfully use Search and URL Context",
        ));
    }
    if !(MIN_AIRCRAFT_MODEL_YEAR..=MAX_AIRCRAFT_MODEL_YEAR).contains(&research.target.model_year) {
        issues.push(issue(
            "invalid_profile_model_year",
            "profile model year is outside the supported range",
        ));
    }
    if research.target.market_code.trim().is_empty() {
        issues.push(issue(
            "missing_profile_market",
            "reference profile market must be explicit",
        ));
    }
    let evidence_by_id = research
        .evidence
        .iter()
        .map(|evidence| (evidence.evidence_id.as_str(), evidence))
        .collect::<BTreeMap<_, _>>();
    for evidence in &research.evidence {
        if !grounding.citation_urls.iter().any(|url| {
            url.trim().trim_end_matches('/') == evidence.source_url.trim().trim_end_matches('/')
        }) {
            issues.push(issue(
                "profile_evidence_url_uncited",
                format!("evidence {} uses an uncited URL", evidence.evidence_id),
            ));
        }
    }
    if !research.evidence.iter().any(|evidence| {
        evidence.source_kind.is_primary()
            && evidence
                .supports
                .contains(&EvidenceClaimKind::FactoryConfiguration)
    }) {
        issues.push(issue(
            "missing_primary_factory_configuration_evidence",
            "standard configuration requires primary-source evidence",
        ));
    }
    if !research.evidence.iter().any(|evidence| {
        evidence.source_kind.is_primary()
            && evidence
                .supports
                .contains(&EvidenceClaimKind::ProductionApplicability)
    }) {
        issues.push(issue(
            "missing_primary_profile_applicability_evidence",
            "exact year/serial/market applicability requires primary evidence",
        ));
    }
    let Some(price) = &research.reference_price else {
        issues.push(issue(
            "missing_exact_year_reference_price",
            "no direct exact-model-year reference price was found",
        ));
        return;
    };
    if price.amount_usd <= 0
        || price.price_reference_year != research.target.model_year
        || !price.direct_exact_model_year
        || price.basis != ResearchedPriceBasis::FullStandardConfiguration
    {
        issues.push(issue(
            "invalid_exact_year_reference_price",
            "price must directly state this model year's full standard configuration",
        ));
    }
    require_claim_kind(
        &price.evidence_ids,
        EvidenceClaimKind::ReferencePrice,
        &evidence_by_id,
        "reference price",
        issues,
    );
    for component in &research.components {
        if component.research_key.trim().is_empty()
            || component.manufacturer.trim().is_empty()
            || component.model.trim().is_empty()
            || component.quantity == 0
        {
            issues.push(issue(
                "invalid_researched_component",
                "factory component requires a key, make, model, and positive quantity",
            ));
        }
        require_claim_kind(
            &component.evidence_ids,
            EvidenceClaimKind::FactoryConfiguration,
            &evidence_by_id,
            &format!("component {}", component.research_key),
            issues,
        );
    }
}

fn require_claim_kind(
    ids: &[String],
    kind: EvidenceClaimKind,
    evidence: &BTreeMap<&str, &EvidenceClaimProposal>,
    subject: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    if ids.is_empty()
        || !ids.iter().any(|id| {
            evidence.get(id.as_str()).is_some_and(|claim| {
                claim.source_kind.is_primary() && claim.supports.contains(&kind)
            })
        })
    {
        issues.push(issue(
            "missing_primary_subject_evidence",
            format!("{subject} lacks primary {kind:?} evidence"),
        ));
    }
}

fn issue(code: impl Into<String>, message: impl Into<String>) -> ValidationIssue {
    ValidationIssue::new(code, message)
}

pub fn build_reference_profile_research_prompt(target: &ReferenceProfileResearchTarget) -> String {
    format!(
        r#"Research the exact standard factory reference configuration for the aircraft target below.

The target came from retained sale listings only to identify which configuration and model year need research. A sale listing is never evidence of standard equipment, package composition, applicability, or new-aircraft price. Execute Google Search on this first attempt and use URL Context on the exact primary documents selected.

Prefer the model-year manufacturer price/specification sheet, order guide, brochure, approved flight manual, type-certificate data, and manufacturer service publications. Return exact model-year, market, and serial applicability. Keep certified designation, generation, and factory package/tier separate. If serial or market applicability cannot be proved, report it unresolved.

For every standard component return the physical product identity, quantity, inclusion basis, authoritative model/part/SKU identifier when available, and evidence IDs. Do not split a multifunction avionics unit by capability and do not treat a capability label (AHRS, ADF, ELT, GPS, display) as a product identity. A suite may be recorded separately from its member hardware only when primary documentation defines both.

Return a new-aircraft price only when a primary source directly states the price for this exact model year and clearly establishes that the amount covers the full standard configuration. Do not interpolate, inflate, use a current replacement price, use a used listing price, or subtract retail avionics prices from historical MSRP. If no qualifying price exists, return null.

Target:
{}"#,
        serde_json::to_string_pretty(target).expect("reference target serializes")
    )
}

pub fn build_reference_profile_adjudication_prompt(
    research: &ReferenceConfigurationEvidenceResearch,
) -> String {
    format!(
        r#"Adjudicate the researched factory reference profile below against the current approved component catalog.

You must call search_aircraft_component_catalog before deciding. Only IDs returned by that live function may be matched. Similar labels are collision candidates, not identity proof. Match an existing unit only when make, physical model identity, and authoritative identifier/evidence agree. Propose a new component only when primary product documentation supplies a stable model/part/SKU-like identifier and proves every returned candidate is different. Otherwise return unresolved. Never create a component from a capability or generic equipment category.

Confirm reference_price_is_usable only for a direct exact-model-year primary-source price covering the full standard configuration. Confidence can be very_high only if target identity/year/market/serial applicability and all factory facts are proved.

Research bundle:
{}"#,
        serde_json::to_string_pretty(research).expect("reference research serializes")
    )
}

pub fn build_reference_profile_verification_prompt(
    research: &ReferenceConfigurationEvidenceResearch,
    adjudication: &ReferenceProfileAdjudication,
) -> String {
    let payload = json!({"research": research, "adjudication": adjudication});
    format!(
        r#"Freshly and independently verify this proposed aircraft reference profile. Execute Google Search and use URL Context on every primary source needed. Do not defer to the previous model.

Confirm only if: the hierarchy/package is exact; year, serial, and market applicability are proved; every standard component is factory evidence rather than installed-equipment evidence from a sale listing; component identities are physical products rather than capabilities; and the stated price is direct for this exact model year and covers the full standard configuration. Otherwise reject or return ambiguous. Do not repair the proposal silently.

Payload:
{}"#,
        serde_json::to_string_pretty(&payload).expect("profile verification payload serializes")
    )
}

pub fn reference_profile_research_response_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "target": target_schema(),
            "evidence": {"type": "array", "items": evidence_schema()},
            "reference_price": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "amount_usd": {"type": "integer"},
                            "price_reference_year": {"type": "integer"},
                            "basis": {"type": "string", "enum": ["full_standard_configuration", "base_aircraft_only", "unknown"]},
                            "direct_exact_model_year": {"type": "boolean"},
                            "evidence_ids": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["amount_usd", "price_reference_year", "basis", "direct_exact_model_year", "evidence_ids"]
                    }
                ]
            },
            "components": {"type": "array", "items": researched_component_schema()},
            "features": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "feature_key": {"type": "string"},
                        "display_name": {"type": "string"},
                        "value": {"type": "string"},
                        "unit": {"type": ["string", "null"]},
                        "evidence_ids": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["feature_key", "display_name", "value", "unit", "evidence_ids"]
                }
            },
            "contradictions": {"type": "array", "items": {"type": "string"}},
            "unresolved_questions": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["target", "evidence", "reference_price", "components", "features", "contradictions", "unresolved_questions"]
    })
}

fn target_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "make": {"type": "string"},
            "model_family": {"type": "string"},
            "designation": {"type": "string"},
            "generation": {"type": ["string", "null"]},
            "package": {"type": ["string", "null"]},
            "model_year": {"type": "integer"},
            "market_code": {"type": "string"},
            "serial_number": {"type": ["string", "null"]}
        },
        "required": ["make", "model_family", "designation", "generation", "package", "model_year", "market_code", "serial_number"]
    })
}

fn evidence_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "evidence_id": {"type": "string"},
            "source_url": {"type": "string"},
            "source_title": {"type": "string"},
            "evidence_excerpt": {"type": "string"},
            "source_kind": {"type": "string", "enum": ["manufacturer", "regulator", "type_certificate", "approved_flight_manual", "manufacturer_service_publication", "recognized_secondary", "marketplace_listing"]},
            "supports": {"type": "array", "items": {"type": "string", "enum": ["hierarchy_identity", "production_applicability", "market_applicability", "factory_configuration", "reference_price", "component_identity", "material_feature"]}}
        },
        "required": ["evidence_id", "source_url", "source_title", "evidence_excerpt", "source_kind", "supports"]
    })
}

fn researched_component_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "research_key": {"type": "string"},
            "kind": {"type": "string", "enum": ["avionics", "engine", "propeller", "airframe_feature"]},
            "manufacturer": {"type": "string"},
            "model": {"type": "string"},
            "authoritative_identifier_kind": {"type": ["string", "null"]},
            "authoritative_identifier": {"type": ["string", "null"]},
            "quantity": {"type": "integer", "minimum": 1},
            "inclusion": {"type": "string", "enum": ["standard", "mandatory", "included_in_tier", "optional"]},
            "evidence_ids": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["research_key", "kind", "manufacturer", "model", "authoritative_identifier_kind", "authoritative_identifier", "quantity", "inclusion", "evidence_ids"]
    })
}

pub fn reference_profile_adjudication_response_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "confidence": {"type": "string", "enum": ["low", "medium", "high", "very_high"]},
            "target_is_exact": {"type": "boolean"},
            "reference_price_is_usable": {"type": "boolean"},
            "component_decisions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "research_key": {"type": "string"},
                        "kind": {"type": "string", "enum": ["avionics", "engine", "propeller", "airframe_feature"]},
                        "action": {"type": "string", "enum": ["match_existing", "propose_new", "reject_observation", "unresolved"]},
                        "existing_catalog_id": {"type": ["integer", "null"]},
                        "canonical_manufacturer": {"type": ["string", "null"]},
                        "canonical_model": {"type": ["string", "null"]},
                        "authoritative_identifier_kind": {"type": ["string", "null"]},
                        "authoritative_identifier": {"type": ["string", "null"]},
                        "evidence_ids": {"type": "array", "items": {"type": "string"}},
                        "rationale": {"type": "string"}
                    },
                    "required": ["research_key", "kind", "action", "existing_catalog_id", "canonical_manufacturer", "canonical_model", "authoritative_identifier_kind", "authoritative_identifier", "evidence_ids", "rationale"]
                }
            },
            "unresolved_questions": {"type": "array", "items": {"type": "string"}},
            "rationale": {"type": "string"}
        },
        "required": ["confidence", "target_is_exact", "reference_price_is_usable", "component_decisions", "unresolved_questions", "rationale"]
    })
}

pub fn reference_profile_verification_response_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "verdict": {"type": "string", "enum": ["confirm", "reject", "ambiguous"]},
            "confidence": {"type": "string", "enum": ["low", "medium", "high", "very_high"]},
            "exact_year_price_confirmed": {"type": "boolean"},
            "applicability_confirmed": {"type": "boolean"},
            "standard_configuration_confirmed": {"type": "boolean"},
            "no_listing_evidence_used_as_factory_fact": {"type": "boolean"},
            "verified_evidence_ids": {"type": "array", "items": {"type": "string"}},
            "errors": {"type": "array", "items": {"type": "string"}},
            "rationale": {"type": "string"}
        },
        "required": ["verdict", "confidence", "exact_year_price_confirmed", "applicability_confirmed", "standard_configuration_confirmed", "no_listing_evidence_used_as_factory_fact", "verified_evidence_ids", "errors", "rationale"]
    })
}

pub fn search_component_catalog_function_declaration() -> Value {
    json!({
        "type": "function",
        "name": "search_aircraft_component_catalog",
        "description": "Search the live approved avionics, engine, propeller, and aircraft-feature catalogs for identity collision candidates. Candidate similarity is not identity evidence.",
        "parameters": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "components": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "research_key": {"type": "string"},
                            "kind": {"type": "string", "enum": ["avionics", "engine", "propeller", "airframe_feature"]},
                            "manufacturer": {"type": "string"},
                            "model": {"type": "string"},
                            "authoritative_identifier": {"type": ["string", "null"]}
                        },
                        "required": ["research_key", "kind", "manufacturer", "model", "authoritative_identifier"]
                    }
                }
            },
            "required": ["components"]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::catalog::EvidenceSourceKind;

    fn primary_evidence(id: &str, kinds: &[EvidenceClaimKind]) -> EvidenceClaimProposal {
        EvidenceClaimProposal {
            evidence_id: id.to_string(),
            source_url: format!("https://manufacturer.example/{id}"),
            source_title: "Official model-year order guide".to_string(),
            evidence_excerpt: "Official exact-year configuration and price statement.".to_string(),
            source_kind: EvidenceSourceKind::Manufacturer,
            supports: kinds.iter().copied().collect(),
        }
    }

    #[test]
    fn a_used_listing_cannot_establish_factory_configuration() {
        let evidence = EvidenceClaimProposal {
            source_kind: EvidenceSourceKind::MarketplaceListing,
            ..primary_evidence("listing", &[EvidenceClaimKind::FactoryConfiguration])
        };
        let research = ReferenceConfigurationEvidenceResearch {
            target: ReferenceProfileResearchTarget {
                make: "Cessna".to_string(),
                model_family: "182".to_string(),
                designation: "182T".to_string(),
                generation: None,
                package: None,
                model_year: 2023,
                market_code: "US".to_string(),
                serial_number: None,
            },
            evidence: vec![evidence],
            reference_price: None,
            components: vec![],
            features: vec![],
            contradictions: vec![],
            unresolved_questions: vec![],
        };
        let mut issues = Vec::new();
        validate_research(&research, &GroundingAudit::default(), &mut issues);
        assert!(issues
            .iter()
            .any(|issue| { issue.code == "missing_primary_factory_configuration_evidence" }));
    }

    #[test]
    fn reference_price_must_be_direct_exact_year_and_full_configuration() {
        let evidence = vec![
            primary_evidence(
                "factory",
                &[
                    EvidenceClaimKind::FactoryConfiguration,
                    EvidenceClaimKind::ProductionApplicability,
                ],
            ),
            primary_evidence("price", &[EvidenceClaimKind::ReferencePrice]),
        ];
        let research = ReferenceConfigurationEvidenceResearch {
            target: ReferenceProfileResearchTarget {
                make: "Cessna".to_string(),
                model_family: "182".to_string(),
                designation: "182T".to_string(),
                generation: None,
                package: None,
                model_year: 2023,
                market_code: "US".to_string(),
                serial_number: None,
            },
            reference_price: Some(ResearchedReferencePrice {
                amount_usd: 600_000,
                price_reference_year: 2026,
                basis: ResearchedPriceBasis::BaseAircraftOnly,
                direct_exact_model_year: false,
                evidence_ids: vec!["price".to_string()],
            }),
            components: vec![],
            features: vec![],
            evidence,
            contradictions: vec![],
            unresolved_questions: vec![],
        };
        let grounding = GroundingAudit {
            google_search_call_count: 1,
            url_context_call_count: 1,
            citation_urls: [
                "https://manufacturer.example/factory".to_string(),
                "https://manufacturer.example/price".to_string(),
            ]
            .into_iter()
            .collect(),
        };
        let mut issues = Vec::new();
        validate_research(&research, &grounding, &mut issues);
        assert!(issues
            .iter()
            .any(|issue| issue.code == "invalid_exact_year_reference_price"));
    }
}
