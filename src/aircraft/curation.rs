//! Evidence-first aircraft hierarchy curation.
//!
//! This module contains the deterministic contract around Gemini. The model
//! may research and propose identities, but it cannot approve catalog rows.
//! Mechanical normalization is used only to retrieve candidates; evidence,
//! exact identifiers, and an independent verification pass determine whether
//! a proposal is reviewable.

pub mod application;
pub mod persistence;
pub mod profile;
pub(crate) mod regulator;
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
use crate::aircraft::curation::regulator::{
    TcdsFamilyBinding, TcdsIdentityBinding, TcdsMakeLineageEvidence,
};
use crate::aircraft::faa::{AircraftGrounding, Snapshot};
use crate::aircraft::observations::AircraftIdentityObservation;
use crate::db::{AppDb, DatabaseBackend};
use crate::gemini::curation::workflow::{SourceEvidenceProof, SourceEvidenceSpanProof};

pub const AIRCRAFT_IDENTITY_PROMPT_VERSION: &str = "aircraft-identity-v14";
pub const AIRCRAFT_IDENTITY_SCHEMA_VERSION: &str = "aircraft-identity-schema-v12";

/// Compare a catalog legal make with a holder name transcribed from an FAA
/// TCDS. FAA sources are inconsistent about a final period in legal suffixes
/// such as `Inc.`. This deliberately tolerates only surrounding whitespace,
/// ASCII case, and terminal periods; it does not remove suffixes, punctuation
/// inside the name, or otherwise turn a case-scoped lineage into a broad alias.
pub(super) fn tcds_holder_names_match(left: &str, right: &str) -> bool {
    left.trim()
        .trim_end_matches('.')
        .eq_ignore_ascii_case(right.trim().trim_end_matches('.'))
}

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
    NoSupportedSelection,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaaMakeRelationshipAction {
    /// The canonical make label is literally the FAA-reported legal make.
    ExactCanonicalLabel,
    /// An already approved, market/year-scoped alias maps the FAA legal make
    /// to the selected canonical make.
    MatchApprovedAlias,
    /// Primary web evidence supports proposing a new market/year-scoped alias.
    ProposeAlias,
    /// Exact FAA registry and current FAA TCDS evidence relate this FAA legal
    /// make to one existing type-certificate holder for this exact release,
    /// aircraft code, designation, and certified serial interval. This is an
    /// immutable case-scoped binding, never a catalog alias.
    MatchTcdsMakeLineage,
    /// The FAA legal make and selected canonical make cannot yet be related
    /// with primary evidence.
    Unresolved,
}

/// A typed decision for the common case where the FAA registry legal
/// manufacturer differs from the aircraft's marketed make.
///
/// The registry proves only `faa_manufacturer_name`. It never proves that a
/// legal entity such as `TEXTRON AVIATION INC` is interchangeable with a brand
/// such as `Cessna`; that relationship requires separate primary web evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaaMakeRelationshipDecision {
    pub action: FaaMakeRelationshipAction,
    pub faa_manufacturer_name: String,
    pub canonical_make_name: String,
    pub existing_alias_id: Option<i64>,
    pub valid_from_model_year: Option<i64>,
    pub valid_to_model_year: Option<i64>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub applicability_evidence_ids: Vec<String>,
    pub rationale: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FamilyLabelRelationshipAction {
    /// The retained family/model label literally equals the canonical family.
    ExactCanonicalLabel,
    /// The exact retained model field is a composition of the numeric series
    /// stem from the serial-bound FAA designation and the exact canonical
    /// family. Direct manufacturer evidence co-names those same two
    /// components. This is a case-bound comparison, never a catalog alias.
    MatchManufacturerSeriesFamily,
    /// An approved, market/year-scoped family alias maps the retained label to
    /// the selected canonical family.
    MatchApprovedAlias,
    /// Direct primary evidence supports proposing a new scoped family alias.
    ProposeAlias,
    /// A current FAA type-certificate data sheet binds the exact registry
    /// designation to the named family, and its serial-eligibility row
    /// contains the listing's FAA-matched serial. The retained listing label
    /// remains audit input rather than being represented as a TCDS heading.
    /// This is a case-bound regulator relationship, not a catalog alias or
    /// model-year interval.
    MatchFaaTypeCertificateFamily,
    /// The retained label cannot yet be related to the canonical family.
    Unresolved,
}

/// A typed, evidence-bound relationship from the exact retained listing
/// model/family label to the selected canonical aircraft family.
///
/// This decision is deliberately separate from family identity: a canonical
/// family such as `Skylane` does not mechanically prove that an observed
/// legacy label such as `182` belongs to it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FamilyLabelRelationshipDecision {
    pub action: FamilyLabelRelationshipAction,
    pub observed_family_label: String,
    pub canonical_family_name: String,
    pub existing_alias_id: Option<i64>,
    pub valid_from_model_year: Option<i64>,
    pub valid_to_model_year: Option<i64>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub applicability_evidence_ids: Vec<String>,
    pub rationale: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AircraftIdentityEvidenceResearch {
    pub subject_summary: String,
    pub claims: Vec<EvidenceClaimProposal>,
    pub family_candidates: Vec<HierarchyCandidate>,
    pub generation_candidates: Vec<HierarchyCandidate>,
    pub package_candidates: Vec<HierarchyCandidate>,
    #[serde(default)]
    pub contradictions: Vec<String>,
    #[serde(default)]
    pub unresolved_questions: Vec<ResearchUnresolvedQuestion>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchUnresolvedScope {
    FaaMakeBrandRelationship,
    FamilyIdentity,
    FamilyLabelRelationship,
    FamilyProductionApplicability,
    Designation,
    Generation,
    Package,
    SourceIntegrity,
    Other,
}

pub const ALL_RESEARCH_UNRESOLVED_SCOPES: &[ResearchUnresolvedScope] = &[
    ResearchUnresolvedScope::FaaMakeBrandRelationship,
    ResearchUnresolvedScope::FamilyIdentity,
    ResearchUnresolvedScope::FamilyLabelRelationship,
    ResearchUnresolvedScope::FamilyProductionApplicability,
    ResearchUnresolvedScope::Designation,
    ResearchUnresolvedScope::Generation,
    ResearchUnresolvedScope::Package,
    ResearchUnresolvedScope::SourceIntegrity,
    ResearchUnresolvedScope::Other,
];

impl ResearchUnresolvedScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FaaMakeBrandRelationship => "faa_make_brand_relationship",
            Self::FamilyIdentity => "family_identity",
            Self::FamilyLabelRelationship => "family_label_relationship",
            Self::FamilyProductionApplicability => "family_production_applicability",
            Self::Designation => "designation",
            Self::Generation => "generation",
            Self::Package => "package",
            Self::SourceIntegrity => "source_integrity",
            Self::Other => "other",
        }
    }

    /// Whether Gemini may emit this scope in a structured research response.
    ///
    /// `Other` remains deserializable so historical or non-schema-conforming
    /// payloads still fail closed under the ordinary unresolved-question
    /// validator. It is deliberately not exposed to generation: every
    /// aircraft-identity gap has a typed scope, source/citation/provenance gaps
    /// use `source_integrity`, and source or FAA/listing disagreements belong
    /// in `contradictions`. An open-ended generated scope otherwise lets
    /// equipment, configuration, maintenance, or value research poison an
    /// aircraft-identity admission.
    const fn is_model_emittable(self) -> bool {
        !matches!(self, Self::Other)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResearchUnresolvedQuestion {
    pub scope: ResearchUnresolvedScope,
    pub question: String,
}

/// A positively identified hierarchy label from the research pass.
///
/// For optional dimensions, an empty candidate list is not evidence that the
/// real-world dimension does not exist. It is one input to the deterministic
/// decision that this exact case has no safely selectable catalog value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HierarchyCandidate {
    pub label: String,
    pub evidence_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AircraftHierarchyAdjudication {
    pub confidence: CurationConfidence,
    pub make: CatalogEntityDecision,
    pub faa_make_relationship: FaaMakeRelationshipDecision,
    pub family: CatalogEntityDecision,
    pub family_label_relationship: FamilyLabelRelationshipDecision,
    pub designation: CatalogEntityDecision,
    pub generation: CatalogEntityDecision,
    pub package: CatalogEntityDecision,
    #[serde(default)]
    pub material_distinctions: Vec<String>,
    #[serde(default)]
    pub unresolved_questions: Vec<String>,
    pub rationale: String,
}

const SERVER_FAA_REGISTRY_EVIDENCE_ID_PREFIX: &str = "server_faa_registry.";
const SERVER_FAA_DRS_EVIDENCE_ID_PREFIX: &str = "server_faa_drs.";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) enum TcdsSelectionBasis {
    RegistryReference,
    DrsUniqueCurrentExactModel,
    OperatorValidatedExactModelSerial,
}

impl TcdsSelectionBasis {
    fn as_str(self) -> &'static str {
        match self {
            Self::RegistryReference => "registry_reference",
            Self::DrsUniqueCurrentExactModel => "drs_unique_current_exact_model",
            Self::OperatorValidatedExactModelSerial => "operator_validated_exact_model_serial",
        }
    }
}

/// Exact FAA identity claims created by the server from the immutable imported
/// registry projection. These claims are not model output and therefore do not
/// need a Gemini URL citation. Validation accepts the citation exception only
/// when the complete claim equals one in this case-bound registry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ServerFaaIdentityEvidence {
    case_token: String,
    case_snapshot: Snapshot,
    faa_manufacturer_name: String,
    faa_model_designation: String,
    make_claim_id: String,
    designation_claim_id: String,
    listing_model_years: BTreeSet<i64>,
    faa_years_manufactured: BTreeSet<i64>,
    observation_bindings: Vec<ServerFaaObservationBinding>,
    tcds_selection_basis: Option<TcdsSelectionBasis>,
    tcds_identity_binding: Option<TcdsIdentityBinding>,
    tcds_family_binding: Option<TcdsFamilyBinding>,
    tcds_make_lineage_evidence: Option<TcdsMakeLineageEvidence>,
    claims: Vec<EvidenceClaimProposal>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TcdsIdentityClaimIds {
    faa_model_heading: String,
    serial_eligibility: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TcdsMakeLineageClaimIds {
    faa_model_heading: String,
    serial_eligibility: String,
    holder_transfer: String,
    manufacturer_serial_eligibility: Option<String>,
}

impl TcdsMakeLineageClaimIds {
    fn identity(&self) -> BTreeSet<&str> {
        [
            self.faa_model_heading.as_str(),
            self.holder_transfer.as_str(),
        ]
        .into_iter()
        .collect()
    }

    fn applicability(&self) -> BTreeSet<&str> {
        std::iter::once(self.serial_eligibility.as_str())
            .chain(self.manufacturer_serial_eligibility.as_deref())
            .collect()
    }

    fn all(&self) -> BTreeSet<&str> {
        self.identity()
            .into_iter()
            .chain(self.applicability())
            .collect()
    }
}

impl TcdsIdentityClaimIds {
    fn all(&self) -> BTreeSet<&str> {
        [
            self.faa_model_heading.as_str(),
            self.serial_eligibility.as_str(),
        ]
        .into_iter()
        .collect()
    }

    fn hierarchy(&self) -> Vec<String> {
        vec![self.faa_model_heading.clone()]
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ServerFaaObservationBinding {
    listing_id: i64,
    observation_sha256: String,
    observed_make: String,
    observed_model: String,
    observed_variant: String,
    listing_model_year: i64,
    grounding: AircraftGrounding,
}

impl ServerFaaObservationBinding {
    pub(crate) fn new(
        listing_id: i64,
        observation_sha256: impl Into<String>,
        observed_make: impl Into<String>,
        observed_model: impl Into<String>,
        observed_variant: impl Into<String>,
        listing_model_year: i64,
        grounding: AircraftGrounding,
    ) -> Self {
        Self {
            listing_id,
            observation_sha256: observation_sha256.into(),
            observed_make: observed_make.into(),
            observed_model: observed_model.into(),
            observed_variant: observed_variant.into(),
            listing_model_year,
            grounding,
        }
    }
}

impl ServerFaaIdentityEvidence {
    pub(crate) fn new(
        case_token: impl Into<String>,
        case_snapshot: Snapshot,
        mut observation_bindings: Vec<ServerFaaObservationBinding>,
        faa_manufacturer_name: impl Into<String>,
        faa_model_designation: impl Into<String>,
    ) -> Result<Self, String> {
        let case_token = case_token.into();
        let faa_manufacturer_name = faa_manufacturer_name.into().trim().to_string();
        let faa_model_designation = faa_model_designation.into().trim().to_string();
        observation_bindings.sort_by(|left, right| {
            left.listing_id
                .cmp(&right.listing_id)
                .then_with(|| left.observation_sha256.cmp(&right.observation_sha256))
        });
        let snapshot_ids = observation_bindings
            .iter()
            .map(|binding| binding.grounding.snapshot.id)
            .collect::<BTreeSet<_>>();
        let source_record_sha256s = observation_bindings
            .iter()
            .map(|binding| binding.grounding.source_record_sha256.clone())
            .collect::<BTreeSet<_>>();
        let aircraft_codes = observation_bindings
            .iter()
            .map(|binding| binding.grounding.aircraft_code.trim().to_string())
            .collect::<BTreeSet<_>>();
        let n_numbers = observation_bindings
            .iter()
            .map(|binding| binding.grounding.n_number.as_str())
            .collect::<BTreeSet<_>>();
        let listing_model_years = observation_bindings
            .iter()
            .map(|binding| binding.listing_model_year)
            .collect::<BTreeSet<_>>();
        let faa_years_manufactured = observation_bindings
            .iter()
            .filter_map(|binding| binding.grounding.year_manufactured.map(i64::from))
            .collect::<BTreeSet<_>>();
        let same_release = observation_bindings.iter().all(|binding| {
            binding.grounding.snapshot.snapshot_date == case_snapshot.snapshot_date
                && binding.grounding.snapshot.source_url == case_snapshot.source_url
                && binding.grounding.snapshot.archive_sha256 == case_snapshot.archive_sha256
                && binding.grounding.snapshot.source_manifest_sha256
                    == case_snapshot.source_manifest_sha256
        });
        let unique_observations = observation_bindings
            .iter()
            .map(|binding| (binding.listing_id, binding.observation_sha256.as_str()))
            .collect::<BTreeSet<_>>()
            .len()
            == observation_bindings.len();
        let every_binding_has_exact_identity = observation_bindings.iter().all(|binding| {
            binding.grounding.aircraft.as_ref().is_some_and(|aircraft| {
                aircraft.manufacturer_name.as_deref().map(str::trim)
                    == Some(faa_manufacturer_name.as_str())
                    && aircraft.model_name.as_deref().map(str::trim)
                        == Some(faa_model_designation.as_str())
                    && aircraft.aircraft_code.trim() == binding.grounding.aircraft_code.trim()
            })
        });
        if case_token.trim().is_empty()
            || case_snapshot.id <= 0
            || case_snapshot.source_url.trim().is_empty()
            || !is_faa_source_url(&case_snapshot.source_url)
            || snapshot_ids.is_empty()
            || snapshot_ids.iter().any(|id| *id <= 0)
            || case_snapshot.snapshot_date.trim().is_empty()
            || !is_sha256(&case_snapshot.archive_sha256)
            || !is_sha256(&case_snapshot.source_manifest_sha256)
            || source_record_sha256s.is_empty()
            || source_record_sha256s.iter().any(|value| !is_sha256(value))
            || aircraft_codes.len() != 1
            || aircraft_codes.iter().any(|value| value.is_empty())
            || n_numbers.iter().any(|value| value.is_empty())
            || faa_manufacturer_name.trim().is_empty()
            || faa_model_designation.trim().is_empty()
            || listing_model_years.is_empty()
            || !same_release
            || !unique_observations
            || !snapshot_ids.contains(&case_snapshot.id)
            || !every_binding_has_exact_identity
        {
            return Err(
                "server FAA identity evidence requires an FAA-hosted source and complete digest-identified case, snapshot, record, code, make, and model provenance"
                    .to_string(),
            );
        }
        let source_record_sha256s = source_record_sha256s
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(",");
        let snapshot_ids = snapshot_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let aircraft_code = aircraft_codes
            .into_iter()
            .next()
            .expect("one FAA aircraft code checked");
        let n_numbers = n_numbers.into_iter().collect::<Vec<_>>().join(",");
        let observation_provenance = observation_bindings
            .iter()
            .map(|binding| {
                format!(
                    "{}:{}:{:?}:{:?}:{:?}:{}",
                    binding.listing_id,
                    binding.observation_sha256,
                    binding.observed_make,
                    binding.observed_model,
                    binding.observed_variant,
                    binding.listing_model_year
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let provenance = format!(
            "snapshot_ids={snapshot_ids}; snapshot_date={}; archive_sha256={}; source_manifest_sha256={}; source_record_sha256={source_record_sha256s}; n_numbers={n_numbers}; aircraft_code={aircraft_code}; observations={observation_provenance}; case_token={case_token}",
            case_snapshot.snapshot_date,
            case_snapshot.archive_sha256,
            case_snapshot.source_manifest_sha256,
        );
        let make_claim_id =
            server_faa_claim_id(&case_token, "make", &faa_manufacturer_name, &provenance);
        let designation_claim_id = server_faa_claim_id(
            &case_token,
            "designation",
            &faa_model_designation,
            &provenance,
        );
        let source_title = format!(
            "FAA releasable aircraft registry snapshot {} (server import)",
            case_snapshot.snapshot_date
        );
        let supports = [crate::aircraft::catalog::EvidenceClaimKind::HierarchyIdentity]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let claims = vec![
            EvidenceClaimProposal {
                evidence_id: make_claim_id.clone(),
                source_url: case_snapshot.source_url.clone(),
                source_title: source_title.clone(),
                evidence_excerpt: format!(
                    "Imported FAA ACFTREF reports manufacturer_name={faa_manufacturer_name:?}; {provenance}"
                ),
                source_kind: crate::aircraft::catalog::EvidenceSourceKind::Regulator,
                supports: supports.clone(),
            },
            EvidenceClaimProposal {
                evidence_id: designation_claim_id.clone(),
                source_url: case_snapshot.source_url.clone(),
                source_title,
                evidence_excerpt: format!(
                    "Imported FAA ACFTREF reports model_name={faa_model_designation:?}; {provenance}"
                ),
                source_kind: crate::aircraft::catalog::EvidenceSourceKind::Regulator,
                supports,
            },
        ];
        Ok(Self {
            case_token,
            case_snapshot,
            faa_manufacturer_name,
            faa_model_designation,
            make_claim_id,
            designation_claim_id,
            listing_model_years,
            faa_years_manufactured,
            observation_bindings,
            tcds_selection_basis: None,
            tcds_identity_binding: None,
            tcds_family_binding: None,
            tcds_make_lineage_evidence: None,
            claims,
        })
    }

    pub fn claims(&self) -> &[EvidenceClaimProposal] {
        &self.claims
    }

    pub fn faa_manufacturer_name(&self) -> &str {
        &self.faa_manufacturer_name
    }

    pub fn faa_model_designation(&self) -> &str {
        &self.faa_model_designation
    }

    pub fn make_claim_id(&self) -> &str {
        &self.make_claim_id
    }

    pub fn designation_claim_id(&self) -> &str {
        &self.designation_claim_id
    }

    /// Attach regulator-owned exact-designation and serial-applicability proof.
    ///
    /// This proof is mandatory even when the TCDS does not name a marketing
    /// family. Its evidence IDs include the complete case token and
    /// digest-addressed PDF provenance, so they cannot be replayed against a
    /// different listing or TCDS revision.
    pub(crate) fn attach_tcds_identity_binding(
        &mut self,
        binding: TcdsIdentityBinding,
    ) -> Result<(), String> {
        if self.tcds_identity_binding.is_some() {
            return Err("server FAA evidence already has a TCDS identity binding".to_string());
        }
        let every_observation_has_exact_serial = !self.observation_bindings.is_empty()
            && self.observation_bindings.iter().all(|observation| {
                observation.grounding.manufacturer_serial_key.as_deref()
                    == Some(binding.faa_serial_key.as_str())
            });
        if binding.exact_faa_model != self.faa_model_designation
            || !every_observation_has_exact_serial
        {
            return Err(
                "FAA TCDS identity binding does not match this exact registry model and manufacturer serial".to_string(),
            );
        }

        let ids = self.tcds_identity_claim_ids_for(&binding);
        let source_title = format!(
            "FAA Type Certificate Data Sheet {}{}",
            binding.tcds_number,
            binding
                .revision_number
                .as_deref()
                .map(|revision| format!(" revision {revision}"))
                .unwrap_or_default()
        );
        let hierarchy_support = [crate::aircraft::catalog::EvidenceClaimKind::HierarchyIdentity]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let serial_support = [crate::aircraft::catalog::EvidenceClaimKind::SerialApplicability]
            .into_iter()
            .collect::<BTreeSet<_>>();
        self.claims.extend([
            EvidenceClaimProposal {
                evidence_id: ids.faa_model_heading,
                source_url: binding.source_url.clone(),
                source_title: source_title.clone(),
                evidence_excerpt: binding.faa_model_heading.excerpt.clone(),
                source_kind: crate::aircraft::catalog::EvidenceSourceKind::TypeCertificate,
                supports: hierarchy_support,
            },
            EvidenceClaimProposal {
                evidence_id: ids.serial_eligibility,
                source_url: binding.source_url.clone(),
                source_title,
                evidence_excerpt: binding.serial_eligibility.excerpt.clone(),
                source_kind: crate::aircraft::catalog::EvidenceSourceKind::TypeCertificate,
                supports: serial_support,
            },
        ]);
        self.tcds_identity_binding = Some(binding);
        Ok(())
    }

    pub(crate) fn attach_tcds_selection_basis(
        &mut self,
        basis: TcdsSelectionBasis,
    ) -> Result<(), String> {
        if self.tcds_selection_basis.is_some() {
            return Err("server FAA evidence already has a TCDS selection basis".to_string());
        }
        let identity = self.tcds_identity_binding.as_ref().ok_or_else(|| {
            "server FAA evidence requires a TCDS identity before its selection basis".to_string()
        })?;
        let registry_tcds_numbers = self
            .observation_bindings
            .iter()
            .filter_map(|observation| {
                observation
                    .grounding
                    .aircraft
                    .as_ref()?
                    .type_certificate_data_sheet
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .collect::<BTreeSet<_>>();
        let valid = match basis {
            TcdsSelectionBasis::RegistryReference => {
                registry_tcds_numbers.len() == 1
                    && registry_tcds_numbers.contains(identity.tcds_number.as_str())
            }
            TcdsSelectionBasis::DrsUniqueCurrentExactModel
            | TcdsSelectionBasis::OperatorValidatedExactModelSerial => {
                registry_tcds_numbers.is_empty()
            }
        };
        if !valid {
            return Err(
                "FAA TCDS selection basis does not match the exact ACFTREF TCDS presence/value"
                    .to_string(),
            );
        }
        self.tcds_selection_basis = Some(basis);
        Ok(())
    }

    /// Attach an optional named-family projection from the already-bound exact
    /// TCDS identity. The retained listing label is audit input only: it is not
    /// represented as, or required to equal, a TCDS model heading.
    pub(crate) fn attach_tcds_family_binding(
        &mut self,
        binding: TcdsFamilyBinding,
    ) -> Result<(), String> {
        if self.tcds_family_binding.is_some() {
            return Err("server FAA evidence already has a TCDS family binding".to_string());
        }
        let identity = self.tcds_identity_binding.as_ref().ok_or_else(|| {
            "server FAA evidence requires a TCDS identity binding before its family projection"
                .to_string()
        })?;
        let observed_models = self
            .observation_bindings
            .iter()
            .map(|observation| observation.observed_model.trim())
            .filter(|model| !model.is_empty())
            .collect::<BTreeSet<_>>();
        if observed_models.len() != 1
            || !observed_models.contains(binding.observed_model.as_str())
            || binding.canonical_family_name.trim().is_empty()
            || !family_binding_matches_identity(&binding, identity)
        {
            return Err(
                "FAA TCDS family projection does not match its exact identity proof and retained audit label"
                    .to_string(),
            );
        }
        self.tcds_family_binding = Some(binding);
        Ok(())
    }

    pub(crate) fn attach_tcds_make_lineage_evidence(
        &mut self,
        evidence: TcdsMakeLineageEvidence,
    ) -> Result<(), String> {
        if self.tcds_make_lineage_evidence.is_some() {
            return Err("server FAA evidence already has TCDS make-lineage evidence".to_string());
        }
        let identity = self.tcds_identity_binding.as_ref().ok_or_else(|| {
            "server FAA evidence requires a TCDS identity binding before make-lineage evidence"
                .to_string()
        })?;
        if evidence.document_guid != identity.document_guid
            || evidence.tcds_number != identity.tcds_number
            || evidence.source_url != identity.source_url
            || evidence.pdf_sha256 != identity.pdf_sha256
            || evidence.exact_faa_model != identity.exact_faa_model
            || evidence.faa_serial_key != identity.faa_serial_key
            || evidence.holder_transfer.is_none()
            || self.tcds_selection_basis.is_none()
            || evidence
                .manufacturer_serial_eligibility
                .as_ref()
                .is_some_and(|manufacturer| manufacturer.model != identity.exact_faa_model)
        {
            return Err(
                "FAA TCDS make-lineage evidence does not match its exact document/designation/serial proof or lacks holder-transfer evidence"
                    .to_string(),
            );
        }
        let ids = self.tcds_make_lineage_claim_ids_for(&evidence, identity);
        let source_title = format!(
            "FAA Type Certificate Data Sheet {}{}",
            identity.tcds_number,
            identity
                .revision_number
                .as_deref()
                .map(|revision| format!(" revision {revision}"))
                .unwrap_or_default()
        );
        let hierarchy_support = [crate::aircraft::catalog::EvidenceClaimKind::HierarchyIdentity]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let serial_support = [crate::aircraft::catalog::EvidenceClaimKind::SerialApplicability]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let holder = evidence
            .holder_transfer
            .as_ref()
            .expect("holder transfer was validated above");
        self.claims.push(EvidenceClaimProposal {
            evidence_id: ids.holder_transfer,
            source_url: identity.source_url.clone(),
            source_title: source_title.clone(),
            evidence_excerpt: holder.excerpt.clone(),
            source_kind: crate::aircraft::catalog::EvidenceSourceKind::TypeCertificate,
            supports: hierarchy_support,
        });
        if let (Some(manufacturer), Some(evidence_id)) = (
            evidence.manufacturer_serial_eligibility.as_ref(),
            ids.manufacturer_serial_eligibility,
        ) {
            self.claims.push(EvidenceClaimProposal {
                evidence_id,
                source_url: identity.source_url.clone(),
                source_title,
                evidence_excerpt: manufacturer.excerpt.clone(),
                source_kind: crate::aircraft::catalog::EvidenceSourceKind::TypeCertificate,
                supports: serial_support,
            });
        }
        self.tcds_make_lineage_evidence = Some(evidence);
        Ok(())
    }

    pub(crate) fn tcds_make_lineage_evidence(&self) -> Option<&TcdsMakeLineageEvidence> {
        self.tcds_make_lineage_evidence.as_ref()
    }

    pub(crate) fn catalog_server_candidate_keys(&self) -> AircraftCatalogServerCandidateKeys {
        let exact_tcds_holder_names = self
            .tcds_make_lineage_evidence
            .as_ref()
            .and_then(|evidence| evidence.holder_transfer.as_ref())
            .into_iter()
            .flat_map(|holder| {
                [
                    holder.former_holder_name.clone(),
                    holder.current_holder_name.clone(),
                ]
            })
            .collect();
        AircraftCatalogServerCandidateKeys {
            exact_tcds_holder_names,
            exact_tcds_family_name: self
                .tcds_family_binding
                .as_ref()
                .map(|binding| binding.canonical_family_name.clone()),
        }
    }

    fn tcds_identity_claim_ids(&self) -> Option<TcdsIdentityClaimIds> {
        self.tcds_identity_binding
            .as_ref()
            .map(|binding| self.tcds_identity_claim_ids_for(binding))
    }

    fn tcds_family_claim_ids(&self) -> Option<TcdsIdentityClaimIds> {
        self.tcds_family_binding.as_ref()?;
        self.tcds_identity_claim_ids()
    }

    fn tcds_identity_claim_ids_for(&self, binding: &TcdsIdentityBinding) -> TcdsIdentityClaimIds {
        let provenance = format!(
            "case_token={};snapshot_id={};source_record_sha256s={};document_guid={};document_url={};tcds={};revision={:?};revision_date={:?};source_url={};pdf_sha256={};faa_model={};serial={};faa_heading_sha256={};serial_sha256={}",
            self.case_token,
            self.case_snapshot.id,
            self.observation_bindings
                .iter()
                .map(|observation| observation.grounding.source_record_sha256.as_str())
                .collect::<Vec<_>>()
                .join(","),
            binding.document_guid,
            binding.document_url,
            binding.tcds_number,
            binding.revision_number,
            binding.revision_date,
            binding.source_url,
            binding.pdf_sha256,
            binding.exact_faa_model,
            binding.faa_serial_key,
            binding.faa_model_heading.normalized_excerpt_sha256,
            binding.serial_eligibility.normalized_excerpt_sha256,
        );
        TcdsIdentityClaimIds {
            faa_model_heading: server_faa_drs_claim_id(
                &self.case_token,
                "faa_model_heading",
                &provenance,
            ),
            serial_eligibility: server_faa_drs_claim_id(
                &self.case_token,
                "serial_eligibility",
                &provenance,
            ),
        }
    }

    fn tcds_make_lineage_claim_ids(&self) -> Option<TcdsMakeLineageClaimIds> {
        let identity = self.tcds_identity_binding.as_ref()?;
        let evidence = self.tcds_make_lineage_evidence.as_ref()?;
        Some(self.tcds_make_lineage_claim_ids_for(evidence, identity))
    }

    fn validate_tcds_make_lineage_relationship(
        &self,
        relationship: &FaaMakeRelationshipDecision,
        selected_make: &str,
    ) -> Result<(), String> {
        let evidence = self.tcds_make_lineage_evidence.as_ref().ok_or_else(|| {
            "FAA TCDS make-lineage action has no exact case-bound lineage evidence".to_string()
        })?;
        let holder = evidence.holder_transfer.as_ref().ok_or_else(|| {
            "FAA TCDS make-lineage action requires exact holder-transfer evidence".to_string()
        })?;
        let claim_ids = self.tcds_make_lineage_claim_ids().ok_or_else(|| {
            "FAA TCDS make-lineage action has no deterministic claim set".to_string()
        })?;
        let expected_identity = std::iter::once(self.make_claim_id.as_str())
            .chain(claim_ids.identity())
            .collect::<BTreeSet<_>>();
        let expected_applicability = claim_ids.applicability();
        let actual_identity = relationship
            .evidence_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let actual_applicability = relationship
            .applicability_evidence_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let selected_is_exact_holder =
            tcds_holder_names_match(selected_make, holder.former_holder_name.as_str())
                || tcds_holder_names_match(selected_make, holder.current_holder_name.as_str());
        let manufacturer_is_holder = evidence
            .manufacturer_serial_eligibility
            .as_ref()
            .is_none_or(|manufacturer| {
                tcds_holder_names_match(
                    manufacturer.manufacturer_name.as_str(),
                    holder.former_holder_name.as_str(),
                ) || tcds_holder_names_match(
                    manufacturer.manufacturer_name.as_str(),
                    holder.current_holder_name.as_str(),
                )
            });
        if relationship.action != FaaMakeRelationshipAction::MatchTcdsMakeLineage
            || relationship.faa_manufacturer_name != self.faa_manufacturer_name
            || relationship.canonical_make_name != selected_make
            || relationship.existing_alias_id.is_some()
            || relationship.valid_from_model_year.is_some()
            || relationship.valid_to_model_year.is_some()
            || relationship.evidence_ids.len() != expected_identity.len()
            || actual_identity != expected_identity
            || relationship.applicability_evidence_ids.len() != expected_applicability.len()
            || actual_applicability != expected_applicability
            || !selected_is_exact_holder
            || !manufacturer_is_holder
        {
            return Err(
                "FAA TCDS make-lineage action must exactly bind the registry make, parsed holder names, exact model/serial claims, and selected existing holder without an alias or year scope"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn tcds_make_lineage_relationship(
        &self,
        selected_make: &str,
    ) -> Option<FaaMakeRelationshipDecision> {
        let evidence = self.tcds_make_lineage_evidence.as_ref()?;
        let holder = evidence.holder_transfer.as_ref()?;
        if !self.has_exact_tcds_designation_serial_proof()
            || !(tcds_holder_names_match(selected_make, &holder.former_holder_name)
                || tcds_holder_names_match(selected_make, &holder.current_holder_name))
        {
            return None;
        }
        let claim_ids = self.tcds_make_lineage_claim_ids()?;
        Some(FaaMakeRelationshipDecision {
            action: FaaMakeRelationshipAction::MatchTcdsMakeLineage,
            faa_manufacturer_name: self.faa_manufacturer_name.clone(),
            canonical_make_name: selected_make.to_string(),
            existing_alias_id: None,
            valid_from_model_year: None,
            valid_to_model_year: None,
            evidence_ids: std::iter::once(self.make_claim_id.clone())
                .chain(claim_ids.identity().into_iter().map(str::to_string))
                .collect(),
            applicability_evidence_ids: claim_ids
                .applicability()
                .into_iter()
                .map(str::to_string)
                .collect(),
            rationale: format!(
                "Exact, case-bound FAA TCDS holder lineage maps registry make {:?} to existing holder {:?} for this exact designation and manufacturer serial.",
                self.faa_manufacturer_name, selected_make
            ),
        })
    }

    fn tcds_make_lineage_claim_ids_for(
        &self,
        evidence: &TcdsMakeLineageEvidence,
        identity: &TcdsIdentityBinding,
    ) -> TcdsMakeLineageClaimIds {
        let identity_ids = self.tcds_identity_claim_ids_for(identity);
        let holder = evidence
            .holder_transfer
            .as_ref()
            .expect("attached make lineage always has holder-transfer evidence");
        let base_provenance = format!(
            "case_token={};snapshot_id={};document_guid={};tcds={};source_url={};pdf_sha256={};faa_model={};serial={}",
            self.case_token,
            self.case_snapshot.id,
            evidence.document_guid,
            evidence.tcds_number,
            evidence.source_url,
            evidence.pdf_sha256,
            evidence.exact_faa_model,
            evidence.faa_serial_key,
        );
        let holder_provenance = format!(
            "{base_provenance};page={};excerpt_sha256={};former_holder={};current_holder={};effective_date={}",
            holder.page_number,
            holder.normalized_excerpt_sha256,
            holder.former_holder_name,
            holder.current_holder_name,
            holder.effective_date_text,
        );
        let manufacturer_serial_eligibility =
            evidence.manufacturer_serial_eligibility.as_ref().map(|range| {
                let provenance = format!(
                    "{base_provenance};page={};excerpt_sha256={};manufacturer={};model={};first_serial={};last_serial={:?}",
                    range.page_number,
                    range.normalized_excerpt_sha256,
                    range.manufacturer_name,
                    range.model,
                    range.first_serial_key,
                    range.last_serial_key,
                );
                server_faa_drs_claim_id(
                    &self.case_token,
                    "manufacturer_serial_eligibility",
                    &provenance,
                )
            });
        TcdsMakeLineageClaimIds {
            faa_model_heading: identity_ids.faa_model_heading,
            serial_eligibility: identity_ids.serial_eligibility,
            holder_transfer: server_faa_drs_claim_id(
                &self.case_token,
                "holder_transfer",
                &holder_provenance,
            ),
            manufacturer_serial_eligibility,
        }
    }

    fn tcds_family_relationship(
        &self,
        selected_family: &str,
    ) -> Option<FamilyLabelRelationshipDecision> {
        let binding = self.tcds_family_binding.as_ref()?;
        (binding.canonical_family_name == selected_family).then(|| {
            let ids = self
                .tcds_family_claim_ids()
                .expect("binding has deterministic claim IDs");
            FamilyLabelRelationshipDecision {
                action: FamilyLabelRelationshipAction::MatchFaaTypeCertificateFamily,
                observed_family_label: binding.observed_model.clone(),
                canonical_family_name: binding.canonical_family_name.clone(),
                existing_alias_id: None,
                valid_from_model_year: None,
                valid_to_model_year: None,
                evidence_ids: ids.all().into_iter().map(str::to_string).collect(),
                applicability_evidence_ids: Vec::new(),
                rationale: format!(
                    "Current FAA TCDS {} binds exact FAA designation {:?} to family {:?}, and its exact serial-eligibility row contains the FAA-matched manufacturer serial; retained listing label {:?} is audit input, not a claimed TCDS heading.",
                    binding.tcds_number,
                    binding.exact_faa_model,
                    binding.canonical_family_name,
                    binding.observed_model,
                ),
            }
        })
    }

    fn validate_tcds_family_relationship(
        &self,
        relationship: &FamilyLabelRelationshipDecision,
    ) -> Result<(), String> {
        let binding = self
            .tcds_family_binding
            .as_ref()
            .ok_or_else(|| "FAA type-certificate relationship has no bound TCDS".to_string())?;
        let expected = self
            .tcds_family_relationship(&binding.canonical_family_name)
            .expect("bound family produces a relationship");
        let actual_ids = relationship
            .evidence_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected_claim_ids = self
            .tcds_family_claim_ids()
            .expect("bound family has claim IDs");
        let expected_ids = expected_claim_ids.all();
        if relationship.action != FamilyLabelRelationshipAction::MatchFaaTypeCertificateFamily
            || relationship.observed_family_label != expected.observed_family_label
            || relationship.canonical_family_name != expected.canonical_family_name
            || relationship.existing_alias_id.is_some()
            || relationship.valid_from_model_year.is_some()
            || relationship.valid_to_model_year.is_some()
            || !relationship.applicability_evidence_ids.is_empty()
            || relationship.evidence_ids.len() != expected_ids.len()
            || actual_ids != expected_ids
        {
            return Err(
                "FAA type-certificate family relationship does not exactly match its case-bound model/family/serial proof"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn drs_source_for_evidence_id(&self, evidence_id: &str) -> Option<(&str, &str)> {
        let binding = self.tcds_identity_binding.as_ref()?;
        let is_identity_claim = self.tcds_identity_claim_ids()?.all().contains(evidence_id);
        let is_lineage_claim = self
            .tcds_make_lineage_claim_ids()
            .is_some_and(|ids| ids.all().contains(evidence_id));
        (is_identity_claim || is_lineage_claim)
            .then_some((binding.source_url.as_str(), binding.pdf_sha256.as_str()))
    }

    fn verify_observation_binding(
        &self,
        listing_id: i64,
        observation_sha256: &str,
        listing_model_year: i64,
        grounding: &AircraftGrounding,
    ) -> Result<(), String> {
        let exact_binding = self.observation_bindings.iter().any(|binding| {
            binding.listing_id == listing_id
                && binding.observation_sha256 == observation_sha256
                && binding.listing_model_year == listing_model_year
                && &binding.grounding == grounding
        });
        if !exact_binding {
            return Err(
                "reviewable hierarchy server FAA evidence is not bound to this exact listing observation and grounding"
                    .to_string(),
            );
        }
        if grounding.snapshot.snapshot_date != self.case_snapshot.snapshot_date
            || grounding.snapshot.source_url != self.case_snapshot.source_url
            || grounding.snapshot.archive_sha256 != self.case_snapshot.archive_sha256
            || grounding.snapshot.source_manifest_sha256
                != self.case_snapshot.source_manifest_sha256
        {
            return Err(
                "reviewable hierarchy server FAA evidence belongs to a different FAA release"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn contains_exact_claim(&self, claim: &EvidenceClaimProposal) -> bool {
        self.claims.iter().any(|expected| expected == claim)
    }

    fn contains_id(&self, evidence_id: &str) -> bool {
        self.claims
            .iter()
            .any(|claim| claim.evidence_id == evidence_id)
    }

    fn is_reserved_id(evidence_id: &str) -> bool {
        evidence_id.starts_with(SERVER_FAA_REGISTRY_EVIDENCE_ID_PREFIX)
            || evidence_id.starts_with(SERVER_FAA_DRS_EVIDENCE_ID_PREFIX)
    }

    pub fn attach_to(&self, research: &mut AircraftIdentityEvidenceResearch) -> Result<(), String> {
        if research
            .claims
            .iter()
            .any(|claim| Self::is_reserved_id(claim.evidence_id.trim()))
        {
            return Err(
                "Gemini returned an evidence id reserved for server-created FAA registry claims"
                    .to_string(),
            );
        }
        if research
            .family_candidates
            .iter()
            .chain(research.generation_candidates.iter())
            .chain(research.package_candidates.iter())
            .flat_map(|candidate| candidate.evidence_ids.iter())
            .any(|evidence_id| Self::is_reserved_id(evidence_id.trim()))
        {
            return Err(
                "Gemini returned a hierarchy-candidate evidence id reserved for server-created FAA claims"
                    .to_string(),
            );
        }
        research.claims.extend(self.claims.iter().cloned());
        if let (Some(binding), Some(ids)) = (
            self.tcds_family_binding.as_ref(),
            self.tcds_family_claim_ids(),
        ) {
            // The exact candidate is regulator-owned. Remove only an exact
            // duplicate or a mechanically recognizable composite such as
            // "182 Skylane"; retain every genuinely distinct model candidate
            // so validation can surface it as a TCDS conflict instead of
            // silently erasing potentially authoritative disagreement.
            research.family_candidates.retain(|candidate| {
                let label = candidate.label.trim();
                label != binding.canonical_family_name
                    && !(contains_exact_contiguous_label(label, &binding.canonical_family_name)
                        && family_candidate_forbidden_component(label, self).is_some())
            });
            research.family_candidates.push(HierarchyCandidate {
                label: binding.canonical_family_name.clone(),
                evidence_ids: ids.hierarchy(),
            });
        }
        // The tools-disabled structure model may attach a claim that it
        // explicitly typed only as production applicability to a hierarchy
        // candidate merely because the excerpt repeats the family name. Claim
        // typing is already part of the immutable structured dossier, so
        // remove only known references whose declared support does not include
        // hierarchy identity. A hierarchy-typed secondary or otherwise
        // non-authoritative claim remains attached so ordinary validation
        // rejects it; unknown IDs likewise remain fail-closed. Claims
        // themselves are never deleted or reclassified.
        let known_claim_ids = research
            .claims
            .iter()
            .map(|claim| claim.evidence_id.clone())
            .collect::<BTreeSet<_>>();
        let hierarchy_typed_claim_ids = research
            .claims
            .iter()
            .filter(|claim| {
                claim
                    .supports
                    .contains(&crate::aircraft::catalog::EvidenceClaimKind::HierarchyIdentity)
            })
            .map(|claim| claim.evidence_id.clone())
            .collect::<BTreeSet<_>>();
        let retain_unknown_or_hierarchy_typed = |evidence_id: &String| {
            !known_claim_ids.contains(evidence_id)
                || hierarchy_typed_claim_ids.contains(evidence_id)
        };
        for candidate in &mut research.family_candidates {
            candidate
                .evidence_ids
                .retain(&retain_unknown_or_hierarchy_typed);
        }
        for candidate in &mut research.generation_candidates {
            candidate
                .evidence_ids
                .retain(&retain_unknown_or_hierarchy_typed);
        }
        for candidate in &mut research.package_candidates {
            candidate
                .evidence_ids
                .retain(&retain_unknown_or_hierarchy_typed);
        }

        Ok(())
    }

    /// Build the only research dossier that may use regulator-complete mode.
    ///
    /// This is deliberately recomputed from private, server-owned state. The
    /// mode is unavailable unless one exact registry case has a selected,
    /// digest- and serial-bound TCDS identity, a named-family projection, and
    /// holder-lineage evidence. Every retained model/variant token must also
    /// be attributable without rewriting the listing text.
    pub(crate) fn regulator_complete_research(&self) -> Option<AircraftIdentityEvidenceResearch> {
        self.tcds_selection_basis?;
        self.tcds_identity_binding.as_ref()?;
        if !self.has_exact_tcds_designation_serial_proof() {
            return None;
        }
        self.tcds_family_binding.as_ref()?;
        self.tcds_make_lineage_evidence
            .as_ref()?
            .holder_transfer
            .as_ref()?;
        if !self
            .unaccounted_observed_regulator_hierarchy_tokens()
            .is_empty()
        {
            return None;
        }

        let mut research = AircraftIdentityEvidenceResearch {
            subject_summary: concat!(
                "Exact server-owned FAA registry identity and current, digest-bound FAA ",
                "type-certificate designation, serial, named-family, and holder-lineage evidence."
            )
            .to_string(),
            claims: Vec::new(),
            family_candidates: Vec::new(),
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: Vec::new(),
            unresolved_questions: Vec::new(),
        };
        self.attach_to(&mut research).ok()?;
        Some(research)
    }

    /// Return original retained tokens not attributable to exact regulator
    /// identity. Original spelling is retained for bounded search terms; all
    /// matching remains comparison-only.
    pub(crate) fn unaccounted_observed_regulator_hierarchy_tokens(&self) -> Vec<String> {
        let family = self.tcds_family_binding.as_ref();
        let exact_tcds_designation_serial_proof = self.has_exact_tcds_designation_serial_proof();
        let mut unaccounted = Vec::new();

        for (observed_label, other_observed_label) in
            self.observation_bindings.iter().flat_map(|binding| {
                [
                    (
                        binding.observed_model.as_str(),
                        binding.observed_variant.as_str(),
                    ),
                    (
                        binding.observed_variant.as_str(),
                        binding.observed_model.as_str(),
                    ),
                ]
            })
        {
            let exact_designation_is_in_other_field =
                retained_field_accounts_for_exact_faa_designation(
                    other_observed_label,
                    self.faa_model_designation(),
                    exact_tcds_designation_serial_proof,
                );
            let exact_turbo_series_is_in_other_field = family.is_some_and(|binding| {
                retained_field_is_exact_turbo_designation_family_phrase(
                    other_observed_label,
                    self.faa_model_designation(),
                    &binding.canonical_family_name,
                    exact_tcds_designation_serial_proof,
                )
            });
            let mut normalized_unaccounted = unaccounted_observed_hierarchy_tokens(
                observed_label,
                self.faa_model_designation(),
                family.map(|binding| binding.canonical_family_name.as_str()),
                None,
                None,
                None,
                None,
                self.faa_manufacturer_name(),
                exact_tcds_designation_serial_proof,
                exact_designation_is_in_other_field,
                exact_turbo_series_is_in_other_field,
                family.map(|binding| binding.canonical_family_name.as_str()),
            );
            for original in observed_label
                .split(|character: char| !character.is_ascii_alphanumeric())
                .filter(|token| !token.is_empty())
            {
                let normalized = original.to_lowercase();
                if let Some(index) = normalized_unaccounted
                    .iter()
                    .position(|token| token == &normalized)
                {
                    normalized_unaccounted.remove(index);
                    unaccounted.push(original.to_string());
                }
            }
            // `alphanumeric_tokens` accepts non-ASCII alphanumerics while the
            // original-spelling splitter above is intentionally ASCII-bound.
            // Preserve any unmatched token in normalized form so it can never
            // disappear from the admission gate.
            unaccounted.extend(normalized_unaccounted);
        }
        unaccounted
    }

    fn has_exact_tcds_designation_serial_proof(&self) -> bool {
        let Some(binding) = self.tcds_identity_binding.as_ref() else {
            return false;
        };
        self.tcds_selection_basis.is_some()
            && binding.exact_faa_model == self.faa_model_designation
            && !self.observation_bindings.is_empty()
            && self.observation_bindings.iter().all(|observation| {
                observation.grounding.manufacturer_serial_key.as_deref()
                    == Some(binding.faa_serial_key.as_str())
            })
    }
}

fn family_binding_matches_identity(
    family: &TcdsFamilyBinding,
    identity: &TcdsIdentityBinding,
) -> bool {
    family.identity_binding() == *identity
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn server_faa_claim_id(case_token: &str, kind: &str, value: &str, provenance: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aircost-server-faa-evidence-v1\0");
    hasher.update(case_token.as_bytes());
    hasher.update(b"\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.as_bytes());
    hasher.update(b"\0");
    hasher.update(provenance.as_bytes());
    format!(
        "{SERVER_FAA_REGISTRY_EVIDENCE_ID_PREFIX}{kind}.{:x}",
        hasher.finalize()
    )
}

fn server_faa_drs_claim_id(case_token: &str, kind: &str, provenance: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aircost-server-faa-drs-evidence-v1\0");
    hasher.update(case_token.as_bytes());
    hasher.update(b"\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(provenance.as_bytes());
    format!(
        "{SERVER_FAA_DRS_EVIDENCE_ID_PREFIX}{kind}.{:x}",
        hasher.finalize()
    )
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingMode {
    /// Fresh Google Search and URL Context evidence was observed.
    #[default]
    FreshWeb,
    /// A previously verified, exact-scope web dossier was reused.
    ReusedVerifiedDossier,
    /// The server proved the complete required identity directly from its
    /// exact FAA registry row and current, digest-bound TCDS.
    RegulatorComplete,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct GroundingAudit {
    pub mode: GroundingMode,
    pub google_search_call_count: usize,
    pub url_context_call_count: usize,
    pub citation_urls: BTreeSet<String>,
    /// True only when the shared grounding workflow validated and reused an
    /// immutable dossier bound to this exact domain evidence scope.
    pub reused_verified_dossier: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CatalogCandidateRegistry {
    pub ids_by_kind: BTreeMap<HierarchyEntityKind, BTreeSet<i64>>,
    pub identities_by_kind:
        BTreeMap<HierarchyEntityKind, BTreeMap<i64, AircraftCatalogCandidateIdentity>>,
    pub make_aliases_by_id: BTreeMap<i64, AircraftCatalogAliasCandidate>,
    pub family_aliases_by_id: BTreeMap<i64, AircraftCatalogAliasCandidate>,
    pub catalog_revision: Option<String>,
    pub search_request: Option<AircraftCatalogSearchRequest>,
    pub generation_designations: BTreeSet<(i64, i64)>,
    pub package_applicability: Vec<AircraftCatalogPackageApplicabilityRow>,
}

/// Immutable identity fields copied from one exact live catalog candidate.
///
/// Candidate IDs are table-local, so an ID alone cannot prove either the
/// entity kind or its hierarchy owner. Validation keeps these fields separate
/// from retrieval scores so `match_existing` can bind to the exact row that
/// the server returned.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AircraftCatalogCandidateIdentity {
    pub entity_kind: HierarchyEntityKind,
    pub catalog_id: i64,
    pub display_name: String,
    pub authoritative_designator: Option<String>,
    pub parent_catalog_id: Option<i64>,
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AircraftCatalogServerCandidateKeys {
    pub(crate) exact_tcds_holder_names: BTreeSet<String>,
    pub(crate) exact_tcds_family_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AircraftCatalogCandidate {
    pub entity_kind: HierarchyEntityKind,
    pub catalog_id: i64,
    pub display_name: String,
    pub authoritative_designator: Option<String>,
    pub parent_catalog_id: Option<i64>,
    pub aliases: Vec<String>,
    pub approved_aliases: Vec<AircraftCatalogAliasCandidate>,
    pub identifiers: Vec<String>,
    pub retrieval_score: f64,
    pub retrieval_reasons: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AircraftCatalogAliasCandidate {
    pub alias_id: i64,
    pub owner_catalog_id: i64,
    pub alias: String,
    pub valid_from_model_year: Option<i64>,
    pub valid_to_model_year: Option<i64>,
    pub market_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AircraftCatalogSearchResponse {
    pub catalog_revision: String,
    pub catalog_is_empty: bool,
    pub search_request: AircraftCatalogSearchRequest,
    pub allowed_existing_ids_by_kind: BTreeMap<HierarchyEntityKind, Vec<i64>>,
    pub candidates: Vec<AircraftCatalogCandidate>,
    pub generation_designations: Vec<AircraftCatalogGenerationDesignationRow>,
    pub package_applicability: Vec<AircraftCatalogPackageApplicabilityRow>,
    pub warning: String,
}

impl AircraftCatalogSearchResponse {
    pub fn candidate_registry(&self) -> CatalogCandidateRegistry {
        let mut registry = CatalogCandidateRegistry {
            catalog_revision: Some(self.catalog_revision.clone()),
            search_request: Some(self.search_request.clone()),
            generation_designations: self
                .generation_designations
                .iter()
                .map(|row| (row.aircraft_generation_id, row.aircraft_designation_id))
                .collect(),
            package_applicability: self.package_applicability.clone(),
            ..CatalogCandidateRegistry::default()
        };
        for candidate in &self.candidates {
            if self
                .allowed_existing_ids_by_kind
                .get(&candidate.entity_kind)
                .is_some_and(|ids| ids.contains(&candidate.catalog_id))
            {
                registry.insert(candidate.entity_kind, candidate.catalog_id);
            }
            registry
                .identities_by_kind
                .entry(candidate.entity_kind)
                .or_default()
                .insert(
                    candidate.catalog_id,
                    AircraftCatalogCandidateIdentity {
                        entity_kind: candidate.entity_kind,
                        catalog_id: candidate.catalog_id,
                        display_name: candidate.display_name.clone(),
                        authoritative_designator: candidate.authoritative_designator.clone(),
                        parent_catalog_id: candidate.parent_catalog_id,
                    },
                );
            if candidate.entity_kind == HierarchyEntityKind::Make {
                for alias in &candidate.approved_aliases {
                    registry
                        .make_aliases_by_id
                        .insert(alias.alias_id, alias.clone());
                }
            } else if candidate.entity_kind == HierarchyEntityKind::Family {
                for alias in &candidate.approved_aliases {
                    registry
                        .family_aliases_by_id
                        .insert(alias.alias_id, alias.clone());
                }
            }
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
    lookup_id: i64,
    display_value: String,
    normalized_value: String,
    valid_from_model_year: Option<i64>,
    valid_to_model_year: Option<i64>,
    market_code: Option<String>,
}

#[derive(Clone, Debug, Eq, FromRow, PartialEq, Serialize)]
pub struct AircraftCatalogGenerationDesignationRow {
    pub aircraft_generation_id: i64,
    pub aircraft_designation_id: i64,
}

#[derive(Clone, Debug, Eq, FromRow, PartialEq, Serialize)]
pub struct AircraftCatalogPackageApplicabilityRow {
    pub applicability_id: i64,
    pub aircraft_factory_package_id: i64,
    pub package_kind: String,
    pub aircraft_designation_id: i64,
    pub aircraft_generation_id: Option<i64>,
    pub valid_from_model_year: Option<i64>,
    pub valid_to_model_year: Option<i64>,
}

/// Search the approved catalog for candidates. Scores are deliberately only
/// retrieval hints. Same-family designation siblings are returned even with a
/// low score so collision-prone identities remain visible to the adjudicator.
pub async fn search_approved_aircraft_catalog(
    db: &AppDb,
    request: &AircraftCatalogSearchRequest,
) -> Result<AircraftCatalogSearchResponse, sqlx::Error> {
    search_approved_aircraft_catalog_internal(db, request, None).await
}

pub(crate) async fn search_approved_aircraft_catalog_with_server_keys(
    db: &AppDb,
    request: &AircraftCatalogSearchRequest,
    server_keys: &AircraftCatalogServerCandidateKeys,
) -> Result<AircraftCatalogSearchResponse, sqlx::Error> {
    search_approved_aircraft_catalog_internal(db, request, Some(server_keys)).await
}

async fn search_approved_aircraft_catalog_internal(
    db: &AppDb,
    request: &AircraftCatalogSearchRequest,
    server_keys: Option<&AircraftCatalogServerCandidateKeys>,
) -> Result<AircraftCatalogSearchResponse, sqlx::Error> {
    let base_rows = load_catalog_base_rows(db).await?;
    let lookup_rows = load_catalog_lookup_rows(db).await?;
    let generation_designation_rows = load_catalog_generation_designation_rows(db).await?;
    let package_applicability_rows = load_catalog_package_applicability_rows(db).await?;
    let catalog_revision = catalog_revision(
        &base_rows,
        &lookup_rows,
        &generation_designation_rows,
        &package_applicability_rows,
    );
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

    // Surface the complete existing branch for an exact certified-designation
    // collision even when its canonical make is not textually similar to the
    // FAA registry legal make. This is candidate retrieval only; the
    // MatchTcdsMakeLineage action still requires exact case-bound FAA/TCDS
    // evidence before the branch can be selected.
    let exact_designation_key =
        crate::aircraft::catalog::normalize_aircraft_designator_retrieval_key(
            &request.observed_designation,
        );
    let exact_designation_family_ids = base_rows
        .iter()
        .filter(|row| row.entity_kind == "designation")
        .filter(|row| {
            row.authoritative_designator
                .as_deref()
                .is_some_and(|value| {
                    crate::aircraft::catalog::normalize_aircraft_designator_retrieval_key(value)
                        == exact_designation_key
                })
        })
        .filter_map(|row| row.parent_id)
        .collect::<BTreeSet<_>>();
    let exact_designation_make_ids = base_rows
        .iter()
        .filter(|row| {
            row.entity_kind == "family" && exact_designation_family_ids.contains(&row.entity_id)
        })
        .filter_map(|row| row.parent_id)
        .collect::<BTreeSet<_>>();
    let exact_holder_make_ids = base_rows
        .iter()
        .filter(|row| row.entity_kind == "make")
        .filter(|row| {
            server_keys.is_some_and(|keys| {
                keys.exact_tcds_holder_names
                    .iter()
                    .any(|holder| tcds_holder_names_match(&row.display_name, holder))
            })
        })
        .map(|row| row.entity_id)
        .collect::<BTreeSet<_>>();
    let exact_holder_family_ids = base_rows
        .iter()
        .filter(|row| {
            row.entity_kind == "family"
                && row
                    .parent_id
                    .is_some_and(|make_id| exact_holder_make_ids.contains(&make_id))
        })
        .map(|row| row.entity_id)
        .collect::<BTreeSet<_>>();
    let exact_tcds_named_family_ids = base_rows
        .iter()
        .filter(|row| {
            row.entity_kind == "family"
                && exact_holder_family_ids.contains(&row.entity_id)
                && server_keys
                    .and_then(|keys| keys.exact_tcds_family_name.as_deref())
                    .is_some_and(|name| row.display_name.eq_ignore_ascii_case(name))
        })
        .map(|row| row.entity_id)
        .collect::<BTreeSet<_>>();

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
        let exact_designation_ancestor = (kind == HierarchyEntityKind::Family
            && exact_designation_family_ids.contains(&row.entity_id))
            || (kind == HierarchyEntityKind::Make
                && exact_designation_make_ids.contains(&row.entity_id));
        let exact_holder_candidate = (kind == HierarchyEntityKind::Make
            && exact_holder_make_ids.contains(&row.entity_id))
            || (kind == HierarchyEntityKind::Family
                && exact_holder_family_ids.contains(&row.entity_id))
            || (matches!(
                kind,
                HierarchyEntityKind::Designation
                    | HierarchyEntityKind::Generation
                    | HierarchyEntityKind::Package
            ) && row
                .parent_id
                .is_some_and(|family_id| exact_holder_family_ids.contains(&family_id)));
        if score <= 0.0
            && !same_family_sibling
            && !exact_designation_ancestor
            && !exact_holder_candidate
        {
            continue;
        }
        if same_family_sibling {
            reasons.push("same_family_collision_candidate".to_string());
        }
        if exact_designation_ancestor {
            reasons.push("exact_designation_ancestor_candidate".to_string());
        }
        if kind == HierarchyEntityKind::Make && exact_holder_make_ids.contains(&row.entity_id) {
            reasons.push("exact_tcds_holder_candidate".to_string());
        } else if kind == HierarchyEntityKind::Family
            && exact_tcds_named_family_ids.contains(&row.entity_id)
        {
            reasons.push("exact_tcds_named_family_candidate".to_string());
        } else if exact_holder_candidate {
            reasons.push("exact_tcds_holder_branch_collision_candidate".to_string());
        }
        let aliases = row_lookups
            .into_iter()
            .flatten()
            .filter(|lookup| lookup.lookup_kind == "alias")
            .map(|lookup| lookup.display_value.clone())
            .collect::<Vec<_>>();
        let approved_aliases = row_lookups
            .into_iter()
            .flatten()
            .filter(|lookup| lookup.lookup_kind == "alias")
            .map(|lookup| AircraftCatalogAliasCandidate {
                alias_id: lookup.lookup_id,
                owner_catalog_id: row.entity_id,
                alias: lookup.display_value.clone(),
                valid_from_model_year: lookup.valid_from_model_year,
                valid_to_model_year: lookup.valid_to_model_year,
                market_code: lookup.market_code.clone(),
            })
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
            approved_aliases,
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

    let allowed_existing_ids_by_kind = allowed_existing_catalog_ids(request, candidates.as_slice());
    Ok(AircraftCatalogSearchResponse {
        catalog_revision,
        catalog_is_empty: base_rows.is_empty(),
        search_request: request.clone(),
        allowed_existing_ids_by_kind,
        candidates,
        generation_designations: generation_designation_rows,
        package_applicability: package_applicability_rows,
        warning: "Candidate retrieval is not identity evidence and never authorizes a merge. `match_existing` is forbidden unless the exact ID appears in allowed_existing_ids_by_kind for that entity kind; a designation is allowed only when its canonical authoritative_designator literally equals the exact FAA designation, while near-designation collisions remain visible as candidates. A family match must copy the returned display label exactly and its returned parent must be the selected existing make. Use `propose_new` only with positive exact primary evidence. `match_approved_alias` is forbidden unless its exact alias id appears in the selected make or family candidate's approved_aliases and its copied market/year scope applies. A new alias requires finite from/to years, and each bound must appear as an exact year token in cited direct-primary production-applicability evidence. A new family alias additionally requires direct primary same-claim co-naming and is forbidden when the returned catalog contains a same-make normalized alias or canonical-family collision, regardless of year or market scope. `no_supported_selection` is an operational NULL validated against this response's echoed query and complete generation/designation and package-applicability rows; it is never proof that a real-world dimension does not exist."
            .to_string(),
    })
}

fn allowed_existing_catalog_ids(
    request: &AircraftCatalogSearchRequest,
    candidates: &[AircraftCatalogCandidate],
) -> BTreeMap<HierarchyEntityKind, Vec<i64>> {
    let mut allowed = BTreeMap::new();
    for candidate in candidates {
        // Designation retrieval deliberately surfaces near-collisions, but a
        // normalized score is never merge authority. Only the exact canonical
        // designator stored on the row may authorize this FAA identity.
        if candidate.entity_kind == HierarchyEntityKind::Designation
            && candidate.authoritative_designator.as_deref().map(str::trim)
                != Some(request.observed_designation.trim())
        {
            continue;
        }
        allowed
            .entry(candidate.entity_kind)
            .or_insert_with(Vec::new)
            .push(candidate.catalog_id);
    }
    allowed
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
    generation_designation_rows: &[AircraftCatalogGenerationDesignationRow],
    package_applicability_rows: &[AircraftCatalogPackageApplicabilityRow],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        serde_json::to_vec(&(
            base_rows,
            lookup_rows,
            generation_designation_rows,
            package_applicability_rows,
        ))
        .expect("catalog rows serialize for hashing"),
    );
    format!("sha256:{:x}", hasher.finalize())
}

/// Fingerprint the complete approved hierarchy catalog using the same rows and
/// ordering returned to the curation model.
///
/// Persistence re-reads this value immediately before approving a reviewable
/// proposal so a decision cannot be applied to a catalog different from the
/// one it adjudicated.
pub async fn approved_aircraft_catalog_revision(db: &AppDb) -> Result<String, sqlx::Error> {
    let base_rows = load_catalog_base_rows(db).await?;
    let lookup_rows = load_catalog_lookup_rows(db).await?;
    let generation_designation_rows = load_catalog_generation_designation_rows(db).await?;
    let package_applicability_rows = load_catalog_package_applicability_rows(db).await?;
    Ok(catalog_revision(
        &base_rows,
        &lookup_rows,
        &generation_designation_rows,
        &package_applicability_rows,
    ))
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
        SELECT 'make' AS entity_kind, alias.aircraft_make_id AS entity_id,
               'alias' AS lookup_kind, alias.id AS lookup_id,
               alias.alias AS display_value,
               alias.normalized_alias AS normalized_value,
               alias.valid_from_model_year, alias.valid_to_model_year,
               market.code AS market_code
        FROM aircraft_make_aliases alias
        LEFT JOIN aircraft_markets market ON market.id = alias.aircraft_market_id
        UNION ALL
        SELECT 'family', alias.aircraft_model_family_id, 'alias', alias.id,
               alias.alias, alias.normalized_alias,
               alias.valid_from_model_year, alias.valid_to_model_year,
               market.code
        FROM aircraft_family_aliases alias
        LEFT JOIN aircraft_markets market ON market.id = alias.aircraft_market_id
        UNION ALL
        SELECT 'designation', alias.aircraft_designation_id, 'alias', alias.id,
               alias.alias, alias.normalized_alias,
               alias.valid_from_model_year, alias.valid_to_model_year,
               market.code
        FROM aircraft_designation_aliases alias
        LEFT JOIN aircraft_markets market ON market.id = alias.aircraft_market_id
        UNION ALL
        SELECT 'designation', aircraft_designation_id, 'identifier', id,
               identifier_value, normalized_identifier_value,
               NULL, NULL, NULL
        FROM aircraft_designation_identifiers
        ORDER BY entity_kind, entity_id, lookup_kind, normalized_value, lookup_id
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

async fn load_catalog_generation_designation_rows(
    db: &AppDb,
) -> Result<Vec<AircraftCatalogGenerationDesignationRow>, sqlx::Error> {
    let sql = r#"
        SELECT aircraft_generation_id, aircraft_designation_id
        FROM aircraft_generation_designations
        ORDER BY aircraft_generation_id, aircraft_designation_id
    "#;
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, AircraftCatalogGenerationDesignationRow>(sql)
                .fetch_all(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, AircraftCatalogGenerationDesignationRow>(sql)
                .fetch_all(pool)
                .await
        }
    }
}

async fn load_catalog_package_applicability_rows(
    db: &AppDb,
) -> Result<Vec<AircraftCatalogPackageApplicabilityRow>, sqlx::Error> {
    let sql = r#"
        SELECT applicability.id AS applicability_id,
               applicability.aircraft_factory_package_id,
               package.package_kind,
               applicability.aircraft_designation_id,
               applicability.aircraft_generation_id,
               applicability.valid_from_model_year,
               applicability.valid_to_model_year
        FROM aircraft_package_applicability applicability
        JOIN aircraft_factory_packages package
          ON package.id = applicability.aircraft_factory_package_id
        ORDER BY applicability.id
    "#;
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, AircraftCatalogPackageApplicabilityRow>(sql)
                .fetch_all(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, AircraftCatalogPackageApplicabilityRow>(sql)
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

    fn identity(
        &self,
        kind: HierarchyEntityKind,
        id: i64,
    ) -> Option<&AircraftCatalogCandidateIdentity> {
        self.identities_by_kind
            .get(&kind)
            .and_then(|identities| identities.get(&id))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ReviewableAircraftHierarchy {
    proposal: AircraftHierarchyProposal,
    adjudication: AircraftHierarchyAdjudication,
    verification: AircraftHierarchyVerification,
    server_faa_evidence: ServerFaaIdentityEvidence,
    direct_source_proofs: ServerFetchedAircraftSourceProofs,
}

impl ReviewableAircraftHierarchy {
    pub fn proposal(&self) -> &AircraftHierarchyProposal {
        &self.proposal
    }

    pub fn adjudication(&self) -> &AircraftHierarchyAdjudication {
        &self.adjudication
    }

    pub fn verification(&self) -> &AircraftHierarchyVerification {
        &self.verification
    }

    pub(crate) fn require_direct_source_claim_proof(
        &self,
        evidence_id: &str,
        claim: &EvidenceClaimProposal,
    ) -> Result<&str, String> {
        self.direct_source_proofs
            .require_exact_claim(evidence_id, claim)
            .map(|proof| proof.content_sha256.as_str())
    }

    /// Revalidate the exact server-created FAA case member before persistence.
    ///
    /// This prevents replaying a valid hierarchy decision against another
    /// listing that happens to have the same make/model text. The full imported
    /// grounding, listing id, retained-observation digest, and model year must
    /// all equal one member bound to the opaque reviewable value.
    pub(crate) fn require_server_faa_observation_binding(
        &self,
        listing_id: i64,
        observation_sha256: &str,
        listing_model_year: i64,
        grounding: &AircraftGrounding,
    ) -> Result<(), String> {
        self.server_faa_evidence.verify_observation_binding(
            listing_id,
            observation_sha256,
            listing_model_year,
            grounding,
        )
    }

    pub(crate) fn is_exact_server_faa_claim(
        &self,
        evidence_id: &str,
        claim: &EvidenceClaimProposal,
    ) -> bool {
        claim.evidence_id == evidence_id && self.server_faa_evidence.contains_exact_claim(claim)
    }

    pub(crate) fn server_evidence_source_content_sha256(&self, evidence_id: &str) -> Option<&str> {
        self.server_faa_evidence
            .drs_source_for_evidence_id(evidence_id)
            .map(|(_, pdf_sha256)| pdf_sha256)
    }

    pub(crate) fn require_tcds_family_relationship_binding(
        &self,
        listing_id: i64,
        observation: &AircraftIdentityObservation,
        grounding: &AircraftGrounding,
    ) -> Result<(), String> {
        if self.adjudication.family_label_relationship.action
            != FamilyLabelRelationshipAction::MatchFaaTypeCertificateFamily
        {
            return Ok(());
        }
        self.server_faa_evidence.verify_observation_binding(
            listing_id,
            &observation.observation_sha256,
            observation.model_year,
            grounding,
        )?;
        self.server_faa_evidence
            .validate_tcds_family_relationship(&self.adjudication.family_label_relationship)
    }
}

/// Server-fetched proof for one exact non-FAA claim selected by the final
/// aircraft decisions.
///
/// The fetched page body is deliberately absent. The transient generic proof
/// is retained only so later deterministic persistence validation can rerun
/// the shared excerpt matcher; serialization exposes only the final URL and
/// the two digests that bind the approval fingerprint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ServerFetchedAircraftClaimProof {
    final_url: String,
    content_sha256: String,
    normalized_span_sha256: String,
    #[serde(skip)]
    span_proof: SourceEvidenceSpanProof,
}

impl ServerFetchedAircraftClaimProof {
    fn exact_for_claim(&self, claim: &EvidenceClaimProposal) -> bool {
        self.final_url == claim.source_url
            && is_lower_hex_sha256(&self.content_sha256)
            && is_lower_hex_sha256(&self.normalized_span_sha256)
            && self.normalized_span_sha256 == self.span_proof.span_sha256
            && self.span_proof.matches_excerpt(&claim.evidence_excerpt)
    }
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Opaque server-owned mapping from selected evidence IDs to direct-source
/// fetch proofs. Only the filtered mapping attached to a reviewable approval
/// is serialized; unrelated research claims and all fetched page bodies are
/// discarded.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct ServerFetchedAircraftSourceProofs {
    by_evidence_id: BTreeMap<String, ServerFetchedAircraftClaimProof>,
}

impl ServerFetchedAircraftSourceProofs {
    pub(crate) fn bind_research(
        research: &AircraftIdentityEvidenceResearch,
        server_faa_evidence: &ServerFaaIdentityEvidence,
        fetched_proofs: &[SourceEvidenceProof],
    ) -> Result<Self, ValidationErrors> {
        let mut issues = Vec::new();
        let mut by_evidence_id = BTreeMap::new();
        for claim in &research.claims {
            if server_faa_evidence.contains_exact_claim(claim) {
                continue;
            }
            if claim.evidence_excerpt.contains("...")
                || claim.evidence_excerpt.contains('…')
                || claim.evidence_excerpt.contains("**")
                || claim.evidence_excerpt.contains("__")
                || claim.evidence_excerpt.contains('`')
                || claim.evidence_excerpt.contains("](")
            {
                issues.push(issue(
                    "direct_source_excerpt_not_verbatim",
                    format!(
                        "web evidence {} contains ellipsis or Markdown decoration instead of one contiguous publisher span",
                        claim.evidence_id
                    ),
                ));
                continue;
            }
            let matching_sources = fetched_proofs
                .iter()
                .filter(|proof| {
                    proof.final_url == claim.source_url
                        && proof.matches_excerpt(&claim.source_url, &claim.evidence_excerpt)
                })
                .collect::<Vec<_>>();
            let unique_content_digests = matching_sources
                .iter()
                .map(|proof| proof.content_sha256.as_str())
                .collect::<BTreeSet<_>>();
            if matching_sources.is_empty() {
                issues.push(issue(
                    "direct_source_proof_missing",
                    format!(
                        "web evidence {} has no exact server-fetched final-URL and excerpt proof",
                        claim.evidence_id
                    ),
                ));
                continue;
            }
            if unique_content_digests.len() != 1 {
                issues.push(issue(
                    "direct_source_proof_ambiguous",
                    format!(
                        "web evidence {} matched multiple fetched content digests",
                        claim.evidence_id
                    ),
                ));
                continue;
            }
            let source = matching_sources[0];
            if !is_lower_hex_sha256(&source.content_sha256) {
                issues.push(issue(
                    "direct_source_content_digest_invalid",
                    format!(
                        "web evidence {} has a malformed fetched content SHA-256",
                        claim.evidence_id
                    ),
                ));
                continue;
            }
            let matching_spans = source
                .evidence_spans
                .iter()
                .filter(|span| span.matches_excerpt(&claim.evidence_excerpt))
                .collect::<Vec<_>>();
            let unique_span_digests = matching_spans
                .iter()
                .map(|span| span.span_sha256.as_str())
                .collect::<BTreeSet<_>>();
            if unique_span_digests.len() != 1 {
                issues.push(issue(
                    "direct_source_span_proof_ambiguous",
                    format!(
                        "web evidence {} did not resolve to one normalized fetched-page span digest",
                        claim.evidence_id
                    ),
                ));
                continue;
            }
            let span = matching_spans[0];
            if !is_lower_hex_sha256(&span.span_sha256) {
                issues.push(issue(
                    "direct_source_span_digest_invalid",
                    format!(
                        "web evidence {} has a malformed normalized-span SHA-256",
                        claim.evidence_id
                    ),
                ));
                continue;
            }
            if by_evidence_id
                .insert(
                    claim.evidence_id.clone(),
                    ServerFetchedAircraftClaimProof {
                        final_url: source.final_url.clone(),
                        content_sha256: source.content_sha256.clone(),
                        normalized_span_sha256: span.span_sha256.clone(),
                        span_proof: span.clone(),
                    },
                )
                .is_some()
            {
                issues.push(issue(
                    "direct_source_proof_duplicate_evidence_id",
                    format!(
                        "web source proof mapping repeats evidence id {}",
                        claim.evidence_id
                    ),
                ));
            }
        }
        if issues.is_empty() {
            Ok(Self { by_evidence_id })
        } else {
            Err(ValidationErrors::from_unsorted(issues))
        }
    }

    fn require_exact_claim(
        &self,
        evidence_id: &str,
        claim: &EvidenceClaimProposal,
    ) -> Result<&ServerFetchedAircraftClaimProof, String> {
        let proof = self.by_evidence_id.get(evidence_id).ok_or_else(|| {
            format!("used web evidence {evidence_id} has no bound direct-source proof")
        })?;
        if !proof.exact_for_claim(claim) {
            return Err(format!(
                "used web evidence {evidence_id} no longer matches its final URL or normalized source span proof"
            ));
        }
        Ok(proof)
    }

    fn for_used_decisions(
        &self,
        research: &AircraftIdentityEvidenceResearch,
        server_faa_evidence: &ServerFaaIdentityEvidence,
        evidence_ids: &BTreeSet<&str>,
    ) -> Result<Self, ValidationErrors> {
        let claims = research
            .claims
            .iter()
            .map(|claim| (claim.evidence_id.as_str(), claim))
            .collect::<BTreeMap<_, _>>();
        let mut issues = Vec::new();
        let mut by_evidence_id = BTreeMap::new();
        for evidence_id in evidence_ids {
            let Some(claim) = claims.get(evidence_id).copied() else {
                issues.push(issue(
                    "direct_source_proof_unknown_evidence",
                    format!("used evidence id {evidence_id} is absent from the research claims"),
                ));
                continue;
            };
            if server_faa_evidence.contains_exact_claim(claim) {
                continue;
            }
            match self.require_exact_claim(evidence_id, claim) {
                Ok(proof) => {
                    by_evidence_id.insert((*evidence_id).to_string(), proof.clone());
                }
                Err(message) => issues.push(issue("direct_source_proof_mismatch", message)),
            }
        }
        if issues.is_empty() {
            Ok(Self { by_evidence_id })
        } else {
            Err(ValidationErrors::from_unsorted(issues))
        }
    }
}

pub fn validate_identity_evidence_research(
    research: &AircraftIdentityEvidenceResearch,
    grounding: &GroundingAudit,
    server_faa_evidence: &ServerFaaIdentityEvidence,
) -> Result<(), ValidationErrors> {
    let mut issues = Vec::new();
    if research
        .contradictions
        .iter()
        .any(|value| !value.trim().is_empty())
    {
        issues.push(issue(
            "research_contradictions_present",
            "the evidence dossier reports unresolved contradictions",
        ));
    }
    if research
        .unresolved_questions
        .iter()
        .any(|item| item.question.trim().is_empty())
    {
        issues.push(issue(
            "research_unresolved_question_missing_text",
            "every typed unresolved research item must retain its question text",
        ));
    }
    if research
        .unresolved_questions
        .iter()
        .any(|item| !item.question.trim().is_empty())
    {
        issues.push(issue(
            "research_unresolved_questions_present",
            "the evidence dossier reports unresolved questions",
        ));
    }
    if let Some(binding) = server_faa_evidence.tcds_family_binding.as_ref() {
        for candidate in &research.family_candidates {
            let label = candidate.label.trim();
            if !label.is_empty() && label != binding.canonical_family_name {
                issues.push(issue(
                    "tcds_family_candidate_conflict",
                    format!(
                        "family candidate {label:?} conflicts with exact current-TCDS family {:?} for this FAA-matched serial",
                        binding.canonical_family_name
                    ),
                ));
            }
        }
    }
    let regulator_complete =
        regulator_complete_grounding_matches(research, grounding, server_faa_evidence);
    if grounding.mode == GroundingMode::RegulatorComplete && !regulator_complete {
        issues.push(issue(
            "regulator_complete_grounding_invalid",
            "regulator-complete mode did not exactly recompute from this server FAA registry/TCDS case and deterministic server-only research bundle",
        ));
    }
    let exact_reused_dossier = grounding.mode == GroundingMode::ReusedVerifiedDossier
        && grounding.reused_verified_dossier
        && grounding.google_search_call_count == 0
        && grounding.url_context_call_count == 0
        && !grounding.citation_urls.is_empty();
    let fresh_web = grounding.mode == GroundingMode::FreshWeb
        && !grounding.reused_verified_dossier
        && grounding.google_search_call_count > 0
        && grounding.url_context_call_count > 0;
    if !fresh_web && !exact_reused_dossier && !regulator_complete {
        issues.push(issue(
            "google_search_not_observed",
            "the evidence pass neither executed Google Search in fresh-web mode, reused an exact verified dossier, nor recomputed a complete regulator-only case",
        ));
    }
    if !fresh_web && !exact_reused_dossier && !regulator_complete {
        issues.push(issue(
            "url_context_not_observed",
            "the evidence pass neither inspected selected sources with URL Context in fresh-web mode, reused an exact verified dossier, nor recomputed a complete regulator-only case",
        ));
    }
    if research.claims.is_empty() {
        issues.push(issue(
            "missing_evidence_claims",
            "the evidence pass returned no claims",
        ));
    }
    for expected in server_faa_evidence.claims() {
        if !research.claims.iter().any(|claim| claim == expected) {
            issues.push(issue(
                "missing_server_faa_evidence",
                format!(
                    "the exact case-bound server FAA claim {} was not attached",
                    expected.evidence_id
                ),
            ));
        }
    }
    let mut evidence_ids = BTreeSet::new();
    for (index, claim) in research.claims.iter().enumerate() {
        if !evidence_ids.insert(claim.evidence_id.trim()) {
            issues.push(issue(
                "duplicate_evidence_id",
                format!("evidence claim {index} reuses id {}", claim.evidence_id),
            ));
        }
        let is_exact_server_claim = server_faa_evidence.contains_exact_claim(claim);
        if ServerFaaIdentityEvidence::is_reserved_id(claim.evidence_id.trim())
            && !is_exact_server_claim
        {
            issues.push(issue(
                "forged_server_faa_evidence",
                format!(
                    "evidence claim {} resembles a server FAA claim but does not exactly match this bound case",
                    claim.evidence_id
                ),
            ));
        } else if !is_exact_server_claim
            && !citation_matches(&grounding.citation_urls, &claim.source_url)
        {
            issues.push(issue(
                "uncited_evidence_url",
                format!(
                    "evidence claim {} uses a URL absent from model-output citations",
                    claim.evidence_id
                ),
            ));
        }
        if !is_exact_server_claim
            && claim.source_kind.is_primary()
            && is_obvious_secondary_or_mirror_source_url(&claim.source_url)
        {
            issues.push(issue(
                "third_party_source_mislabeled_primary",
                format!(
                    "evidence claim {} labels an obvious secondary, mirror, or marketplace host as a primary source",
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
    validate_hierarchy_candidates(
        HierarchyEntityKind::Family,
        &research.family_candidates,
        research,
        server_faa_evidence,
        &mut issues,
    );
    validate_hierarchy_candidates(
        HierarchyEntityKind::Generation,
        &research.generation_candidates,
        research,
        server_faa_evidence,
        &mut issues,
    );
    validate_hierarchy_candidates(
        HierarchyEntityKind::Package,
        &research.package_candidates,
        research,
        server_faa_evidence,
        &mut issues,
    );
    validation_result(issues)
}

fn regulator_complete_grounding_matches(
    research: &AircraftIdentityEvidenceResearch,
    grounding: &GroundingAudit,
    server_faa_evidence: &ServerFaaIdentityEvidence,
) -> bool {
    grounding.mode == GroundingMode::RegulatorComplete
        && !grounding.reused_verified_dossier
        && grounding.google_search_call_count == 0
        && grounding.url_context_call_count == 0
        && grounding.citation_urls.is_empty()
        && server_faa_evidence.regulator_complete_research().as_ref() == Some(research)
}

fn validate_hierarchy_candidates(
    kind: HierarchyEntityKind,
    candidates: &[HierarchyCandidate],
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    issues: &mut Vec<ValidationIssue>,
) {
    let mut labels = BTreeSet::new();
    for candidate in candidates {
        let label = candidate.label.trim();
        if label.is_empty() {
            issues.push(issue(
                "hierarchy_candidate_missing_label",
                format!("{} research candidate has no exact label", kind.as_str()),
            ));
        } else if !labels.insert(label.to_string()) {
            issues.push(issue(
                "duplicate_hierarchy_candidate",
                format!(
                    "{} research repeats exact candidate {label:?}",
                    kind.as_str()
                ),
            ));
        }
        if kind == HierarchyEntityKind::Family && !label.is_empty() {
            let exact_label_occurs_in_primary_evidence =
                candidate.evidence_ids.iter().any(|evidence_id| {
                    research.claims.iter().any(|claim| {
                        claim.evidence_id == *evidence_id
                            && (is_web_aircraft_hierarchy_claim(
                                research,
                                server_faa_evidence,
                                evidence_id,
                            ) || is_server_drs_named_family_hierarchy_claim(
                                research,
                                server_faa_evidence,
                                evidence_id,
                            ))
                            && contains_exact_contiguous_label(&claim.evidence_excerpt, label)
                    })
                });
            if !exact_label_occurs_in_primary_evidence {
                issues.push(issue(
                    "family_candidate_label_absent_from_primary_evidence",
                    format!(
                        "family candidate {label:?} must occur exactly in one cited direct-primary hierarchy excerpt"
                    ),
                ));
            }
            if let Some(forbidden_component) =
                family_candidate_forbidden_component(label, server_faa_evidence)
            {
                issues.push(issue(
                    "family_candidate_not_exact_oem_component",
                    format!(
                        "family candidate {label:?} is a composite containing {forbidden_component}; retain only the exact OEM family-name component"
                    ),
                ));
            }
        }
        if candidate.evidence_ids.is_empty() {
            issues.push(issue(
                "hierarchy_candidate_missing_primary_evidence",
                format!(
                    "{} candidate {label:?} lacks direct primary hierarchy evidence",
                    kind.as_str()
                ),
            ));
        }
        for evidence_id in &candidate.evidence_ids {
            let evidence_claim = research
                .claims
                .iter()
                .find(|claim| claim.evidence_id == *evidence_id);
            let evidence_is_primary_hierarchy =
                is_web_aircraft_hierarchy_claim(research, server_faa_evidence, evidence_id)
                    || (kind == HierarchyEntityKind::Family
                        && is_server_drs_named_family_hierarchy_claim(
                            research,
                            server_faa_evidence,
                            evidence_id,
                        ));
            if evidence_claim.is_none() {
                issues.push(issue(
                    "hierarchy_candidate_unknown_evidence",
                    format!(
                        "{} candidate {label:?} references unknown evidence id {evidence_id}",
                        kind.as_str()
                    ),
                ));
            } else if !evidence_is_primary_hierarchy {
                issues.push(issue(
                    "hierarchy_candidate_non_primary_evidence",
                    format!(
                        "{} candidate {label:?} evidence id {evidence_id} is not a direct-primary exact hierarchy claim",
                        kind.as_str()
                    ),
                ));
            } else if matches!(
                kind,
                HierarchyEntityKind::Generation | HierarchyEntityKind::Package
            ) && evidence_claim.is_some_and(|claim| {
                !contains_exact_contiguous_label(&claim.evidence_excerpt, label)
            }) {
                issues.push(issue(
                    "hierarchy_candidate_evidence_label_mismatch",
                    format!(
                        "{} candidate {label:?} evidence id {evidence_id} does not exact-name that candidate in its direct-primary hierarchy excerpt",
                        kind.as_str()
                    ),
                ));
            }
        }
    }
}

fn family_candidate_forbidden_component(
    candidate_label: &str,
    server_faa_evidence: &ServerFaaIdentityEvidence,
) -> Option<String> {
    let has_distinct_family_hint = server_has_distinct_family_name_hint(server_faa_evidence);
    for retained_make in server_faa_evidence
        .observation_bindings
        .iter()
        .map(|binding| binding.observed_make.trim())
        .filter(|label| !label.is_empty())
    {
        if contains_forbidden_composite_component(candidate_label, retained_make) {
            return Some(format!("retained make {retained_make:?}"));
        }
    }
    let faa_designation = server_faa_evidence.faa_model_designation();
    if contains_forbidden_composite_component(candidate_label, faa_designation)
        || (has_distinct_family_hint && exact_token_label(candidate_label, faa_designation))
    {
        return Some(format!("FAA designation {faa_designation:?}"));
    }
    for retained_numeric_label in server_faa_evidence
        .observation_bindings
        .iter()
        .map(|binding| binding.observed_model.trim())
        .filter(|label| retained_family_label_is_numeric(label))
    {
        if contains_forbidden_composite_component(candidate_label, retained_numeric_label)
            || (has_distinct_family_hint
                && exact_token_label(candidate_label, retained_numeric_label))
        {
            return Some(format!(
                "retained numeric family label {retained_numeric_label:?}"
            ));
        }
    }
    None
}

fn server_has_distinct_family_name_hint(server_faa_evidence: &ServerFaaIdentityEvidence) -> bool {
    if server_faa_evidence
        .tcds_family_binding
        .as_ref()
        .is_some_and(|binding| {
            !exact_token_label(
                &binding.canonical_family_name,
                server_faa_evidence.faa_model_designation(),
            )
        })
    {
        return true;
    }
    let designation_tokens = alphanumeric_tokens(server_faa_evidence.faa_model_designation())
        .into_iter()
        .collect::<BTreeSet<_>>();
    server_faa_evidence
        .observation_bindings
        .iter()
        .flat_map(|binding| {
            [
                binding.observed_model.as_str(),
                binding.observed_variant.as_str(),
            ]
            .into_iter()
            .flat_map(|label| {
                label
                    .split(|character: char| !character.is_ascii_alphanumeric())
                    .filter(|token| !token.is_empty())
            })
        })
        .any(|token| {
            !designation_tokens.contains(&token.to_lowercase())
                && token.len() >= 3
                && token
                    .chars()
                    .all(|character| character.is_ascii_alphabetic())
                && !token
                    .chars()
                    .all(|character| character.is_ascii_uppercase())
        })
}

fn exact_token_label(left: &str, right: &str) -> bool {
    let left = alphanumeric_tokens(left);
    !left.is_empty() && left == alphanumeric_tokens(right)
}

fn contains_forbidden_composite_component(candidate_label: &str, component: &str) -> bool {
    let candidate_tokens = alphanumeric_tokens(candidate_label);
    let component_tokens = alphanumeric_tokens(component);
    candidate_tokens.len() > component_tokens.len()
        && !component_tokens.is_empty()
        && candidate_tokens
            .windows(component_tokens.len())
            .any(|window| window == component_tokens)
}

fn retained_family_label_is_numeric(value: &str) -> bool {
    let tokens = alphanumeric_tokens(value);
    !tokens.is_empty()
        && tokens
            .iter()
            .all(|token| token.chars().all(|character| character.is_ascii_digit()))
}

fn is_faa_source_url(value: &str) -> bool {
    Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "faa.gov" || host.ends_with(".faa.gov"))
}

/// Hosts whose role is intrinsically secondary for aircraft identity.
///
/// This is intentionally a conservative deny-list, not an OEM allow-list.
/// Unknown hosts still have to survive the grounded source-classification and
/// independent verification passes. These well-known encyclopedias, document
/// mirrors, and aircraft marketplaces can provide search leads, but a copied
/// OEM document does not become first-party evidence because its contents were
/// originally authored by an OEM.
fn is_obvious_secondary_or_mirror_source_url(value: &str) -> bool {
    const NON_ORIGINAL_PUBLISHER_HOSTS: &[&str] = &[
        "aircraft.com",
        "airmart.com",
        "archive.org",
        "avbuyer.com",
        "controller.com",
        "docslib.org",
        "emanualonline.com",
        "globalair.com",
        "issuu.com",
        "manuals.plus",
        "manualslib.com",
        "manualzz.com",
        "pdfcoffee.com",
        "scribd.com",
        "slideshare.net",
        "studylib.net",
        "trade-a-plane.com",
        "wikimedia.org",
        "wikipedia.org",
        "yumpu.com",
    ];

    Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| {
            NON_ORIGINAL_PUBLISHER_HOSTS
                .iter()
                .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
        })
}

pub(super) fn validate_aircraft_hierarchy_adjudication(
    research: &AircraftIdentityEvidenceResearch,
    evidence_grounding: &GroundingAudit,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    adjudication: &AircraftHierarchyAdjudication,
    catalog_candidates: &CatalogCandidateRegistry,
    catalog_function_call_count: usize,
) -> Result<AircraftHierarchyProposal, ValidationErrors> {
    let mut issues = Vec::new();
    if let Err(errors) =
        validate_identity_evidence_research(research, evidence_grounding, server_faa_evidence)
    {
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
    if !adjudication.unresolved_questions.is_empty() {
        issues.push(issue(
            "adjudication_unresolved_questions_present",
            "the hierarchy adjudication reports unresolved questions",
        ));
    }

    let make = resolved_entity(
        HierarchyEntityKind::Make,
        &adjudication.make,
        false,
        catalog_candidates,
        research,
        server_faa_evidence,
        evidence_grounding,
        &adjudication.designation,
        &adjudication.generation,
        None,
        None,
        &mut issues,
    );
    validate_faa_make_relationship(
        &adjudication.faa_make_relationship,
        &adjudication.make,
        research,
        server_faa_evidence,
        catalog_candidates,
        &mut issues,
    );
    let family = resolved_entity(
        HierarchyEntityKind::Family,
        &adjudication.family,
        false,
        catalog_candidates,
        research,
        server_faa_evidence,
        evidence_grounding,
        &adjudication.designation,
        &adjudication.generation,
        None,
        None,
        &mut issues,
    );
    validate_existing_family_candidate_binding(
        &adjudication.make,
        &adjudication.family,
        catalog_candidates,
        &mut issues,
    );
    let family_label_relationship_valid = validate_family_label_relationship(
        &adjudication.family_label_relationship,
        &adjudication.make,
        &adjudication.family,
        research,
        server_faa_evidence,
        catalog_candidates,
        &mut issues,
    );
    let designation = resolved_entity(
        HierarchyEntityKind::Designation,
        &adjudication.designation,
        false,
        catalog_candidates,
        research,
        server_faa_evidence,
        evidence_grounding,
        &adjudication.designation,
        &adjudication.generation,
        None,
        None,
        &mut issues,
    );
    validate_existing_designation_candidate_binding(
        &adjudication.family,
        &adjudication.designation,
        catalog_candidates,
        server_faa_evidence,
        &mut issues,
    );
    // These labels may account for literal listing tokens only after the
    // corresponding required dimensions have gone through the positive
    // resolution path. If either required decision is invalid, its existing
    // validation issue still rejects the complete proposal.
    let resolved_make_label = make.as_ref().map(|entity| entity.display_name.as_str());
    let resolved_family_label = family.as_ref().and_then(|entity| {
        decision_has_exact_typed_candidate(
            HierarchyEntityKind::Family,
            &adjudication.family,
            research,
            server_faa_evidence,
        )
        .then_some(entity.display_name.as_str())
    });
    // FAA TCDS family binding and manufacturer series/family composition both
    // preserve the complete retained label as audit input. Neither may consume
    // that label wholesale: its exact family component and proof-gated numeric
    // series stem are accounted independently so every extra token remains
    // live for generation/package validation.
    let resolved_observed_family_label = (family_label_relationship_valid
        && resolved_family_label.is_some()
        && !matches!(
            adjudication.family_label_relationship.action,
            FamilyLabelRelationshipAction::MatchFaaTypeCertificateFamily
                | FamilyLabelRelationshipAction::MatchManufacturerSeriesFamily
        ))
    .then_some(
        adjudication
            .family_label_relationship
            .observed_family_label
            .trim(),
    );
    // Resolve every positive optional dimension before deciding whether the
    // other dimension may remain NULL. A positively validated G6 generation,
    // for example, accounts for the G6 token while package liveness is checked.
    let generation = (adjudication.generation.action
        != EntityResolutionAction::NoSupportedSelection)
        .then(|| {
            resolved_entity(
                HierarchyEntityKind::Generation,
                &adjudication.generation,
                true,
                catalog_candidates,
                research,
                server_faa_evidence,
                evidence_grounding,
                &adjudication.designation,
                &adjudication.generation,
                resolved_make_label,
                resolved_family_label,
                &mut issues,
            )
        })
        .flatten();
    let tier = (adjudication.package.action != EntityResolutionAction::NoSupportedSelection)
        .then(|| {
            resolved_entity(
                HierarchyEntityKind::Package,
                &adjudication.package,
                true,
                catalog_candidates,
                research,
                server_faa_evidence,
                evidence_grounding,
                &adjudication.designation,
                &adjudication.generation,
                resolved_make_label,
                resolved_family_label,
                &mut issues,
            )
        })
        .flatten();
    let resolved_generation_label = generation.as_ref().and_then(|entity| {
        decision_has_exact_typed_candidate(
            HierarchyEntityKind::Generation,
            &adjudication.generation,
            research,
            server_faa_evidence,
        )
        .then_some(entity.display_name.as_str())
    });
    let resolved_package_label = tier.as_ref().and_then(|entity| {
        decision_has_exact_typed_candidate(
            HierarchyEntityKind::Package,
            &adjudication.package,
            research,
            server_faa_evidence,
        )
        .then_some(entity.display_name.as_str())
    });
    if adjudication.generation.action == EntityResolutionAction::NoSupportedSelection {
        validate_no_supported_selection(
            HierarchyEntityKind::Generation,
            &adjudication.generation,
            true,
            catalog_candidates,
            research,
            server_faa_evidence,
            evidence_grounding,
            &adjudication.designation,
            &adjudication.generation,
            resolved_make_label,
            resolved_family_label,
            resolved_observed_family_label,
            resolved_generation_label,
            resolved_package_label,
            &mut issues,
        );
    }
    if adjudication.package.action == EntityResolutionAction::NoSupportedSelection {
        validate_no_supported_selection(
            HierarchyEntityKind::Package,
            &adjudication.package,
            true,
            catalog_candidates,
            research,
            server_faa_evidence,
            evidence_grounding,
            &adjudication.designation,
            &adjudication.generation,
            resolved_make_label,
            resolved_family_label,
            resolved_observed_family_label,
            resolved_generation_label,
            resolved_package_label,
            &mut issues,
        );
    }

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
    server_faa_evidence: &ServerFaaIdentityEvidence,
    direct_source_proofs: &ServerFetchedAircraftSourceProofs,
    adjudication: AircraftHierarchyAdjudication,
    catalog_candidates: &CatalogCandidateRegistry,
    catalog_function_call_count: usize,
    verification: AircraftHierarchyVerification,
    verification_grounding: &GroundingAudit,
    server_faa_only_verification: bool,
) -> Result<ReviewableAircraftHierarchy, ValidationErrors> {
    let proposal = validate_aircraft_hierarchy_adjudication(
        research,
        evidence_grounding,
        server_faa_evidence,
        &adjudication,
        catalog_candidates,
        catalog_function_call_count,
    )?;
    let mut issues = Vec::new();

    let verification_evidence_is_grounded = if server_faa_only_verification {
        server_faa_only_verification_evidence_ids(research, server_faa_evidence, &adjudication)
            .is_some()
    } else {
        (verification_grounding.mode == GroundingMode::FreshWeb
            && !verification_grounding.reused_verified_dossier
            && verification_grounding.google_search_call_count > 0
            && verification_grounding.url_context_call_count > 0)
            || (verification_grounding.mode == GroundingMode::ReusedVerifiedDossier
                && verification_grounding.reused_verified_dossier
                && verification_grounding.google_search_call_count == 0
                && verification_grounding.url_context_call_count == 0
                && !verification_grounding.citation_urls.is_empty()
                && verification_grounding.citation_urls == evidence_grounding.citation_urls)
    };
    if !verification_evidence_is_grounded {
        issues.push(issue(
            "verifier_grounding_not_observed",
            "the independent verifier has neither fresh Search/URL Context grounding, an exact reused verified dossier, nor a complete selected server FAA/TCDS evidence scope",
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
    if !verification.errors.is_empty() {
        issues.push(issue(
            "independent_verifier_errors_present",
            "the independent verifier reports unresolved errors",
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
    let adjudication_evidence_ids = adjudication_evidence_ids(&adjudication);
    let selected_direct_source_proofs = match direct_source_proofs.for_used_decisions(
        research,
        server_faa_evidence,
        &adjudication_evidence_ids,
    ) {
        Ok(proofs) => Some(proofs),
        Err(errors) => {
            issues.extend(errors.0);
            None
        }
    };
    let verified_ids = verification
        .verified_evidence_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for id in adjudication_evidence_ids.difference(&verified_ids) {
        issues.push(issue(
            "verifier_missing_adjudication_evidence",
            format!("the independent verifier did not affirm adjudication evidence id {id}"),
        ));
    }
    for required_id in [
        server_faa_evidence.make_claim_id(),
        server_faa_evidence.designation_claim_id(),
    ] {
        if !verification
            .verified_evidence_ids
            .iter()
            .any(|id| id == required_id)
        {
            issues.push(issue(
                "verifier_missing_server_faa_identity",
                format!(
                    "the independent verifier did not affirm bound server FAA evidence id {required_id}"
                ),
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
        server_faa_evidence: server_faa_evidence.clone(),
        direct_source_proofs: selected_direct_source_proofs
            .expect("direct source proof selection validated"),
    })
}

fn adjudication_evidence_ids(adjudication: &AircraftHierarchyAdjudication) -> BTreeSet<&str> {
    [
        &adjudication.make,
        &adjudication.family,
        &adjudication.designation,
        &adjudication.generation,
        &adjudication.package,
    ]
    .into_iter()
    .flat_map(|decision| decision.evidence_ids.iter().map(String::as_str))
    .chain(
        adjudication
            .faa_make_relationship
            .evidence_ids
            .iter()
            .map(String::as_str),
    )
    .chain(
        adjudication
            .faa_make_relationship
            .applicability_evidence_ids
            .iter()
            .map(String::as_str),
    )
    .chain(
        adjudication
            .family_label_relationship
            .evidence_ids
            .iter()
            .map(String::as_str),
    )
    .chain(
        adjudication
            .family_label_relationship
            .applicability_evidence_ids
            .iter()
            .map(String::as_str),
    )
    .collect()
}

/// Return the exact selected evidence scope when a fresh verifier can safely
/// audit only server-created FAA registry/TCDS claims.
///
/// This is deliberately stricter than merely checking an evidence-ID prefix.
/// The exact designation/serial and named-family bindings must exist, their
/// deterministic TCDS selection basis must be present, every selected claim
/// must equal a claim in this case-bound server bundle, and no optional
/// hierarchy selection or unresolved/contradictory state may remain. Any web
/// evidence selected by adjudication keeps the ordinary grounded-dossier path.
pub(crate) fn server_faa_only_verification_evidence_ids(
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    adjudication: &AircraftHierarchyAdjudication,
) -> Option<BTreeSet<String>> {
    if server_faa_evidence.tcds_selection_basis.is_none()
        || server_faa_evidence.tcds_identity_binding.is_none()
        || server_faa_evidence.tcds_family_binding.is_none()
        || !research.contradictions.is_empty()
        || !research.unresolved_questions.is_empty()
        || !adjudication.unresolved_questions.is_empty()
        || adjudication.confidence != CurationConfidence::VeryHigh
        || adjudication.family_label_relationship.action
            != FamilyLabelRelationshipAction::MatchFaaTypeCertificateFamily
        || !matches!(
            adjudication.faa_make_relationship.action,
            FaaMakeRelationshipAction::ExactCanonicalLabel
                | FaaMakeRelationshipAction::MatchTcdsMakeLineage
        )
        || adjudication.generation.action != EntityResolutionAction::NoSupportedSelection
        || adjudication.package.action != EntityResolutionAction::NoSupportedSelection
    {
        return None;
    }

    let selected = adjudication_evidence_ids(adjudication)
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if selected.is_empty()
        || selected.iter().any(|evidence_id| {
            !evidence_id.starts_with(SERVER_FAA_REGISTRY_EVIDENCE_ID_PREFIX)
                && !evidence_id.starts_with(SERVER_FAA_DRS_EVIDENCE_ID_PREFIX)
        })
    {
        return None;
    }
    let exact_server_claims = server_faa_evidence
        .claims()
        .iter()
        .map(|claim| claim.evidence_id.as_str())
        .collect::<BTreeSet<_>>();
    if selected
        .iter()
        .any(|evidence_id| !exact_server_claims.contains(evidence_id.as_str()))
    {
        return None;
    }

    let identity_claim_ids = server_faa_evidence.tcds_identity_claim_ids()?;
    let identity_claims = identity_claim_ids.all();
    if !identity_claims
        .iter()
        .all(|evidence_id| selected.contains(*evidence_id))
        || !selected.contains(server_faa_evidence.make_claim_id())
        || !selected.contains(server_faa_evidence.designation_claim_id())
    {
        return None;
    }
    Some(selected)
}

fn resolved_entity(
    kind: HierarchyEntityKind,
    decision: &CatalogEntityDecision,
    optional: bool,
    candidates: &CatalogCandidateRegistry,
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    evidence_grounding: &GroundingAudit,
    designation_decision: &CatalogEntityDecision,
    generation_decision: &CatalogEntityDecision,
    resolved_make_label: Option<&str>,
    resolved_family_label: Option<&str>,
    issues: &mut Vec<ValidationIssue>,
) -> Option<CatalogEntityProposal> {
    if decision.action == EntityResolutionAction::NoSupportedSelection {
        validate_no_supported_selection(
            kind,
            decision,
            optional,
            candidates,
            research,
            server_faa_evidence,
            evidence_grounding,
            designation_decision,
            generation_decision,
            resolved_make_label,
            resolved_family_label,
            None,
            None,
            None,
            issues,
        );
        return None;
    }
    let known_evidence = research
        .claims
        .iter()
        .map(|claim| claim.evidence_id.as_str())
        .collect::<BTreeSet<_>>();
    if decision.evidence_ids.is_empty() {
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
    match kind {
        HierarchyEntityKind::Make => {
            if !decision
                .evidence_ids
                .iter()
                .any(|id| id == server_faa_evidence.make_claim_id())
            {
                issues.push(issue(
                    "missing_server_faa_make_evidence",
                    "make decision must cite the exact server-created FAA make claim",
                ));
            }
        }
        HierarchyEntityKind::Designation => {
            if !decision
                .evidence_ids
                .iter()
                .any(|id| id == server_faa_evidence.designation_claim_id())
            {
                issues.push(issue(
                    "missing_server_faa_designation_evidence",
                    "designation decision must cite the exact server-created FAA model claim",
                ));
            }
            let selected_designator = decision.authoritative_designator.as_deref().map(str::trim);
            if selected_designator != Some(server_faa_evidence.faa_model_designation()) {
                issues.push(issue(
                    "server_faa_designation_mismatch",
                    format!(
                        "designation must preserve exact FAA model {:?}, received {:?}",
                        server_faa_evidence.faa_model_designation(),
                        selected_designator
                    ),
                ));
            }
            if decision.action == EntityResolutionAction::ProposeNew
                && decision.display_name.as_deref().map(str::trim)
                    != Some(server_faa_evidence.faa_model_designation())
            {
                issues.push(issue(
                    "new_designation_display_name_mismatch",
                    format!(
                        "a new designation display name must literally preserve exact FAA model {:?}, received {:?}",
                        server_faa_evidence.faa_model_designation(),
                        decision.display_name.as_deref().map(str::trim)
                    ),
                ));
            }
        }
        HierarchyEntityKind::Family => {
            if !decision.evidence_ids.iter().any(|id| {
                is_web_aircraft_hierarchy_claim(research, server_faa_evidence, id)
                    || is_server_drs_named_family_hierarchy_claim(research, server_faa_evidence, id)
            }) {
                issues.push(issue(
                    "missing_primary_identity_evidence",
                    "family decision requires exact direct-primary OEM/AFM evidence or an exact case-bound current FAA TCDS family claim",
                ));
            }
        }
        HierarchyEntityKind::Generation | HierarchyEntityKind::Package => {
            if !decision
                .evidence_ids
                .iter()
                .any(|id| is_web_aircraft_hierarchy_claim(research, server_faa_evidence, id))
            {
                issues.push(issue(
                    "missing_web_identity_evidence",
                    format!(
                        "{} decision requires direct primary web evidence; FAA registry/TCDS evidence is insufficient",
                        kind.as_str()
                    ),
                ));
            }
        }
    }
    if matches!(
        kind,
        HierarchyEntityKind::Family
            | HierarchyEntityKind::Generation
            | HierarchyEntityKind::Package
    ) && matches!(
        decision.action,
        EntityResolutionAction::MatchExisting | EntityResolutionAction::ProposeNew
    ) && !decision_has_exact_typed_candidate(kind, decision, research, server_faa_evidence)
    {
        issues.push(issue(
            "entity_missing_exact_typed_candidate",
            format!(
                "{} positive decision must exactly match a direct-primary typed research candidate and cite its evidence",
                kind.as_str()
            ),
        ));
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
        EntityResolutionAction::NoSupportedSelection if optional => {
            unreachable!("no-supported selection handled before positive-evidence validation")
        }
        EntityResolutionAction::NoSupportedSelection => {
            issues.push(issue(
                "required_entity_no_supported_selection",
                format!("{} cannot use no_supported_selection", kind.as_str()),
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

fn validate_existing_family_candidate_binding(
    make: &CatalogEntityDecision,
    family: &CatalogEntityDecision,
    candidates: &CatalogCandidateRegistry,
    issues: &mut Vec<ValidationIssue>,
) {
    if family.action != EntityResolutionAction::MatchExisting {
        return;
    }
    let Some(family_id) = family.existing_catalog_id else {
        // `resolved_entity` reports the missing required ID.
        return;
    };
    let Some(candidate) = candidates.identity(HierarchyEntityKind::Family, family_id) else {
        issues.push(issue(
            "family_catalog_candidate_identity_not_retrieved",
            format!(
                "family catalog id {family_id} lacks the exact family identity returned by live catalog search"
            ),
        ));
        return;
    };
    if candidate.entity_kind != HierarchyEntityKind::Family || candidate.catalog_id != family_id {
        issues.push(issue(
            "family_catalog_candidate_kind_mismatch",
            format!(
                "family catalog id {family_id} was returned as {:?} id {}",
                candidate.entity_kind, candidate.catalog_id
            ),
        ));
    }
    if family.display_name.as_deref().map(str::trim) != Some(candidate.display_name.as_str()) {
        issues.push(issue(
            "family_catalog_candidate_label_mismatch",
            format!(
                "matched family id {family_id} must preserve returned display label {:?}",
                candidate.display_name
            ),
        ));
    }
    let selected_make_id = (make.action == EntityResolutionAction::MatchExisting)
        .then_some(make.existing_catalog_id)
        .flatten();
    if candidate.parent_catalog_id != selected_make_id {
        issues.push(issue(
            "family_catalog_candidate_parent_mismatch",
            format!(
                "matched family id {family_id} belongs to make {:?}, but the selected existing make is {selected_make_id:?}",
                candidate.parent_catalog_id
            ),
        ));
    }
}

fn validate_existing_designation_candidate_binding(
    family: &CatalogEntityDecision,
    designation: &CatalogEntityDecision,
    candidates: &CatalogCandidateRegistry,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    issues: &mut Vec<ValidationIssue>,
) {
    if designation.action != EntityResolutionAction::MatchExisting {
        return;
    }
    let Some(designation_id) = designation.existing_catalog_id else {
        // `resolved_entity` reports the missing required ID.
        return;
    };
    let Some(candidate) = candidates.identity(HierarchyEntityKind::Designation, designation_id)
    else {
        issues.push(issue(
            "designation_catalog_candidate_identity_not_retrieved",
            format!(
                "designation catalog id {designation_id} lacks the exact designation identity returned by live catalog search"
            ),
        ));
        return;
    };
    if candidate.entity_kind != HierarchyEntityKind::Designation
        || candidate.catalog_id != designation_id
    {
        issues.push(issue(
            "designation_catalog_candidate_kind_mismatch",
            format!(
                "designation catalog id {designation_id} was returned as {:?} id {}",
                candidate.entity_kind, candidate.catalog_id
            ),
        ));
    }
    if designation.display_name.as_deref().map(str::trim) != Some(candidate.display_name.as_str()) {
        issues.push(issue(
            "designation_catalog_candidate_label_mismatch",
            format!(
                "matched designation id {designation_id} must preserve returned display label {:?}",
                candidate.display_name
            ),
        ));
    }
    if candidate.authoritative_designator.as_deref().map(str::trim)
        != Some(server_faa_evidence.faa_model_designation())
    {
        issues.push(issue(
            "designation_catalog_authoritative_designator_mismatch",
            format!(
                "designation catalog id {designation_id} has canonical authoritative designator {:?}, not exact FAA model {:?}",
                candidate
                    .authoritative_designator
                    .as_deref()
                    .map(str::trim),
                server_faa_evidence.faa_model_designation()
            ),
        ));
    }
    let Some(selected_family_id) = (family.action == EntityResolutionAction::MatchExisting)
        .then_some(family.existing_catalog_id)
        .flatten()
    else {
        issues.push(issue(
            "designation_catalog_existing_family_required",
            format!(
                "existing designation id {designation_id} cannot be selected without its existing parent family"
            ),
        ));
        return;
    };
    if candidate.parent_catalog_id != Some(selected_family_id) {
        issues.push(issue(
            "designation_catalog_candidate_parent_mismatch",
            format!(
                "matched designation id {designation_id} belongs to family {:?}, but the selected existing family is {selected_family_id}",
                candidate.parent_catalog_id
            ),
        ));
    }
}

/// Return material listing tokens that cannot be attributed to the already
/// validated required identity.
///
/// This is deliberately comparison-only normalization. It neither rewrites
/// retained observations nor creates aliases. Exact contiguous token-sequence
/// membership prevents reordering and prevents a shorter label from consuming
/// a material prefix/suffix (`182 != 182T`, `SR22 != SR22T`).
fn unaccounted_observed_hierarchy_tokens(
    observed_label: &str,
    exact_faa_designation: &str,
    resolved_family_label: Option<&str>,
    resolved_observed_family_label: Option<&str>,
    resolved_make_label: Option<&str>,
    resolved_generation_label: Option<&str>,
    resolved_package_label: Option<&str>,
    faa_make_label: &str,
    exact_tcds_designation_serial_proof: bool,
    exact_designation_is_in_other_field: bool,
    exact_turbo_series_is_in_other_field: bool,
    named_tcds_family_label: Option<&str>,
) -> Vec<String> {
    let observed_tokens = alphanumeric_tokens(observed_label);
    let mut consumed = vec![false; observed_tokens.len()];

    consume_exact_designation_tokens(
        &observed_tokens,
        &mut consumed,
        exact_faa_designation,
        exact_tcds_designation_serial_proof,
    );
    for attributable_label in [
        resolved_family_label,
        resolved_observed_family_label,
        resolved_make_label,
        resolved_generation_label,
        resolved_package_label,
        Some(faa_make_label),
    ]
    .into_iter()
    .flatten()
    {
        consume_exact_label_tokens(&observed_tokens, &mut consumed, attributable_label);
    }
    consume_proof_gated_numeric_designation_series_stem(
        &observed_tokens,
        &mut consumed,
        exact_faa_designation,
        exact_tcds_designation_serial_proof,
        exact_designation_is_in_other_field,
        exact_turbo_series_is_in_other_field,
        named_tcds_family_label
            .is_some_and(|named_family| resolved_family_label == Some(named_family)),
    );

    observed_tokens
        .into_iter()
        .zip(consumed)
        .filter_map(|(token, consumed)| (!consumed).then_some(token))
        .collect()
}

fn retained_field_accounts_for_exact_faa_designation(
    retained_field: &str,
    exact_faa_designation: &str,
    exact_tcds_designation_serial_proof: bool,
) -> bool {
    let observed_tokens = alphanumeric_tokens(retained_field);
    let mut consumed = vec![false; observed_tokens.len()];
    consume_exact_designation_tokens(
        &observed_tokens,
        &mut consumed,
        exact_faa_designation,
        exact_tcds_designation_serial_proof,
    );
    consumed.into_iter().any(|value| value)
}

/// Account for a broad numeric series label only when the same retained
/// observation is bound either to its exact regulator-proven certified
/// designation in the other field or to the exact family independently named
/// by the same serial-bound TCDS proof. The separate `T182T` compatibility
/// case additionally requires the paired field to be exactly
/// `Turbo 182T <named family>`.
///
/// This remains comparison-only. It recognizes only digits followed by one
/// trailing designation letter (`182Q` -> `182`). It does not strip arbitrary
/// make/model prefixes, rewrite retained text, or consume any
/// generation/package/equipment token.
fn consume_proof_gated_numeric_designation_series_stem(
    observed_tokens: &[String],
    consumed: &mut [bool],
    exact_faa_designation: &str,
    exact_tcds_designation_serial_proof: bool,
    exact_designation_is_in_other_field: bool,
    exact_turbo_series_is_in_other_field: bool,
    exact_named_tcds_family_is_resolved: bool,
) {
    if !exact_tcds_designation_serial_proof {
        return;
    }
    let numeric_stem = if let Some(numeric_stem) = exact_numeric_series_stem(exact_faa_designation)
    {
        if !exact_designation_is_in_other_field && !exact_named_tcds_family_is_resolved {
            return;
        }
        numeric_stem
    } else {
        if !exact_turbo_series_is_in_other_field || !exact_named_tcds_family_is_resolved {
            return;
        }
        let Some(numeric_stem) = exact_turbo_numeric_series_stem(exact_faa_designation) else {
            return;
        };
        numeric_stem
    };
    if let Some(index) = observed_tokens
        .iter()
        .enumerate()
        .find_map(|(index, token)| (!consumed[index] && token == numeric_stem).then_some(index))
    {
        consumed[index] = true;
    }
}

fn alphanumeric_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn contains_exact_contiguous_label(value: &str, exact_label: &str) -> bool {
    let value_tokens = alphanumeric_tokens(value);
    let label_tokens = alphanumeric_tokens(exact_label);
    !label_tokens.is_empty()
        && value_tokens
            .windows(label_tokens.len())
            .any(|window| window == label_tokens)
}

/// Return the numeric series stem for the deliberately narrow certified
/// designation shape used by the case-bound manufacturer relationship.
///
/// Only digits followed by one terminal letter qualify (`182R` -> `182`).
/// Prefixes, multiple suffix letters, punctuation, and friendly display forms
/// do not qualify, so `T182T`, `SR22T`, and `182RG` never become broad-series
/// authority.
fn exact_numeric_series_stem(exact_faa_designation: &str) -> Option<&str> {
    let designation = exact_faa_designation.trim();
    if !designation.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return None;
    }
    let trailing = designation.chars().last()?;
    if !trailing.is_ascii_alphabetic() {
        return None;
    }
    let stem = &designation[..designation.len() - trailing.len_utf8()];
    (!stem.is_empty() && stem.bytes().all(|byte| byte.is_ascii_digit())).then_some(stem)
}

/// Return the numeric base of the one supported turbo display shape.
///
/// This remains distinct from [`exact_numeric_series_stem`]: `T182T` never
/// becomes generic authority for `182`. The caller may use this base only
/// when the paired retained field exactly spells `Turbo 182T <named family>`
/// under current serial-bound TCDS proof.
fn exact_turbo_numeric_series_stem(exact_faa_designation: &str) -> Option<&str> {
    let designation = exact_faa_designation.trim();
    if !designation.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return None;
    }
    let without_leading_t = designation.strip_prefix('T')?;
    let trailing = without_leading_t.chars().last()?;
    if !trailing.is_ascii_alphabetic() {
        return None;
    }
    let stem = &without_leading_t[..without_leading_t.len() - trailing.len_utf8()];
    (!stem.is_empty() && stem.bytes().all(|byte| byte.is_ascii_digit())).then_some(stem)
}

fn retained_field_is_exact_turbo_designation_family_phrase(
    retained_field: &str,
    exact_faa_designation: &str,
    named_tcds_family: &str,
    exact_tcds_designation_serial_proof: bool,
) -> bool {
    if !exact_tcds_designation_serial_proof
        || exact_turbo_numeric_series_stem(exact_faa_designation).is_none()
    {
        return false;
    }
    let Some(display_designation) = exact_faa_designation.trim().strip_prefix('T') else {
        return false;
    };
    let mut expected_tokens = vec!["turbo".to_string()];
    expected_tokens.extend(alphanumeric_tokens(display_designation));
    let family_tokens = alphanumeric_tokens(named_tcds_family);
    if family_tokens.is_empty() {
        return false;
    }
    expected_tokens.extend(family_tokens);
    alphanumeric_tokens(retained_field) == expected_tokens
}

fn exact_series_family_composition(
    observed_label: &str,
    exact_faa_designation: &str,
    canonical_family: &str,
) -> bool {
    let Some(series_stem) = exact_numeric_series_stem(exact_faa_designation) else {
        return false;
    };
    let observed_tokens = alphanumeric_tokens(observed_label);
    let family_tokens = alphanumeric_tokens(canonical_family);
    if family_tokens.is_empty() {
        return false;
    }
    let series_token = series_stem.to_ascii_lowercase();
    let mut series_then_family = Vec::with_capacity(family_tokens.len() + 1);
    series_then_family.push(series_token.clone());
    series_then_family.extend(family_tokens.iter().cloned());
    let mut family_then_series = family_tokens;
    family_then_series.push(series_token);
    observed_tokens == series_then_family || observed_tokens == family_then_series
}

fn excerpt_conames_exact_series_and_family(
    excerpt: &str,
    exact_faa_designation: &str,
    canonical_family: &str,
) -> bool {
    let Some(series_stem) = exact_numeric_series_stem(exact_faa_designation) else {
        return false;
    };
    let excerpt_tokens = alphanumeric_tokens(excerpt);
    let family_tokens = alphanumeric_tokens(canonical_family);
    if family_tokens.is_empty() {
        return false;
    }
    let series_token = series_stem.to_ascii_lowercase();
    let mut series_then_family = Vec::with_capacity(family_tokens.len() + 1);
    series_then_family.push(series_token.clone());
    series_then_family.extend(family_tokens.iter().cloned());
    let mut family_then_series = family_tokens;
    family_then_series.push(series_token);
    excerpt_tokens
        .windows(series_then_family.len())
        .any(|window| window == series_then_family || window == family_then_series)
}

fn consume_exact_designation_tokens(
    observed_tokens: &[String],
    consumed: &mut [bool],
    exact_faa_designation: &str,
    exact_tcds_designation_serial_proof: bool,
) {
    let designation_key = crate::aircraft::catalog::normalize_aircraft_designator_retrieval_key(
        exact_faa_designation,
    );
    if designation_key.is_empty() {
        return;
    }

    for start in 0..observed_tokens.len() {
        if consumed[start] {
            continue;
        }
        let mut candidate_key = String::new();
        for end in start..observed_tokens.len() {
            if consumed[end] {
                break;
            }
            candidate_key.push_str(
                &crate::aircraft::catalog::normalize_aircraft_designator_retrieval_key(
                    &observed_tokens[end],
                ),
            );
            if candidate_key == designation_key {
                consumed[start..=end].fill(true);
                break;
            }
            if candidate_key.len() >= designation_key.len() {
                break;
            }
        }
    }

    if exact_tcds_designation_serial_proof {
        consume_turbo_designation_display_expansion(
            observed_tokens,
            consumed,
            exact_faa_designation,
        );
    }
}

/// Recognize one conventional display expansion without ever producing a
/// normalized label: exact FAA `T…` may be compared with literal `Turbo`
/// immediately followed by that exact designator with its one leading `T`
/// removed. The pair is atomic, so bare `182T` cannot consume `T182T`.
fn consume_turbo_designation_display_expansion(
    observed_tokens: &[String],
    consumed: &mut [bool],
    exact_faa_designation: &str,
) {
    let exact_faa_designation = exact_faa_designation.trim();
    let Some(without_leading_t) = exact_faa_designation.strip_prefix('T') else {
        return;
    };
    if without_leading_t.is_empty()
        || without_leading_t.starts_with(['T', 't'])
        || !without_leading_t
            .chars()
            .any(|character| character.is_ascii_alphanumeric())
    {
        return;
    }
    let display_key =
        crate::aircraft::catalog::normalize_aircraft_designator_retrieval_key(without_leading_t);
    if display_key.is_empty() {
        return;
    }

    for turbo_index in 0..observed_tokens.len() {
        if consumed[turbo_index]
            || observed_tokens[turbo_index] != "turbo"
            || turbo_index + 1 >= observed_tokens.len()
        {
            continue;
        }
        let mut candidate_key = String::new();
        for end in turbo_index + 1..observed_tokens.len() {
            if consumed[end] {
                break;
            }
            candidate_key.push_str(
                &crate::aircraft::catalog::normalize_aircraft_designator_retrieval_key(
                    &observed_tokens[end],
                ),
            );
            if candidate_key == display_key {
                consumed[turbo_index..=end].fill(true);
                break;
            }
            if candidate_key.len() >= display_key.len() {
                break;
            }
        }
    }
}

fn consume_exact_label_tokens(
    observed_tokens: &[String],
    consumed: &mut [bool],
    attributable_label: &str,
) {
    let attributable_tokens = alphanumeric_tokens(attributable_label);
    if attributable_tokens.is_empty() || attributable_tokens.len() > observed_tokens.len() {
        return;
    }
    for start in 0..=observed_tokens.len() - attributable_tokens.len() {
        let end = start + attributable_tokens.len();
        if consumed[start..end]
            .iter()
            .any(|already_consumed| *already_consumed)
        {
            continue;
        }
        if observed_tokens[start..end] == attributable_tokens {
            consumed[start..end].fill(true);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_no_supported_selection(
    kind: HierarchyEntityKind,
    decision: &CatalogEntityDecision,
    optional: bool,
    candidates: &CatalogCandidateRegistry,
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    evidence_grounding: &GroundingAudit,
    designation_decision: &CatalogEntityDecision,
    generation_decision: &CatalogEntityDecision,
    resolved_make_label: Option<&str>,
    resolved_family_label: Option<&str>,
    resolved_observed_family_label: Option<&str>,
    resolved_generation_label: Option<&str>,
    resolved_package_label: Option<&str>,
    issues: &mut Vec<ValidationIssue>,
) {
    if !optional {
        issues.push(issue(
            "required_entity_no_supported_selection",
            format!("{} cannot use no_supported_selection", kind.as_str()),
        ));
    }
    if decision.existing_catalog_id.is_some()
        || decision
            .display_name
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || decision
            .authoritative_designator
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || !decision.evidence_ids.is_empty()
    {
        issues.push(issue(
            "no_supported_selection_has_entity_fields",
            format!(
                "{} no_supported_selection must not carry an id, label, designator, or evidence",
                kind.as_str()
            ),
        ));
    }
    let dossier_is_grounded = (evidence_grounding.mode == GroundingMode::FreshWeb
        && !evidence_grounding.reused_verified_dossier
        && evidence_grounding.google_search_call_count > 0
        && evidence_grounding.url_context_call_count > 0)
        || (evidence_grounding.mode == GroundingMode::ReusedVerifiedDossier
            && evidence_grounding.reused_verified_dossier
            && evidence_grounding.google_search_call_count == 0
            && evidence_grounding.url_context_call_count == 0
            && !evidence_grounding.citation_urls.is_empty())
        || regulator_complete_grounding_matches(research, evidence_grounding, server_faa_evidence);
    if !dossier_is_grounded {
        issues.push(issue(
            "no_supported_selection_grounding_required",
            format!(
                "{} no_supported_selection requires fresh Search plus URL Context grounding or an exact reused verified dossier",
                kind.as_str()
            ),
        ));
    }

    for (observed_field, observed_label, other_observed_label) in server_faa_evidence
        .observation_bindings
        .iter()
        .flat_map(|binding| {
            [
                (
                    "model",
                    binding.observed_model.trim(),
                    binding.observed_variant.trim(),
                ),
                (
                    "variant",
                    binding.observed_variant.trim(),
                    binding.observed_model.trim(),
                ),
            ]
        })
        .filter(|(_, value, _)| !value.is_empty())
    {
        let exact_designation_is_in_other_field = retained_field_accounts_for_exact_faa_designation(
            other_observed_label,
            server_faa_evidence.faa_model_designation(),
            server_faa_evidence.has_exact_tcds_designation_serial_proof(),
        );
        let exact_turbo_series_is_in_other_field = server_faa_evidence
            .tcds_family_binding
            .as_ref()
            .is_some_and(|binding| {
                retained_field_is_exact_turbo_designation_family_phrase(
                    other_observed_label,
                    server_faa_evidence.faa_model_designation(),
                    &binding.canonical_family_name,
                    server_faa_evidence.has_exact_tcds_designation_serial_proof(),
                )
            });
        let unaccounted_tokens = unaccounted_observed_hierarchy_tokens(
            observed_label,
            server_faa_evidence.faa_model_designation(),
            resolved_family_label,
            resolved_observed_family_label,
            resolved_make_label,
            resolved_generation_label,
            resolved_package_label,
            server_faa_evidence.faa_manufacturer_name(),
            server_faa_evidence.has_exact_tcds_designation_serial_proof(),
            exact_designation_is_in_other_field,
            exact_turbo_series_is_in_other_field,
            server_faa_evidence
                .tcds_family_binding
                .as_ref()
                .map(|binding| binding.canonical_family_name.as_str()),
        );
        if !unaccounted_tokens.is_empty() {
            issues.push(issue(
                "no_supported_selection_unaccounted_observed_label",
                format!(
                    "{} cannot be left NULL because observed {observed_field} {observed_label:?} has unaccounted material token(s) {:?}; only the exact FAA designation, proof-gated numeric series stem, exact contiguous positively resolved family, validated family alias, optional, or make labels may be consumed",
                    kind.as_str(),
                    unaccounted_tokens
                ),
            ));
        }
    }

    if candidates
        .catalog_revision
        .as_deref()
        .is_none_or(|revision| revision.trim().is_empty())
    {
        issues.push(issue(
            "no_supported_selection_catalog_state_missing",
            format!(
                "{} no_supported_selection requires the exact server-owned catalog result",
                kind.as_str()
            ),
        ));
        return;
    }
    let Some(search_request) = candidates.search_request.as_ref() else {
        issues.push(issue(
            "no_supported_selection_catalog_state_missing",
            format!(
                "{} no_supported_selection requires the echoed catalog search request",
                kind.as_str()
            ),
        ));
        return;
    };
    if search_request.observed_make.trim() != server_faa_evidence.faa_manufacturer_name()
        || search_request.observed_designation.trim() != server_faa_evidence.faa_model_designation()
        || server_faa_evidence
            .listing_model_years
            .iter()
            .any(|year| *year != search_request.model_year)
    {
        issues.push(issue(
            "no_supported_selection_catalog_scope_mismatch",
            format!(
                "{} catalog result is not scoped to the exact FAA legal make, designation, and every listing model year",
                kind.as_str()
            ),
        ));
    }

    let positive_candidates = match kind {
        HierarchyEntityKind::Generation => &research.generation_candidates,
        HierarchyEntityKind::Package => &research.package_candidates,
        _ => {
            return;
        }
    };
    if !positive_candidates.is_empty() {
        issues.push(issue(
            "no_supported_selection_positive_candidate_exists",
            format!(
                "{} research found positive direct-primary hierarchy candidates",
                kind.as_str()
            ),
        ));
    }

    let selected_designation_id = (designation_decision.action
        == EntityResolutionAction::MatchExisting)
        .then_some(designation_decision.existing_catalog_id)
        .flatten();
    match kind {
        HierarchyEntityKind::Generation => {
            if selected_designation_id.is_some_and(|designation_id| {
                candidates
                    .generation_designations
                    .iter()
                    .any(|(_, related_designation_id)| *related_designation_id == designation_id)
            }) {
                issues.push(issue(
                    "no_supported_selection_generation_relation_exists",
                    "the exact catalog already relates at least one generation to the selected designation",
                ));
            }
        }
        HierarchyEntityKind::Package => {
            let selected_generation_id = (generation_decision.action
                == EntityResolutionAction::MatchExisting)
                .then_some(generation_decision.existing_catalog_id)
                .flatten();
            if selected_designation_id.is_some_and(|designation_id| {
                candidates.package_applicability.iter().any(|row| {
                    row.package_kind == "trim_tier"
                        && row.aircraft_designation_id == designation_id
                        && row
                            .valid_from_model_year
                            .is_none_or(|from| from <= search_request.model_year)
                        && row
                            .valid_to_model_year
                            .is_none_or(|to| to >= search_request.model_year)
                        && (row.aircraft_generation_id.is_none()
                            || row.aircraft_generation_id == selected_generation_id)
                })
            }) {
                issues.push(issue(
                    "no_supported_selection_package_relation_exists",
                    "the exact catalog already has an applicable package for the selected designation/generation/model year",
                ));
            }
        }
        _ => {}
    }
}

pub(super) fn validate_faa_make_relationship(
    relationship: &FaaMakeRelationshipDecision,
    make: &CatalogEntityDecision,
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    catalog_candidates: &CatalogCandidateRegistry,
    issues: &mut Vec<ValidationIssue>,
) {
    let expected_faa_make = server_faa_evidence.faa_manufacturer_name();
    if relationship.faa_manufacturer_name.trim() != expected_faa_make {
        issues.push(issue(
            "faa_make_relationship_source_mismatch",
            format!(
                "FAA make relationship must preserve exact registry label {expected_faa_make:?}"
            ),
        ));
    }
    let selected_make = make.display_name.as_deref().map(str::trim).unwrap_or("");
    if relationship.canonical_make_name.trim() != selected_make {
        issues.push(issue(
            "faa_make_relationship_target_mismatch",
            "FAA make relationship canonical label does not equal the selected make",
        ));
    }
    let known_ids = research
        .claims
        .iter()
        .map(|claim| claim.evidence_id.as_str())
        .collect::<BTreeSet<_>>();
    for id in &relationship.evidence_ids {
        if !known_ids.contains(id.as_str()) {
            issues.push(issue(
                "faa_make_relationship_unknown_evidence",
                format!("FAA make relationship references unknown evidence id {id}"),
            ));
        }
    }
    for id in &relationship.applicability_evidence_ids {
        if !known_ids.contains(id.as_str()) {
            issues.push(issue(
                "faa_make_relationship_unknown_evidence",
                format!("FAA make applicability references unknown evidence id {id}"),
            ));
        }
    }
    if !relationship
        .evidence_ids
        .iter()
        .any(|id| id == server_faa_evidence.make_claim_id())
    {
        issues.push(issue(
            "faa_make_relationship_missing_server_evidence",
            "FAA make relationship must cite the exact server-created FAA make claim",
        ));
    }
    let eligible_tcds_holder_ids = server_faa_evidence
        .tcds_make_lineage_evidence()
        .and_then(|evidence| evidence.holder_transfer.as_ref())
        .map(|holder| {
            catalog_candidates
                .identities_by_kind
                .get(&HierarchyEntityKind::Make)
                .into_iter()
                .flat_map(BTreeMap::values)
                .filter(|candidate| {
                    tcds_holder_names_match(&candidate.display_name, &holder.former_holder_name)
                        || tcds_holder_names_match(
                            &candidate.display_name,
                            &holder.current_holder_name,
                        )
                })
                .map(|candidate| candidate.catalog_id)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if eligible_tcds_holder_ids.len() == 1 {
        let required_holder_id = *eligible_tcds_holder_ids
            .iter()
            .next()
            .expect("one eligible holder id was counted");
        if relationship.action != FaaMakeRelationshipAction::MatchTcdsMakeLineage
            || make.action != EntityResolutionAction::MatchExisting
            || make.existing_catalog_id != Some(required_holder_id)
        {
            issues.push(issue(
                "faa_make_tcds_lineage_required",
                format!(
                    "exact FAA/TCDS evidence and the returned catalog require existing holder make id {required_holder_id} with match_tcds_make_lineage"
                ),
            ));
        }
    } else if eligible_tcds_holder_ids.len() > 1 {
        issues.push(issue(
            "faa_make_tcds_lineage_holder_ambiguous",
            "FAA/TCDS holder lineage matches multiple returned canonical makes; admission requires catalog deduplication or operator review",
        ));
    }

    match relationship.action {
        FaaMakeRelationshipAction::ExactCanonicalLabel => {
            if selected_make != expected_faa_make {
                issues.push(issue(
                    "faa_make_exact_label_mismatch",
                    format!(
                        "exact FAA make action requires canonical label {expected_faa_make:?}, received {selected_make:?}"
                    ),
                ));
            }
            if relationship.existing_alias_id.is_some()
                || relationship.valid_from_model_year.is_some()
                || relationship.valid_to_model_year.is_some()
                || !relationship.applicability_evidence_ids.is_empty()
            {
                issues.push(issue(
                    "faa_make_exact_label_has_alias_fields",
                    "exact FAA make action must not carry alias id, bounds, or alias applicability evidence",
                ));
            }
        }
        FaaMakeRelationshipAction::MatchApprovedAlias => {
            if selected_make == expected_faa_make {
                issues.push(issue(
                    "faa_make_alias_not_required",
                    "alias action is invalid when the canonical and FAA make labels are already exact",
                ));
            }
            let Some(alias_id) = relationship.existing_alias_id else {
                issues.push(issue(
                    "faa_make_relationship_missing_existing_alias_id",
                    "match_approved_alias requires an exact alias id returned by catalog search",
                ));
                return;
            };
            let Some(alias) = catalog_candidates.make_aliases_by_id.get(&alias_id) else {
                issues.push(issue(
                    "faa_make_relationship_alias_not_retrieved",
                    format!("approved make alias id {alias_id} was not returned by catalog search"),
                ));
                return;
            };
            if make.existing_catalog_id != Some(alias.owner_catalog_id)
                || alias.alias.trim() != expected_faa_make
                || alias.valid_from_model_year != relationship.valid_from_model_year
                || alias.valid_to_model_year != relationship.valid_to_model_year
                || alias
                    .market_code
                    .as_deref()
                    .is_some_and(|market| market != "GLOBAL" && market != "US")
            {
                issues.push(issue(
                    "faa_make_relationship_alias_mismatch",
                    "selected approved alias does not exactly match the FAA label, canonical make, US/GLOBAL market, and copied applicability bounds",
                ));
            }
            validate_alias_web_evidence_and_scope(
                relationship,
                research,
                server_faa_evidence,
                issues,
            );
        }
        FaaMakeRelationshipAction::ProposeAlias => {
            if selected_make == expected_faa_make {
                issues.push(issue(
                    "faa_make_alias_not_required",
                    "alias action is invalid when the canonical and FAA make labels are already exact",
                ));
            }
            if relationship.existing_alias_id.is_some() {
                issues.push(issue(
                    "faa_make_relationship_new_alias_has_id",
                    "propose_alias must not carry an existing alias id",
                ));
            }
            validate_alias_web_evidence_and_scope(
                relationship,
                research,
                server_faa_evidence,
                issues,
            );
            validate_proposed_alias_evidence_bounds(
                relationship.valid_from_model_year,
                relationship.valid_to_model_year,
                &relationship.applicability_evidence_ids,
                research,
                server_faa_evidence,
                "faa_make_relationship",
                "FAA make alias",
                issues,
            );
        }
        FaaMakeRelationshipAction::MatchTcdsMakeLineage => {
            if let Err(message) = server_faa_evidence
                .validate_tcds_make_lineage_relationship(relationship, selected_make)
            {
                issues.push(issue("faa_make_tcds_lineage_mismatch", message));
            }
            if make.action != EntityResolutionAction::MatchExisting
                || make.existing_catalog_id.is_none()
                || eligible_tcds_holder_ids.len() != 1
                || make
                    .existing_catalog_id
                    .is_some_and(|id| !eligible_tcds_holder_ids.contains(&id))
            {
                issues.push(issue(
                    "faa_make_tcds_lineage_holder_ambiguous",
                    "FAA TCDS make-lineage requires exactly one returned existing make whose display label exactly names a parsed holder",
                ));
            }
        }
        FaaMakeRelationshipAction::Unresolved => {
            issues.push(issue(
                "faa_make_relationship_unresolved",
                "FAA legal manufacturer and canonical make relationship remains unresolved",
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_proposed_alias_evidence_bounds(
    valid_from_model_year: Option<i64>,
    valid_to_model_year: Option<i64>,
    applicability_evidence_ids: &[String],
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    issue_prefix: &str,
    alias_description: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let (Some(valid_from), Some(valid_to)) = (valid_from_model_year, valid_to_model_year) else {
        issues.push(issue(
            format!("{issue_prefix}_new_alias_requires_finite_bounds"),
            format!(
                "a proposed {alias_description} requires finite from/to model years; omitted or open bounds cannot be inferred from prose"
            ),
        ));
        return;
    };

    for bound in BTreeSet::from([valid_from, valid_to]) {
        let bound_token = bound.to_string();
        let explicitly_supported = applicability_evidence_ids.iter().any(|evidence_id| {
            is_web_manufacturer_claim(
                research,
                server_faa_evidence,
                evidence_id,
                crate::aircraft::catalog::EvidenceClaimKind::ProductionApplicability,
            ) && research.claims.iter().any(|claim| {
                claim.evidence_id == *evidence_id
                    && alphanumeric_tokens(&claim.evidence_excerpt)
                        .iter()
                        .any(|token| token == &bound_token)
            })
        });
        if !explicitly_supported {
            issues.push(issue(
                format!("{issue_prefix}_bound_missing_from_applicability_evidence"),
                format!(
                    "proposed {alias_description} bound {bound} must appear as an exact year token in a cited direct-primary production_applicability excerpt"
                ),
            ));
        }
    }
    let one_claim_contains_complete_interval =
        applicability_evidence_ids.iter().any(|evidence_id| {
            is_web_manufacturer_claim(
                research,
                server_faa_evidence,
                evidence_id,
                crate::aircraft::catalog::EvidenceClaimKind::ProductionApplicability,
            ) && research.claims.iter().any(|claim| {
                if claim.evidence_id != *evidence_id {
                    return false;
                }
                explicitly_states_alias_applicability_scope(
                    &claim.evidence_excerpt,
                    valid_from,
                    valid_to,
                )
            })
        });
    if !one_claim_contains_complete_interval {
        issues.push(issue(
            format!("{issue_prefix}_finite_interval_missing_from_single_applicability_evidence"),
            format!(
                "a proposed {alias_description} requires one selected direct-primary production_applicability excerpt that explicitly states the complete finite production/applicability scope; publication, anniversary, and unrelated event years do not establish an interval"
            ),
        ));
    }
    if server_faa_evidence
        .listing_model_years
        .iter()
        .any(|year| *year < valid_from || *year > valid_to)
    {
        issues.push(issue(
            format!("{issue_prefix}_applicability_interval_does_not_cover_listing_year"),
            format!(
                "the proposed {alias_description} finite interval must contain every immutable listing model year"
            ),
        ));
    }
}

fn explicitly_states_alias_applicability_scope(
    excerpt: &str,
    valid_from: i64,
    valid_to: i64,
) -> bool {
    let tokens = alphanumeric_tokens(excerpt);
    let has_applicability_subject = tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "applicability"
                | "applicable"
                | "available"
                | "availability"
                | "built"
                | "manufactured"
                | "marketed"
                | "offered"
                | "produced"
                | "production"
                | "relationship"
                | "sold"
        )
    });
    if !has_applicability_subject {
        return false;
    }

    let from = valid_from.to_string();
    let to = valid_to.to_string();
    if valid_from == valid_to {
        return tokens.windows(3).any(|window| {
            (window[0] == "model" && window[1] == "year" && window[2] == from)
                || (window[0] == from && window[1] == "model" && window[2] == "year")
                || (matches!(
                    window[0].as_str(),
                    "built" | "manufactured" | "marketed" | "offered" | "produced" | "sold"
                ) && matches!(window[1].as_str(), "for" | "in" | "during")
                    && window[2] == from)
        });
    }

    tokens.windows(4).any(|window| {
        ((window[0] == "from" && window[1] == from)
            && matches!(window[2].as_str(), "through" | "to" | "until")
            && window[3] == to)
            || ((window[0] == "between" && window[1] == from)
                && window[2] == "and"
                && window[3] == to)
    }) || tokens.windows(3).any(|window| {
        window[0] == from
            && matches!(window[1].as_str(), "through" | "to" | "until")
            && window[2] == to
    })
}

fn validate_alias_web_evidence_and_scope(
    relationship: &FaaMakeRelationshipDecision,
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    issues: &mut Vec<ValidationIssue>,
) {
    if !relationship.evidence_ids.iter().any(|id| {
        is_web_manufacturer_claim(
            research,
            server_faa_evidence,
            id,
            crate::aircraft::catalog::EvidenceClaimKind::HierarchyIdentity,
        ) && research.claims.iter().any(|claim| {
            claim.evidence_id == *id
                && contains_exact_contiguous_label(
                    &claim.evidence_excerpt,
                    server_faa_evidence.faa_manufacturer_name(),
                )
                && contains_exact_contiguous_label(
                    &claim.evidence_excerpt,
                    relationship.canonical_make_name.trim(),
                )
        })
    }) {
        issues.push(issue(
            "faa_make_relationship_missing_web_evidence",
            "an FAA legal-make to canonical-brand mapping requires a direct-primary hierarchy excerpt that co-names the exact FAA legal make and canonical brand",
        ));
    }
    if !relationship.applicability_evidence_ids.iter().any(|id| {
        is_web_manufacturer_claim(
            research,
            server_faa_evidence,
            id,
            crate::aircraft::catalog::EvidenceClaimKind::ProductionApplicability,
        )
    }) {
        issues.push(issue(
            "faa_make_relationship_missing_applicability_evidence",
            "an alias requires primary web evidence for its bounded or explicitly unbounded year applicability",
        ));
    }
    if relationship
        .valid_from_model_year
        .is_some_and(|year| !(1900..=2200).contains(&year))
        || relationship
            .valid_to_model_year
            .is_some_and(|year| !(1900..=2200).contains(&year))
        || matches!(
            (
                relationship.valid_from_model_year,
                relationship.valid_to_model_year
            ),
            (Some(from), Some(to)) if to < from
        )
    {
        issues.push(issue(
            "faa_make_relationship_invalid_year_scope",
            "alias applicability bounds must be ordered years in 1900..=2200",
        ));
    }
    let applies_to_year = |year: i64| {
        relationship
            .valid_from_model_year
            .is_none_or(|from| from <= year)
            && relationship.valid_to_model_year.is_none_or(|to| to >= year)
    };
    if server_faa_evidence
        .listing_model_years
        .iter()
        .any(|year| !applies_to_year(*year))
    {
        issues.push(issue(
            "faa_make_relationship_year_out_of_scope",
            "alias applicability bounds do not cover every immutable listing model year in this case; FAA year manufactured is audit-only",
        ));
    }
}

fn validate_family_label_relationship(
    relationship: &FamilyLabelRelationshipDecision,
    make: &CatalogEntityDecision,
    family: &CatalogEntityDecision,
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    catalog_candidates: &CatalogCandidateRegistry,
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let initial_issue_count = issues.len();
    let observed_label = relationship.observed_family_label.trim();
    let canonical_family = relationship.canonical_family_name.trim();
    let selected_family = family.display_name.as_deref().map(str::trim).unwrap_or("");

    let bound_observed_models = server_faa_evidence
        .observation_bindings
        .iter()
        .map(|binding| binding.observed_model.trim())
        .filter(|label| !label.is_empty())
        .collect::<BTreeSet<_>>();
    if observed_label.is_empty()
        || bound_observed_models.len() != 1
        || !bound_observed_models.contains(observed_label)
    {
        issues.push(issue(
            "family_label_relationship_source_mismatch",
            format!(
                "family-label relationship must preserve one exact retained model/family label; received {observed_label:?}, bound labels are {bound_observed_models:?}"
            ),
        ));
    }
    if catalog_candidates
        .search_request
        .as_ref()
        .is_none_or(|request| request.observed_family.trim() != observed_label)
    {
        issues.push(issue(
            "family_label_relationship_catalog_scope_mismatch",
            "family-label relationship must use the exact observed_family echoed by the catalog search",
        ));
    }
    if canonical_family.is_empty() || canonical_family != selected_family {
        issues.push(issue(
            "family_label_relationship_target_mismatch",
            "family-label relationship canonical owner must exactly equal the selected family",
        ));
    }

    let known_ids = research
        .claims
        .iter()
        .map(|claim| claim.evidence_id.as_str())
        .collect::<BTreeSet<_>>();
    for id in relationship
        .evidence_ids
        .iter()
        .chain(relationship.applicability_evidence_ids.iter())
    {
        if !known_ids.contains(id.as_str()) {
            issues.push(issue(
                "family_label_relationship_unknown_evidence",
                format!("family-label relationship references unknown evidence id {id}"),
            ));
        }
    }

    match relationship.action {
        FamilyLabelRelationshipAction::ExactCanonicalLabel => {
            if observed_label != selected_family {
                issues.push(issue(
                    "family_label_exact_canonical_mismatch",
                    "exact_canonical_label requires the retained family label and selected canonical family to be literally equal",
                ));
            }
            if relationship.existing_alias_id.is_some()
                || relationship.valid_from_model_year.is_some()
                || relationship.valid_to_model_year.is_some()
                || !relationship.evidence_ids.is_empty()
                || !relationship.applicability_evidence_ids.is_empty()
            {
                issues.push(issue(
                    "family_label_exact_canonical_has_alias_fields",
                    "exact_canonical_label must not carry an alias id, year bounds, or relationship/applicability evidence",
                ));
            }
        }
        FamilyLabelRelationshipAction::MatchManufacturerSeriesFamily => {
            if observed_label == selected_family
                || relationship.existing_alias_id.is_some()
                || relationship.valid_from_model_year.is_some()
                || relationship.valid_to_model_year.is_some()
                || !relationship.applicability_evidence_ids.is_empty()
            {
                issues.push(issue(
                    "family_label_manufacturer_series_has_alias_fields",
                    "manufacturer series/family matching requires different retained/canonical labels, no alias id, no model-year bounds, and no alias-applicability evidence",
                ));
            }
            if !server_faa_evidence.has_exact_tcds_designation_serial_proof() {
                issues.push(issue(
                    "family_label_manufacturer_series_tcds_identity_required",
                    "manufacturer series/family matching requires exact current TCDS designation and serial-eligibility proof",
                ));
            }
            if server_faa_evidence.tcds_family_binding.is_some() {
                issues.push(issue(
                    "family_label_manufacturer_series_named_tcds_forbidden",
                    "a named-family TCDS projection must use the stronger match_faa_type_certificate_family action instead of OEM series/family matching",
                ));
            }
            if !exact_series_family_composition(
                observed_label,
                server_faa_evidence.faa_model_designation(),
                selected_family,
            ) {
                issues.push(issue(
                    "family_label_manufacturer_series_composition_mismatch",
                    "retained family/model label must consist exactly of the numeric FAA designation series stem and the selected canonical family, in either adjacent order",
                ));
            }
            if server_faa_evidence.observation_bindings.is_empty()
                || server_faa_evidence
                    .observation_bindings
                    .iter()
                    .any(|binding| {
                        binding.observed_model.trim() != observed_label
                            || !exact_token_label(
                                &binding.observed_variant,
                                server_faa_evidence.faa_model_designation(),
                            )
                    })
            {
                issues.push(issue(
                    "family_label_manufacturer_series_paired_designation_required",
                    "every bound observation must carry only the exact serial-bound FAA designation in the paired retained variant field",
                ));
            }
            let relationship_claims_are_exact = !relationship.evidence_ids.is_empty()
                && relationship.evidence_ids.iter().all(|evidence_id| {
                    research.claims.iter().any(|claim| {
                        claim.evidence_id == *evidence_id
                            && is_web_manufacturer_claim(
                                research,
                                server_faa_evidence,
                                evidence_id,
                                crate::aircraft::catalog::EvidenceClaimKind::HierarchyIdentity,
                            )
                            && excerpt_conames_exact_series_and_family(
                                &claim.evidence_excerpt,
                                server_faa_evidence.faa_model_designation(),
                                selected_family,
                            )
                    })
                });
            if !relationship_claims_are_exact {
                issues.push(issue(
                    "family_label_manufacturer_series_primary_evidence_required",
                    "manufacturer series/family matching requires only direct-primary OEM hierarchy claims that co-name the exact numeric series stem and canonical family as adjacent components in either order",
                ));
            }
        }
        FamilyLabelRelationshipAction::MatchApprovedAlias => {
            if observed_label == selected_family {
                issues.push(issue(
                    "family_label_alias_not_required",
                    "an alias action is invalid when retained and canonical family labels are already exact",
                ));
            }
            let Some(alias_id) = relationship.existing_alias_id else {
                issues.push(issue(
                    "family_label_relationship_missing_existing_alias_id",
                    "match_approved_alias requires an exact family alias id returned by catalog search",
                ));
                return false;
            };
            let Some(alias) = catalog_candidates.family_aliases_by_id.get(&alias_id) else {
                issues.push(issue(
                    "family_label_relationship_alias_not_retrieved",
                    format!(
                        "approved family alias id {alias_id} was not returned by catalog search"
                    ),
                ));
                return false;
            };
            if family.existing_catalog_id != Some(alias.owner_catalog_id)
                || alias.alias.trim() != observed_label
                || alias.valid_from_model_year != relationship.valid_from_model_year
                || alias.valid_to_model_year != relationship.valid_to_model_year
                || alias
                    .market_code
                    .as_deref()
                    .is_some_and(|market| market != "GLOBAL" && market != "US")
            {
                issues.push(issue(
                    "family_label_relationship_alias_mismatch",
                    "selected approved family alias does not exactly match the retained label, canonical owner, US/GLOBAL market, and copied applicability bounds",
                ));
            }
            validate_family_label_year_scope(relationship, server_faa_evidence, issues);
        }
        FamilyLabelRelationshipAction::ProposeAlias => {
            if observed_label == selected_family {
                issues.push(issue(
                    "family_label_alias_not_required",
                    "an alias action is invalid when retained and canonical family labels are already exact",
                ));
            }
            if relationship.existing_alias_id.is_some() {
                issues.push(issue(
                    "family_label_relationship_new_alias_has_id",
                    "propose_alias must not carry an existing alias id",
                ));
            }
            let has_same_claim_primary_conaming =
                relationship.evidence_ids.iter().any(|evidence_id| {
                    research.claims.iter().any(|claim| {
                        claim.evidence_id == *evidence_id
                            && is_web_manufacturer_claim(
                                research,
                                server_faa_evidence,
                                evidence_id,
                                crate::aircraft::catalog::EvidenceClaimKind::HierarchyIdentity,
                            )
                            && contains_exact_contiguous_label(
                                &claim.evidence_excerpt,
                                observed_label,
                            )
                            && contains_exact_contiguous_label(
                                &claim.evidence_excerpt,
                                selected_family,
                            )
                    })
                });
            if !has_same_claim_primary_conaming {
                issues.push(issue(
                    "family_label_relationship_missing_conaming_evidence",
                    "a new family alias requires one direct-primary OEM hierarchy claim whose exact excerpt co-names the retained and canonical family labels as exact contiguous token sequences",
                ));
            }
            if !relationship
                .applicability_evidence_ids
                .iter()
                .any(|evidence_id| {
                    is_web_manufacturer_claim(
                        research,
                        server_faa_evidence,
                        evidence_id,
                        crate::aircraft::catalog::EvidenceClaimKind::ProductionApplicability,
                    )
                })
            {
                issues.push(issue(
                    "family_label_relationship_missing_applicability_evidence",
                    "a new family alias requires direct-primary OEM production-applicability evidence for its finite from/to scope",
                ));
            }
            validate_family_label_year_scope(relationship, server_faa_evidence, issues);
            validate_proposed_alias_evidence_bounds(
                relationship.valid_from_model_year,
                relationship.valid_to_model_year,
                &relationship.applicability_evidence_ids,
                research,
                server_faa_evidence,
                "family_label_relationship",
                "family alias",
                issues,
            );
            validate_proposed_family_alias_collisions(
                relationship,
                make,
                family,
                catalog_candidates,
                issues,
            );
        }
        FamilyLabelRelationshipAction::MatchFaaTypeCertificateFamily => {
            if observed_label == selected_family
                || relationship.existing_alias_id.is_some()
                || relationship.valid_from_model_year.is_some()
                || relationship.valid_to_model_year.is_some()
                || !relationship.applicability_evidence_ids.is_empty()
            {
                issues.push(issue(
                    "family_label_type_certificate_has_alias_fields",
                    "FAA type-certificate family matching requires different retained/canonical labels and no alias id, model-year bounds, or alias-applicability evidence",
                ));
            }
            if let Err(message) =
                server_faa_evidence.validate_tcds_family_relationship(relationship)
            {
                issues.push(issue(
                    "family_label_type_certificate_evidence_mismatch",
                    message,
                ));
            }
        }
        FamilyLabelRelationshipAction::Unresolved => {
            issues.push(issue(
                "family_label_relationship_unresolved",
                "retained model/family label and canonical family relationship remains unresolved",
            ));
        }
    }

    issues.len() == initial_issue_count
}

fn validate_proposed_family_alias_collisions(
    relationship: &FamilyLabelRelationshipDecision,
    make: &CatalogEntityDecision,
    family: &CatalogEntityDecision,
    catalog_candidates: &CatalogCandidateRegistry,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(make_id) = (make.action == EntityResolutionAction::MatchExisting)
        .then_some(make.existing_catalog_id)
        .flatten()
    else {
        // A newly proposed make has no same-make rows in the immutable catalog
        // result. Existing-family parent binding is validated separately.
        return;
    };
    let normalized_alias = crate::aircraft::catalog::normalize_aircraft_retrieval_text(
        &relationship.observed_family_label,
    );
    let normalized_family = crate::aircraft::catalog::normalize_aircraft_retrieval_text(
        &relationship.observed_family_label,
    );

    let same_make_family = |family_id: i64| {
        catalog_candidates
            .identity(HierarchyEntityKind::Family, family_id)
            .is_some_and(|candidate| candidate.parent_catalog_id == Some(make_id))
    };
    if let Some(alias) = catalog_candidates
        .family_aliases_by_id
        .values()
        .find(|alias| {
            same_make_family(alias.owner_catalog_id)
                && crate::aircraft::catalog::normalize_aircraft_retrieval_text(&alias.alias)
                    == normalized_alias
        })
    {
        issues.push(issue(
            "family_label_relationship_same_make_alias_collision",
            format!(
                "proposed family alias {:?} collides with returned same-make alias id {} owned by family {}; collision checks conservatively ignore year and market scope",
                relationship.observed_family_label, alias.alias_id, alias.owner_catalog_id
            ),
        ));
    }

    let selected_existing_family_id = (family.action == EntityResolutionAction::MatchExisting)
        .then_some(family.existing_catalog_id)
        .flatten();
    if let Some(candidate) = catalog_candidates
        .identities_by_kind
        .get(&HierarchyEntityKind::Family)
        .into_iter()
        .flat_map(|identities| identities.values())
        .find(|candidate| {
            candidate.parent_catalog_id == Some(make_id)
                && Some(candidate.catalog_id) != selected_existing_family_id
                && crate::aircraft::catalog::normalize_aircraft_retrieval_text(
                    &candidate.display_name,
                ) == normalized_family
        })
    {
        issues.push(issue(
            "family_label_relationship_same_make_canonical_collision",
            format!(
                "proposed family alias {:?} collides with returned canonical same-make family id {} ({:?})",
                relationship.observed_family_label,
                candidate.catalog_id,
                candidate.display_name
            ),
        ));
    }
}

fn validate_family_label_year_scope(
    relationship: &FamilyLabelRelationshipDecision,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    issues: &mut Vec<ValidationIssue>,
) {
    if relationship
        .valid_from_model_year
        .is_some_and(|year| !(1900..=2200).contains(&year))
        || relationship
            .valid_to_model_year
            .is_some_and(|year| !(1900..=2200).contains(&year))
        || matches!(
            (
                relationship.valid_from_model_year,
                relationship.valid_to_model_year
            ),
            (Some(from), Some(to)) if to < from
        )
    {
        issues.push(issue(
            "family_label_relationship_invalid_year_scope",
            "family alias applicability bounds must be ordered years in 1900..=2200",
        ));
    }
    if server_faa_evidence.listing_model_years.iter().any(|year| {
        relationship
            .valid_from_model_year
            .is_some_and(|from| from > *year)
            || relationship
                .valid_to_model_year
                .is_some_and(|to| to < *year)
    }) {
        issues.push(issue(
            "family_label_relationship_year_out_of_scope",
            "family alias applicability bounds do not cover every immutable listing model year in this case; FAA year manufactured is audit-only",
        ));
    }
}

fn is_web_manufacturer_claim(
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    evidence_id: &str,
    claim_kind: crate::aircraft::catalog::EvidenceClaimKind,
) -> bool {
    research.claims.iter().any(|claim| {
        claim.evidence_id == evidence_id
            && !server_faa_evidence.contains_id(evidence_id)
            && !is_obvious_secondary_or_mirror_source_url(&claim.source_url)
            && matches!(
                claim.source_kind,
                crate::aircraft::catalog::EvidenceSourceKind::Manufacturer
                    | crate::aircraft::catalog::EvidenceSourceKind::ManufacturerServicePublication
            )
            && claim.supports.contains(&claim_kind)
    })
}

fn is_web_aircraft_hierarchy_claim(
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    evidence_id: &str,
) -> bool {
    research.claims.iter().any(|claim| {
        claim.evidence_id == evidence_id
            && !server_faa_evidence.contains_id(evidence_id)
            && !is_obvious_secondary_or_mirror_source_url(&claim.source_url)
            && matches!(
                claim.source_kind,
                crate::aircraft::catalog::EvidenceSourceKind::Manufacturer
                    | crate::aircraft::catalog::EvidenceSourceKind::ApprovedFlightManual
                    | crate::aircraft::catalog::EvidenceSourceKind::ManufacturerServicePublication
            )
            && claim
                .supports
                .contains(&crate::aircraft::catalog::EvidenceClaimKind::HierarchyIdentity)
    })
}

fn is_server_drs_named_family_hierarchy_claim(
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
    evidence_id: &str,
) -> bool {
    let is_named_family_claim = server_faa_evidence
        .tcds_family_claim_ids()
        .is_some_and(|ids| ids.hierarchy().iter().any(|id| id == evidence_id));
    if !is_named_family_claim {
        return false;
    }
    research.claims.iter().any(|claim| {
        claim.evidence_id == evidence_id
            && evidence_id.starts_with(SERVER_FAA_DRS_EVIDENCE_ID_PREFIX)
            && server_faa_evidence.contains_exact_claim(claim)
            && claim.source_kind == crate::aircraft::catalog::EvidenceSourceKind::TypeCertificate
            && claim
                .supports
                .contains(&crate::aircraft::catalog::EvidenceClaimKind::HierarchyIdentity)
    })
}

fn decision_has_exact_typed_candidate(
    kind: HierarchyEntityKind,
    decision: &CatalogEntityDecision,
    research: &AircraftIdentityEvidenceResearch,
    server_faa_evidence: &ServerFaaIdentityEvidence,
) -> bool {
    let Some(selected_label) = decision.display_name.as_deref().map(str::trim) else {
        return false;
    };
    let candidates = match kind {
        HierarchyEntityKind::Family => &research.family_candidates,
        HierarchyEntityKind::Generation => &research.generation_candidates,
        HierarchyEntityKind::Package => &research.package_candidates,
        HierarchyEntityKind::Make | HierarchyEntityKind::Designation => return false,
    };
    candidates.iter().any(|candidate| {
        candidate.label.trim() == selected_label
            && !candidate.evidence_ids.is_empty()
            && candidate.evidence_ids.iter().all(|evidence_id| {
                decision.evidence_ids.contains(evidence_id)
                    && (is_web_aircraft_hierarchy_claim(research, server_faa_evidence, evidence_id)
                        || (kind == HierarchyEntityKind::Family
                            && is_server_drs_named_family_hierarchy_claim(
                                research,
                                server_faa_evidence,
                                evidence_id,
                            )))
                    && (!matches!(
                        kind,
                        HierarchyEntityKind::Generation | HierarchyEntityKind::Package
                    ) || research.claims.iter().any(|claim| {
                        claim.evidence_id == *evidence_id
                            && contains_exact_contiguous_label(
                                &claim.evidence_excerpt,
                                selected_label,
                            )
                    }))
            })
    })
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

Prompt contract: {AIRCRAFT_IDENTITY_PROMPT_VERSION}.

This is an evidence-discovery pass, not a normalization pass and not permission to update a database. You must execute Google Search on this first attempt. Source selection is part of the task: first identify the aircraft OEM's or its corporate owner's official domain from an official company/about/brands page, then use site-restricted searches on that domain and its official media or service-publication subdomains. Prefer direct manufacturer publications, regulator type-certificate data, approved flight manuals, and manufacturer service publications. Use URL Context to inspect the exact direct primary pages you select. The high-confidence web-evidence path supports only directly fetchable OEM or corporate-owner HTML/plain-text publisher pages. Do not return a PDF, a search-result redirect, an archive, a document mirror, or a third-party copy: PDF text is unsupported for this admission path even when the publisher is authoritative. For a U.S. N-number, FAA registry evidence is controlling over the listing and model memory for registration, manufacturer serial number, year manufactured, FAA make/model/series, and FAA engine code. A conflict with FAA data must be returned as a contradiction, never silently reconciled.

Spend the limited source slots on direct first-party pages that answer these questions separately. When the appended server grounding includes an explicit current-TCDS named-family projection, that projection already answers question 2 for this exact aircraft: do not request family production boundaries, emit a family-relationship gap, or try to replace the regulator-owned family candidate. A TCDS identity binding without a named-family projection proves only designation and serial applicability, so OEM family research remains required.
1. Can direct official evidence prove a relationship between the FAA legal manufacturer and a different consumer-facing aircraft brand for the listing model year? This is an opportunistic alias proposal, not a required identity gap: if complete proof is unavailable, return no make/brand web claim or question because adjudication safely retains the exact FAA legal make.
2. What exact model family does the manufacturer place the FAA designation in? Separately, does one direct OEM excerpt explicitly co-name the retained numeric series and that canonical family? When the retained model is exactly the numeric series stem of the exact serial-bound FAA designation plus that family, in either adjacent order, this is a case-bound non-alias relationship and requires no production-year or production-applicability evidence. For every ordinary alias proposal, does one direct OEM excerpt explicitly co-name the complete retained model/family label and canonical family, and what OEM production-applicability evidence covers the listing model year? Do not derive either relationship by deleting a prefix or suffix.
3. Only when the retained hierarchy text itself asserts a possible marketing generation or factory tier/package, does the manufacturer separately name that exact hierarchy dimension for this designation and model year? Seek an official year-specific product page, brochure, lineup, press release, approved manual, or service publication that makes the naming structure explicit. Installed or standard equipment, avionics, configuration, options, maintenance, condition, and price/value are separate listing or valuation concerns and are not aircraft-identity research gaps.

Before emitting JSON, inventory every direct-primary source span relevant to each required relationship and its explicit finite year interval. Compare every interval with every immutable listing `model_year`. For each distinct need, if any available direct-primary source explicitly supplies a finite interval containing all listing model years, select a covering source and do not select a source whose interval ends before or begins after a listing year. Search-result order, a newer or older title, and a more convenient excerpt never justify choosing a non-covering interval when a covering interval is available. If no available exact span establishes coverage for a required identity relationship with no deterministic server decision, preserve that need as unresolved. Merely failing to prove an optional make/brand alias is not such a gap; omit the alias proposal and retain the exact FAA legal make.

Identify separately: legal/manufacturer make, model family, exact certified designation, marketing generation (only if the manufacturer actually names one), and factory tier/package (only if the manufacturer actually names one). Preserve material prefixes and suffixes: 182T is not T182T, SR22 is not SR22T, G6 is a generation, and GTS is a package/tier. Treat “Skylane” as a possible marketing/popular model/family name, not automatically as the certified designation or a package. Treat suspicious OCR or extraction labels such as “182I” as unresolved unless direct primary evidence proves them.

The server will attach its own case-bound claims for the imported FAA legal make/model and a current FAA TCDS designation/serial identity after this pass. When the exact FAA heading explicitly names a family, it also attaches a named-family projection. Do not invent, copy, or return evidence IDs beginning with `server_faa_registry.` or `server_faa_drs.`. A named-family projection resolves this FAA-bound aircraft's family without a model-year alias range, while retaining arbitrary listing model text only as audit input; an identity-only TCDS binding leaves family research unresolved. Research direct primary web evidence for every remaining fact, including a legal-manufacturer-to-marketing-brand relationship and any generation or tier/package actually asserted in the retained text. Neither the registry nor a TCDS establishes that TEXTRON AVIATION INC and Cessna are interchangeable names, that Skylane is a package, or which avionics are installed or standard.

Return every positively identified exact manufacturer family label in `family_candidates`, with only direct-primary evidence IDs whose typed `supports` include `hierarchy_identity` and whose exact excerpt names that family. Never attach a production-only or applicability-only claim to a hierarchy candidate merely because its excerpt repeats the family name; keep that claim separately for its typed role. A family candidate `label` is only the exact OEM family-name component. Exclude the consumer brand/make, the retained numeric model label, and the certified designation from that component even when an OEM heading or sentence contains all of them. Never copy the entire co-naming heading or excerpt into `label`; the complete co-naming excerpt remains evidence for the relationship. This establishes canonical family identity but does not map a different retained label to it.

Emit evidence coverage for every distinct proof need. Without an exact server TCDS named-family projection, always cover the exact family co-naming relationship as `hierarchy_identity`. When the retained model is exactly the numeric series stem of the exact serial-bound FAA designation plus the canonical family, in either adjacent order, and the paired retained variant is only that exact designation, the case-bound manufacturer-series/family path requires that hierarchy co-naming but no production-year bounds and no `production_applicability` claim. For every ordinary alias proposal, separately cover finite family production applicability as `production_applicability`. Cover finite FAA-legal-make-to-brand applicability as `production_applicability` only when proposing a canonical make different from the exact FAA legal make; otherwise emit no web make/brand claim or gap. With an exact named-family projection, do not emit a web family candidate merely to duplicate it and do not report missing family production years as unresolved; the server will install the digest-bound family candidate and case-specific relationship after this pass. One exact contiguous span may support multiple claim kinds when its literal text truly proves each need; do not duplicate the same span under redundant IDs merely to separate roles. When facts occur in different spans, emit separate claims and never build an omnibus excerpt by joining text. Candidate `evidence_ids` cite family identity/co-naming evidence, while the later relationship decisions cite the applicable exact claims.

When the complete retained model/family label differs from the canonical family, distinguish the narrow case-bound manufacturer-series/family relationship from a catalog alias. For the case-bound path, the retained label must consist exactly of the numeric series stem of the serial-bound FAA designation and the canonical family in either adjacent order, the paired retained variant must contain only that exact designation, and one direct OEM `hierarchy_identity` excerpt must co-name those same adjacent series/family components in either order; return no production-applicability claim or missing-applicability question for that path. For every other mapping, return the family co-naming `hierarchy_identity` claim only if one exact OEM excerpt contains both complete labels as exact contiguous token sequences, and separately return covering direct-primary `production_applicability` evidence. Preserve every explicitly supported boundary year in ordinary-alias applicability claims: a new alias must use finite from/to years, and each selected bound must appear as an exact year token in cited production-applicability evidence. If the source proves only the listing year, both bounds must equal that year; do not infer historical, current, open-ended, or unbounded applicability from prose. Never put a brand, retained numeric label, certified designation, generation, package, or equipment label into the family-name component.

Avionics, an equipment/default-configuration list, an option bundle, or a feature difference never establishes a named generation or package by itself. Do not turn G1000/G1000 NXi, a navigation option, an interior, an engine, or another installed/default feature into a hierarchy label. A generation/package candidate requires the direct official source to name that distinct commercial dimension. Return every positively identified exact label in `generation_candidates` or `package_candidates`, with its direct-primary evidence IDs. After the targeted official research, return an empty list when no positive exact candidate was found. An empty list is not a factual claim that the real-world dimension does not exist; it only reports that this dossier found no supported selection. Never return an unresolved question or contradiction merely because installed/default equipment, configuration, options, maintenance, condition, or price/value could not be established; omit those non-identity gaps from this aircraft-identity response.

Wikipedia, owner forums, brokers, dealers, marketplaces, manual-download sites, document mirrors, and reseller-hosted copies are discovery leads only. They cannot establish primary aircraft identity even when the copied document was originally written by the manufacturer. Follow a lead to the manufacturer's own URL. Do not cite rejected secondary pages in the discovery dossier, because every cited URL consumes a URL Context slot. If no direct primary page is available for a required aircraft-identity claim asserted by the retained hierarchy text, preserve that identity gap as unresolved instead of substituting a secondary copy or model memory. Do not manufacture an identity gap from missing equipment, configuration, maintenance, condition, or value evidence. Marketplace listings may explain what was observed but cannot establish canonical identity, production applicability, or factory package.

`unresolved_questions` is an aircraft-identity admission contract, not a general research backlog. Return each genuinely unresolved identity item as an exact allowed `scope` plus `question`. Use `family_identity`, `family_label_relationship`, or `family_production_applicability` only for that precise family need; use `faa_make_brand_relationship`, `designation`, `generation`, `package`, or `source_integrity` for those distinct identity needs. There is intentionally no generated catch-all scope. Put an actual source or FAA/listing disagreement in `contradictions`; use `source_integrity` only for a citation, retrieval, publisher-authority, or provenance defect that affects evidence actually required or returned for aircraft identity. An unsuccessful search for an optional make/brand alias is neither source-integrity uncertainty nor an identity gap because the exact FAA legal make is the deterministic fallback. Never report installation applicability, actual or factory-default equipment, avionics, options, configuration, maintenance, condition, price, or value as an unresolved aircraft-identity question or contradiction. Never label a make, designation, provenance, or optional-dimension uncertainty as a family question. The response schema may restrict this case to a server-selected subset of scopes; never evade that restriction by relabeling a question. Do not report a server-resolved fallback or exact TCDS decision as unresolved. Every unresolved question you do return remains blocking regardless of its scope. A question tied to a retained generation/package label such as G6 or GTS is unresolved and must be returned when its precise scope is allowed; an equipment label such as G1000 or NXi is outside this identity-question contract.

Every returned non-FAA claim must use the final direct http(s) URL of an OEM or corporate-owner HTML/plain-text page on the original publisher's host that appears in this response's citations, a concise source excerpt, and explicit supported claim kinds. Copy `evidence_excerpt` verbatim from one contiguous visible-text span on that exact publisher page. Do not paraphrase, join separate passages, add an ellipsis, preserve Markdown decoration, or expand a search/URL-Context snippet into wording absent from the page. The server will fetch the final URL and require the normalized excerpt as a token-bounded span in the fetched publisher text before any catalog decision. Label any retained secondary context as `recognized_secondary` or `marketplace_listing`, never as manufacturer, approved manual, service publication, regulator, or type certificate; secondary context cannot authorize admission and should normally be omitted. If authoritative sources disagree, return the contradiction. Do not fill gaps from model memory.

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

Before deciding, you must call search_aircraft_catalog exactly as provided. Pass the exact server FAA legal make as `observed_make`, copy the complete retained model/family field without normalization as `observed_family`, pass the exact FAA designation as `observed_designation`, and pass the listing model year unchanged. It returns the current approved catalog plus structured make/family aliases and server-owned generation/designation and package-applicability state; only IDs listed under `allowed_existing_ids_by_kind` for that entity kind may be selected. Other returned candidates are collision context and remain forbidden selections. Similar spelling is candidate retrieval, never proof of identity. Choose match_existing only when positive authoritative evidence establishes the same entity. Choose propose_new only when positive authoritative evidence establishes the exact new label and after reviewing all returned collision candidates. Choose unresolved whenever a listing hierarchy label or positive research candidate is not safely resolved.

When the research bundle contains multiple direct-primary applicability claims, compare every explicit finite interval with every immutable listing model year before selecting evidence IDs or bounds. If a covering interval is available for a relationship, select its claim ID and never select a non-covering claim for that relationship. Keep family co-naming, family applicability, and legal-make-to-brand applicability as distinct proof needs; one exact span may satisfy more than one need only when its literal text proves each selected role.

Make, family, and exact certified designation are required. The make decision's `evidence_ids` and the `faa_make_relationship.evidence_ids` MUST both include the exact server FAA make claim ID, for every relationship action. A `match_existing` family decision must copy the selected returned candidate's display label exactly, and that candidate's returned parent must be the selected existing make; an ID cannot be reused with a substituted label or parent. The family decision label must exactly equal one typed `family_candidates` label and cite all evidence IDs for that candidate before its canonical tokens can account for retained text. Return a separate `family_label_relationship` mapping the complete retained model/family label to the selected canonical family. Use `exact_canonical_label` only for literally equal labels and leave every alias/evidence field empty. Use `match_manufacturer_series_family` only when current FAA/TCDS evidence binds the exact designation and serial, no named-family TCDS projection exists, the paired retained variant equals only that exact designation with no other token, the complete retained model consists exactly of its numeric series stem plus the selected canonical family in either adjacent order, and cited direct-primary OEM hierarchy evidence co-names those same adjacent series/family components in either order. Preserve the complete retained label, cite only those OEM co-naming claims, and leave alias id, year bounds, and applicability evidence empty. This action is case-bound comparison only: it never creates a catalog alias, never consumes the complete label wholesale, and any leftover generation, package, or equipment token must still block. Use `match_approved_alias` only with the exact structured family alias id, owner, US/GLOBAL market, and copied year bounds returned by catalog search; approved aliases may retain copied open bounds. Use `propose_alias` only when one direct-primary OEM hierarchy claim's exact excerpt co-names both complete labels as exact contiguous token sequences and direct-primary `production_applicability` evidence covers the listing model year. A proposed alias requires finite from/to years, and each bound must appear as an exact year token in a cited direct-primary production-applicability excerpt; if only the listing year is explicit, use that year for both bounds. When the evidence bundle contains a case-bound current FAA DRS named-family projection whose exact FAA designation heading names the canonical family and whose serial-eligibility row covers the FAA-matched serial, you MUST instead use `match_faa_type_certificate_family`. Preserve the exact retained model as audit input and copy the canonical family from that projection, set alias id and both model-year bounds to null, leave applicability evidence empty, and include exactly all of that projection's `server_faa_drs.*` claim IDs in `evidence_ids`; do not add, omit, or substitute any claim ID. Never represent the retained label as a TCDS heading. This action is case-bound only: it must never create a catalog alias, infer a model-year interval, or be used without the exact server-provided named-family projection. Never propose a family alias when any returned same-make family has the same normalized alias or canonical family key, regardless of year or market scope. Otherwise use `unresolved`, which blocks admission. Never derive `182` from `182T` or `SR22` from `SR22T`. A `match_existing` designation may select only a returned ID whose canonical `authoritative_designator` literally equals the exact server FAA model claim, must copy that candidate's display label, and must belong to the selected existing family; similar or wrong-branch collision candidates remain visible but are forbidden selections. An existing designation cannot be attached to a newly proposed family. The designation decision's `authoritative_designator` must literally preserve the server FAA model claim, and a `propose_new` designation's `display_name` must literally preserve that same exact FAA value; a friendly display name cannot substitute for either. Generation and package are independent optional dimensions. Every positive generation/package decision must exactly equal its typed candidate and cite that candidate's direct-primary evidence. Do not put a generation or package into the certified designation merely because the listing combines the words. Never infer a generation or package from avionics (including a G1000/G1000 NXi installation), equipment, recency, a phrase such as “modern production generation,” or a “standard configuration”; those facts are not hierarchy evidence. Imported FAA registry claims establish only the exact legal make and exact FAA model printed in those claims. Exact `server_faa_drs.*` claims establish this serial's designation applicability and, only when the exact heading explicitly names one, its case-bound family identity; they never establish a catalog alias or production-year interval. The retained listing label remains audit input. When a named-family projection fully resolves a family need, the research bundle must not retain that need as an unresolved question; an identity-only binding does not resolve family. Every retained typed research question blocks admission. Neither FAA evidence class establishes a corporate/brand alias, generation, package, factory configuration, or avionics.

`no_supported_selection` is an operational NULL under this exact catalog result, not evidence or a claim that a real-world generation/package does not exist. It is valid only for generation/package, with null id/display/designator and an empty evidence list. Select it only when every material token in the retained model and variant fields is accounted for by the exact FAA designation, exact contiguous labels from the positively primary-supported resolved family and its validated `family_label_relationship`, or exact contiguous positive optional/make labels, targeted grounded research returned no positive candidate for that dimension, and the function result reports no existing relation applicable to the selected designation/generation/model year. The observed make is audited through the separate make-relationship decision and does not gate optional dimensions. This accounting is comparison-only: never rewrite the observation, use the designation display label as an accounting source, reorder a multi-token label, mechanically strip a substring, or let 182 consume 182T or SR22 consume SR22T. Retained model `182` plus variant `182T Skylane` is fully accounted only when 182T is the exact FAA designation, Skylane is the positively supported canonical family, and a valid family-label relationship maps the distinct exact label 182 to Skylane. G6/GTS or G1000/NXi-like leftover tokens must remain unresolved unless a typed, positive direct-primary decision accounts for them; they can never be erased by `no_supported_selection`.

Return a typed `faa_make_relationship` decision. Use `exact_canonical_label` when the selected canonical make literally equals the FAA legal make; in that case alias id, bounds, and applicability evidence must be null/empty. Use `match_approved_alias` only when the catalog returned the selected make with the exact FAA label as an approved alias; copy its exact alias id and year bounds, including already-approved open bounds. Use `propose_alias` only when primary web evidence proves a new legal-make-to-brand relationship and set no existing alias id. When the server evidence contains an exact current FAA TCDS holder-transfer projection and the catalog returns exactly one existing make whose display label exactly names one parsed holder, you MUST select that existing make and use `match_tcds_make_lineage`; never propose a new FAA-label make branch instead. This relates the exact FAA registry make to that existing holder for only this FAA release/code/designation/certified serial interval. If multiple returned makes name a parsed holder, do not choose among them: the case is ambiguous and must remain blocked for catalog cleanup or review. Copy exactly the server FAA make, TCDS model-heading, and holder-transfer claim IDs into `evidence_ids`; copy exactly the TCDS serial-applicability and optional manufacturer-range claim IDs into `applicability_evidence_ids`; leave alias id and both year bounds null. This action is a case-scoped immutable binding, never an alias, and may not be inferred from spelling, suffix removal, or company-name normalization. Other non-exact relationships must cite both the server FAA make claim and separate primary web evidence. A proposed alias must also have finite from/to model years, and each bound must appear as an exact year token in a cited direct-primary `production_applicability` excerpt. If evidence proves only the listing year, set both bounds to that year; never infer historical or open-ended applicability from prose or one listing. If no complete approved-alias, TCDS-lineage, or primary-web proof is available, do not select the marketing brand and do not leave the relationship unresolved: safely fall back to the exact FAA legal make as the canonical make, choose `exact_canonical_label`, and cite the server FAA make claim in both decisions. Confidence may be very_high only when every selected dimension, make relationship, and family-label relationship has the required direct evidence or exact approved catalog state and no unresolved collision.

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
        r#"Independently audit the proposed aircraft hierarchy below. This is a fresh reasoning pass: do not defer to the first adjudicator's confidence or rationale.

Audit against the caller-supplied primary-source evidence bundle and exact server-owned catalog result for this FAA-bound case. Claims beginning `server_faa_registry.` were created from the immutable imported FAA registry and are authoritative only for their exact legal make/model excerpts. Claims beginning `server_faa_drs.` were created from one digest-bound current FAA TCDS only after the exact FAA designation heading and FAA-matched serial-eligibility row passed deterministic validation; they always prove designation applicability and prove family only when that exact heading explicitly names it. The retained listing label is audit input, never a claimed TCDS heading. These claims never establish a catalog alias or year interval. Every retained typed research question is blocking regardless of its model-selected scope; reject any payload that reaches this pass with one. Every non-server claim actually cited by the proposed adjudication must be a verbatim contiguous excerpt already verified against a server-fetched final OEM HTML/plain-text publisher URL. Other PDFs, mirrors, paraphrases, ellipses, Markdown-decorated quotations, and passages composed from separate spans are unsupported. Do not broaden or replace either evidence set. Search/URL discovery candidates that were not selected into the evidence bundle and are not cited by the proposed adjudication are retrieval noise: do not audit them, require facts from them, or turn their irrelevance into an error. Check every selected web evidence URL, the exact server FAA claims, every existing/new decision, exact designation characters, model-year applicability, the typed FAA-legal-make relationship, the typed `family_label_relationship`, and the separation of certified designation from generation and package. Confirm that a matched family copies the exact returned entity kind, display label, and parent make. For every proposed make or family alias, require finite from/to years and confirm each bound appears as an exact year token in a cited direct-primary `production_applicability` excerpt; open bounds are allowed only when copied from an already-approved alias. For a proposed family alias, also confirm that one direct-primary OEM excerpt co-names the complete observed and canonical labels as exact contiguous token sequences, affirm every relationship evidence ID, and reject every returned same-make normalized alias or canonical-family collision regardless of year or market scope. Registry evidence alone cannot prove family; only an explicit server DRS named-family projection can do so without OEM web evidence. Neither FAA evidence class proves a Textron/Cessna-style brand relationship, generation, package, factory configuration, or avionics. Explicitly compare collision-prone pairs when relevant (182/182T/T182T, SR22/SR22T, generation/tier, and popular name/certified model). For `no_supported_selection`, confirm only that this exact case has no safely selectable value: every retained model/variant material token is exactly accounted for by exact contiguous FAA/canonical/validated-relationship labels, there is no positive direct-primary candidate, and there is no applicable catalog relation. The observed make is handled separately and does not gate optional dimensions. Token accounting is comparison-only and exact-contiguous-token based; it must not use the designation display label, reorder tokens, rewrite input, or substring-strip a prefix/suffix. Never reinterpret the operational NULL as proof that the real-world dimension does not exist. G6/GTS- or G1000/NXi-like leftovers remain unresolved unless positively resolved. Confirm at very_high confidence only if the proposal is fully proved by the appropriate evidence class. Otherwise reject or return ambiguous. Reference only evidence IDs present in the supplied bundle, including exact registry/DRS server IDs and every relationship evidence ID when affirming them; report contradictions as errors rather than silently replacing the evidence bundle.

Audit payload:
{}"#,
        serde_json::to_string_pretty(&payload).expect("verification payload serializes")
    )
}

pub fn identity_evidence_response_schema() -> Value {
    identity_evidence_response_schema_with_unresolved_scopes(ALL_RESEARCH_UNRESOLVED_SCOPES)
}

/// Builds the evidence response schema with only the unresolved scopes that
/// the server determined are meaningful for this exact case.
///
/// This allowlist controls what the model may report; it never changes the
/// admission rule. Every returned unresolved question remains blocking.
/// `Other` is accepted by the Rust type only so a historical or
/// non-schema-conforming payload fails closed; it is never exposed to model
/// generation. An empty effective allowlist is represented by an empty array
/// contract instead of an invalid JSON Schema `enum: []`.
pub fn identity_evidence_response_schema_with_unresolved_scopes(
    allowed_unresolved_scopes: &[ResearchUnresolvedScope],
) -> Value {
    let mut seen_scopes = BTreeSet::new();
    let allowed_scope_names = allowed_unresolved_scopes
        .iter()
        .copied()
        .filter(|scope| scope.is_model_emittable())
        .map(ResearchUnresolvedScope::as_str)
        .filter(|scope| seen_scopes.insert(*scope))
        .collect::<Vec<_>>();
    let no_unresolved_scopes_allowed = allowed_scope_names.is_empty();
    let scope_schema = if no_unresolved_scopes_allowed {
        // `maxItems: 0` below makes this sentinel unreachable while retaining
        // a valid non-empty JSON Schema enum for Gemini's schema validator.
        json!({"type": "string", "enum": ["other"]})
    } else {
        json!({"type": "string", "enum": allowed_scope_names})
    };
    let mut unresolved_questions_schema = json!({
        "type": "array",
        "description": "Aircraft-identity questions only, never a general research backlog. Every emitted item is blocking regardless of scope. The generated contract has no catch-all scope: a citation, retrieval, publisher-authority, or provenance defect affecting identity evidence actually required or returned uses source_integrity, and an actual source or FAA/listing disagreement belongs in contradictions. An unsuccessful optional make/brand-alias search is not a gap because the exact FAA legal make is the deterministic fallback. Never report installation applicability, actual or factory-default equipment, avionics, options, configuration, maintenance, condition, price, or value here. Do not report a server-resolved fallback or exact regulator decision as unresolved, and never relabel a question to evade the server-supplied scope allowlist.",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "scope": scope_schema,
                "question": {
                    "type": "string",
                    "minLength": 1,
                    "description": "One precise unresolved aircraft-identity question within the selected typed scope; never an equipment, configuration, maintenance, condition, price, or value research question"
                }
            },
            "required": ["scope", "question"]
        }
    });
    if no_unresolved_scopes_allowed {
        unresolved_questions_schema["maxItems"] = json!(0);
    }

    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "subject_summary": {"type": "string"},
            "claims": {
                "type": "array",
                "description": "Cover family co-naming hierarchy identity, ordinary-alias family production applicability, and legal-make-to-brand applicability as distinct proof needs. The case-bound numeric-series/family relationship requires exact OEM hierarchy co-naming but no production-year or production-applicability claim. One exact span may support multiple kinds only when its literal text proves each; use separate claims for separate spans and never join spans or duplicate one span under redundant IDs.",
                "items": evidence_claim_schema()
            },
            "family_candidates": {
                "type": "array",
                "description": "Each label is only the exact OEM family-name component, excluding consumer brand/make, retained numeric model label, certified designation, generation, and package. The full co-naming sentence remains evidence, not the candidate label.",
                "items": hierarchy_candidate_schema(
                    "Exact OEM family-name component only; never the complete co-naming heading or sentence"
                )
            },
            "generation_candidates": {
                "type": "array",
                "items": hierarchy_candidate_schema(
                    "Exact OEM-named generation component only"
                )
            },
            "package_candidates": {
                "type": "array",
                "items": hierarchy_candidate_schema(
                    "Exact OEM-named factory package/tier component only"
                )
            },
            "contradictions": {
                "type": "array",
                "description": "Actual aircraft-identity conflicts only: a retained listing identity fact conflicts with controlling FAA data, or authoritative identity sources directly disagree. Missing or uncertain installation applicability, equipment, avionics, options, configuration, maintenance, condition, price, or value is not a contradiction and must be omitted.",
                "items": {"type": "string"}
            },
            "unresolved_questions": unresolved_questions_schema
        },
        "required": [
            "subject_summary", "claims", "family_candidates", "generation_candidates",
            "package_candidates", "contradictions", "unresolved_questions"
        ]
    })
}

fn hierarchy_candidate_schema(label_description: &str) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "label": {
                "type": "string",
                "description": label_description
            },
            "evidence_ids": {
                "type": "array",
                "description": "Every id must reference a direct-primary hierarchy_identity claim whose exact excerpt names this exact candidate label",
                "items": {"type": "string"}
            }
        },
        "required": ["label", "evidence_ids"]
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
            "evidence_excerpt": {
                "type": "string",
                "description": "One verbatim contiguous publisher span for this claim's single proof need; never join separate spans"
            },
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
                        "hierarchy_identity", "production_applicability"
                    ]
                },
                "minItems": 1,
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
                "enum": ["match_existing", "propose_new", "no_supported_selection", "unresolved"],
                "description": "no_supported_selection is valid only for generation/package and represents an operational NULL for the exact grounded catalog state, never factual absence"
            },
            "existing_catalog_id": {"type": ["integer", "null"]},
            "display_name": {
                "type": ["string", "null"],
                "description": "For match_existing family/designation, copy the exact display_name of that returned candidate and preserve its returned parent branch; for a propose_new designation, literally copy the exact server FAA model claim; never substitute a friendly label"
            },
            "authoritative_designator": {
                "type": ["string", "null"],
                "description": "For designation, literally copy the exact server FAA model claim; a match_existing ID is selectable only when its returned canonical authoritative_designator is the same exact value"
            },
            "evidence_ids": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Use the exact server FAA make claim for make and the exact server FAA model claim for designation; positive generation/package selections require direct primary hierarchy evidence; no_supported_selection requires an empty list"
            },
            "rationale": {"type": "string"}
        },
        "required": [
            "action", "existing_catalog_id", "display_name",
            "authoritative_designator", "evidence_ids", "rationale"
        ]
    });
    let faa_make_relationship = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "action": {
                "type": "string",
                "enum": [
                    "exact_canonical_label", "match_approved_alias",
                    "propose_alias", "match_tcds_make_lineage", "unresolved"
                ],
                "description": "match_tcds_make_lineage is permitted only for an existing catalog make exactly named by the case-bound current TCDS holder-transfer projection; it creates a release/code/designation/serial-scoped binding, never an alias. If a non-exact relationship lacks complete direct proof, select the exact FAA legal make and exact_canonical_label instead of unresolved"
            },
            "faa_manufacturer_name": {"type": "string"},
            "canonical_make_name": {"type": "string"},
            "existing_alias_id": {"type": ["integer", "null"], "minimum": 1},
            "valid_from_model_year": {
                "type": ["integer", "null"], "minimum": 1900, "maximum": 2200,
                "description": "Required and finite for propose_alias, with this exact year token present in cited direct-primary production_applicability evidence; copy exactly for match_approved_alias"
            },
            "valid_to_model_year": {
                "type": ["integer", "null"], "minimum": 1900, "maximum": 2200,
                "description": "Required and finite for propose_alias, with this exact year token present in cited direct-primary production_applicability evidence; copy exactly for match_approved_alias"
            },
            "evidence_ids": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Must always include the exact server FAA make claim; match_tcds_make_lineage additionally requires exactly the server TCDS model-heading and holder-transfer claims"
            },
            "applicability_evidence_ids": {
                "type": "array",
                "items": {"type": "string"},
                "description": "For propose_alias, cited direct-primary production_applicability excerpts must explicitly contain both finite boundary years as exact tokens. For match_tcds_make_lineage, include exactly the server TCDS serial-applicability and optional manufacturer-range claims"
            },
            "rationale": {"type": "string"}
        },
        "required": [
            "action", "faa_manufacturer_name", "canonical_make_name",
            "existing_alias_id", "valid_from_model_year", "valid_to_model_year",
            "evidence_ids", "applicability_evidence_ids", "rationale"
        ]
    });
    let family_label_relationship = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "action": {
                "type": "string",
                "enum": [
                    "exact_canonical_label",
                    "match_manufacturer_series_family",
                    "match_approved_alias", "propose_alias",
                    "match_faa_type_certificate_family", "unresolved"
                ],
                "description": "Relates the exact retained model/family label to the selected canonical family; spelling or designation-prefix similarity never proves this relationship. match_manufacturer_series_family is permitted only when exact current FAA/TCDS designation+serial proof exists, no named-family TCDS projection exists, the paired retained variant equals only that exact designation, the complete retained model is exactly its numeric series stem plus the canonical family in either adjacent order, and cited direct-primary OEM hierarchy evidence co-names those same adjacent components; it is case-bound and never an alias. match_faa_type_certificate_family is required exactly when a case-bound server FAA DRS projection proves the exact FAA designation/FAA-matched serial and its exact heading explicitly names the canonical family. The retained label remains audit input and is not claimed as a TCDS heading; neither case-bound action is a catalog alias or year interval"
            },
            "observed_family_label": {
                "type": "string",
                "description": "Copy the complete retained model/family field exactly; do not normalize, shorten, or derive it from the FAA designation"
            },
            "canonical_family_name": {
                "type": "string",
                "description": "Must literally equal the selected family display_name"
            },
            "existing_alias_id": {"type": ["integer", "null"], "minimum": 1},
            "valid_from_model_year": {
                "type": ["integer", "null"], "minimum": 1900, "maximum": 2200,
                "description": "Required and finite for propose_alias, with this exact year token present in cited direct-primary production_applicability evidence; copy exactly for match_approved_alias"
            },
            "valid_to_model_year": {
                "type": ["integer", "null"], "minimum": 1900, "maximum": 2200,
                "description": "Required and finite for propose_alias, with this exact year token present in cited direct-primary production_applicability evidence; copy exactly for match_approved_alias"
            },
            "evidence_ids": {
                "type": "array",
                "items": {"type": "string"},
                "description": "match_manufacturer_series_family requires only direct-primary OEM hierarchy claims whose exact excerpts co-name the numeric FAA series stem and canonical family as adjacent components in either order. A proposed alias requires direct-primary OEM hierarchy evidence that co-names the complete observed and canonical labels in the same exact excerpt; exact_canonical_label requires an empty list; match_faa_type_certificate_family requires exactly every server_faa_drs.* claim ID from the case-bound TCDS binding, with none omitted, added, or substituted"
            },
            "applicability_evidence_ids": {
                "type": "array",
                "items": {"type": "string"},
                "description": "A proposed alias requires direct-primary production_applicability evidence covering the listing model year and explicitly containing both finite boundary years as exact tokens; exact_canonical_label, match_manufacturer_series_family, and match_faa_type_certificate_family require an empty list"
            },
            "rationale": {"type": "string"}
        },
        "required": [
            "action", "observed_family_label", "canonical_family_name",
            "existing_alias_id", "valid_from_model_year", "valid_to_model_year",
            "evidence_ids", "applicability_evidence_ids", "rationale"
        ]
    });
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "confidence": confidence_schema(),
            "make": entity.clone(),
            "faa_make_relationship": faa_make_relationship,
            "family": entity.clone(),
            "family_label_relationship": family_label_relationship,
            "designation": entity.clone(),
            "generation": entity.clone(),
            "package": entity,
            "material_distinctions": {"type": "array", "items": {"type": "string"}},
            "unresolved_questions": {"type": "array", "items": {"type": "string"}},
            "rationale": {"type": "string"}
        },
        "required": [
            "confidence", "make", "faa_make_relationship", "family",
            "family_label_relationship", "designation", "generation", "package",
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
    use crate::aircraft::faa::{AircraftReference, SerialMatch};

    #[test]
    fn tcds_holder_equality_tolerates_only_case_whitespace_and_terminal_periods() {
        assert!(tcds_holder_names_match(
            "TEXTRON AVIATION INC",
            " Textron Aviation Inc. "
        ));
        assert!(!tcds_holder_names_match(
            "TEXTRON AVIATION",
            "Textron Aviation Inc."
        ));
        assert!(!tcds_holder_names_match(
            "TEXTRON-AVIATION INC",
            "Textron Aviation Inc."
        ));
        assert!(!tcds_holder_names_match(
            "CESSNA AIRCRAFT COMPANY",
            "Textron Aviation Inc."
        ));
    }

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

    fn server_evidence(make: &str, model: &str) -> ServerFaaIdentityEvidence {
        server_evidence_with_variant(make, model, model)
    }

    fn server_evidence_with_variant(
        make: &str,
        model: &str,
        observed_variant: &str,
    ) -> ServerFaaIdentityEvidence {
        let observed_model = if model == "182T" { "182" } else { model };
        server_evidence_with_observed_model(make, model, observed_model, observed_variant)
    }

    fn server_evidence_with_observed_model(
        make: &str,
        model: &str,
        observed_model: &str,
        observed_variant: &str,
    ) -> ServerFaaIdentityEvidence {
        let snapshot = Snapshot {
            id: 2,
            evidence_source_id: 7,
            snapshot_date: "2026-07-20".to_string(),
            source_url: crate::aircraft::faa::RELEASE_SOURCE_URL.to_string(),
            archive_sha256: "a".repeat(64),
            source_manifest_sha256: "b".repeat(64),
            target_set_sha256: "d".repeat(64),
            record_hash_domain: crate::aircraft::faa::AIRCRAFT_RECORD_HASH_DOMAIN.to_string(),
        };
        let grounding = AircraftGrounding {
            snapshot: snapshot.clone(),
            n_number: "N89225".to_string(),
            manufacturer_serial_raw: Some("18283169".to_string()),
            manufacturer_serial_key: Some("18283169".to_string()),
            aircraft_code: "2072723".to_string(),
            engine_code: None,
            source_record_sha256: "c".repeat(64),
            year_manufactured: Some(2022),
            aircraft: Some(AircraftReference {
                aircraft_code: "2072723".to_string(),
                manufacturer_name: Some(make.to_string()),
                model_name: Some(model.to_string()),
                aircraft_type_code: None,
                engine_type_code: None,
                category_code: None,
                certification_indicator_code: None,
                engine_count: None,
                seat_count: None,
                weight_class_code: None,
                cruise_speed_mph: None,
                type_certificate_data_sheet: None,
                type_certificate_holder: None,
            }),
            engine: None,
            serial_match: SerialMatch::RawExact,
        };
        ServerFaaIdentityEvidence::new(
            "faa_case_test",
            snapshot,
            vec![ServerFaaObservationBinding::new(
                23,
                "e".repeat(64),
                make,
                observed_model,
                observed_variant,
                2022,
                grounding,
            )],
            make,
            model,
        )
        .unwrap()
    }

    fn attach_test_tcds(server: &mut ServerFaaIdentityEvidence, family: &str) {
        use crate::aircraft::curation::regulator::{SelectedTcdsExcerpt, TcdsSerialEligibility};

        let observed_model = server.observation_bindings[0].observed_model.clone();
        let exact_faa_model = server.faa_model_designation().to_string();
        let faa_serial_key = server.observation_bindings[0]
            .grounding
            .manufacturer_serial_key
            .clone()
            .expect("test server has an FAA manufacturer serial");
        let excerpt = |page_number, text: &str| SelectedTcdsExcerpt {
            page_number,
            excerpt: text.to_string(),
            normalized_excerpt_sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        };
        let model_heading = format!("Model {exact_faa_model}, {family}, 4 PCLM (Normal Category).");
        let serial_excerpt =
            format!("Serial Numbers Eligible {exact_faa_model}: {faa_serial_key} and On");
        let binding = TcdsFamilyBinding {
            document_guid: "01234567-89ab-cdef-0123-456789abcdef".to_string(),
            document_url: "https://drs.faa.gov/browse/TCDSMODEL/3A13".to_string(),
            tcds_number: "3A13".to_string(),
            revision_number: Some("75".to_string()),
            revision_date: Some("2024-08-07".to_string()),
            source_url: concat!(
                "https://drs.faa.gov/api/drs/data-pull/download/",
                "01234567-89ab-cdef-0123-456789abcdef"
            )
            .to_string(),
            pdf_sha256: "6".repeat(64),
            exact_faa_model: exact_faa_model.clone(),
            observed_model,
            canonical_family_name: family.to_string(),
            faa_serial_key: faa_serial_key.clone(),
            faa_model_heading: excerpt(34, &model_heading),
            serial_eligibility: TcdsSerialEligibility {
                page_number: 35,
                excerpt: serial_excerpt.clone(),
                normalized_excerpt_sha256: format!(
                    "{:x}",
                    Sha256::digest(serial_excerpt.as_bytes())
                ),
                model: exact_faa_model,
                first_serial_key: faa_serial_key,
                last_serial_key: None,
            },
        };
        server
            .attach_tcds_identity_binding(binding.identity_binding())
            .unwrap();
        server.attach_tcds_family_binding(binding).unwrap();
    }

    fn attach_familyless_test_tcds(server: &mut ServerFaaIdentityEvidence) {
        use crate::aircraft::curation::regulator::{
            SelectedTcdsExcerpt, TcdsIdentityBinding, TcdsSerialEligibility,
        };

        let exact_faa_model = server.faa_model_designation().to_string();
        let faa_serial_key = server.observation_bindings[0]
            .grounding
            .manufacturer_serial_key
            .clone()
            .expect("test server has an FAA manufacturer serial");
        let model_heading =
            format!("Model {exact_faa_model}, 4 PCLM (Normal Category), Approved August 29, 1980");
        let serial_excerpt =
            format!("Serial Numbers Eligible {exact_faa_model}: {faa_serial_key} and On");
        server
            .attach_tcds_identity_binding(TcdsIdentityBinding {
                document_guid: "01234567-89ab-cdef-0123-456789abcdef".to_string(),
                document_url: "https://drs.faa.gov/browse/TCDSMODEL/3A13".to_string(),
                tcds_number: "3A13".to_string(),
                revision_number: Some("75".to_string()),
                revision_date: Some("2024-08-07".to_string()),
                source_url: concat!(
                    "https://drs.faa.gov/api/drs/data-pull/download/",
                    "01234567-89ab-cdef-0123-456789abcdef"
                )
                .to_string(),
                pdf_sha256: "6".repeat(64),
                exact_faa_model: exact_faa_model.clone(),
                faa_serial_key: faa_serial_key.clone(),
                faa_model_heading: SelectedTcdsExcerpt {
                    page_number: 19,
                    excerpt: model_heading.clone(),
                    normalized_excerpt_sha256: format!(
                        "{:x}",
                        Sha256::digest(model_heading.as_bytes())
                    ),
                },
                serial_eligibility: TcdsSerialEligibility {
                    page_number: 20,
                    excerpt: serial_excerpt.clone(),
                    normalized_excerpt_sha256: format!(
                        "{:x}",
                        Sha256::digest(serial_excerpt.as_bytes())
                    ),
                    model: exact_faa_model,
                    first_serial_key: faa_serial_key,
                    last_serial_key: None,
                },
            })
            .unwrap();
        server
            .attach_tcds_selection_basis(TcdsSelectionBasis::OperatorValidatedExactModelSerial)
            .unwrap();
    }

    fn attach_test_tcds_lineage(server: &mut ServerFaaIdentityEvidence) {
        use crate::aircraft::curation::regulator::{
            TcdsHolderTransferEvidence, TcdsMakeLineageEvidence,
        };

        attach_test_tcds(server, "Skylane");
        server
            .attach_tcds_selection_basis(TcdsSelectionBasis::OperatorValidatedExactModelSerial)
            .unwrap();
        let identity = server
            .tcds_identity_binding
            .as_ref()
            .expect("test TCDS identity")
            .clone();
        let holder_excerpt = concat!(
            "Type Certificate Holder Record Cessna Aircraft Company transferred to ",
            "Textron Aviation Inc. on July 29, 2015"
        );
        server
            .attach_tcds_make_lineage_evidence(TcdsMakeLineageEvidence {
                document_guid: identity.document_guid,
                tcds_number: identity.tcds_number,
                source_url: identity.source_url,
                pdf_sha256: identity.pdf_sha256,
                exact_faa_model: identity.exact_faa_model,
                faa_serial_key: identity.faa_serial_key,
                manufacturer_serial_eligibility: None,
                holder_transfer: Some(TcdsHolderTransferEvidence {
                    page_number: 1,
                    excerpt: holder_excerpt.to_string(),
                    normalized_excerpt_sha256: format!(
                        "{:x}",
                        Sha256::digest(holder_excerpt.as_bytes())
                    ),
                    former_holder_name: "Cessna Aircraft Company".to_string(),
                    current_holder_name: "Textron Aviation Inc.".to_string(),
                    effective_date_text: "July 29, 2015".to_string(),
                }),
            })
            .unwrap();
    }

    #[test]
    fn tcds_identity_and_regulator_complete_require_every_observation_serial() {
        let mut source =
            server_evidence_with_observed_model("CESSNA", "182T", "182", "182T Skylane");
        attach_test_tcds_lineage(&mut source);
        let identity = source
            .tcds_identity_binding
            .clone()
            .expect("test source has an exact TCDS identity");

        let mut missing_serial =
            server_evidence_with_observed_model("CESSNA", "182T", "182", "182T Skylane");
        let mut second = missing_serial.observation_bindings[0].clone();
        second.listing_id += 1;
        second.observation_sha256 = "f".repeat(64);
        second.grounding.manufacturer_serial_key = None;
        missing_serial.observation_bindings.push(second);
        assert!(
            missing_serial
                .attach_tcds_identity_binding(identity)
                .is_err(),
            "one exact serial must not hide another observation with no serial"
        );

        source.observation_bindings[0]
            .grounding
            .manufacturer_serial_key = None;
        assert!(
            source.regulator_complete_research().is_none(),
            "regulator-complete mode must recheck every retained FAA serial"
        );
    }

    fn exact_server_tcds_adjudication(
        server: &ServerFaaIdentityEvidence,
    ) -> AircraftHierarchyAdjudication {
        let identity_ids = server
            .tcds_identity_claim_ids()
            .expect("test server has TCDS identity claims");
        let family_relationship = server
            .tcds_family_relationship("Skylane")
            .expect("test server has a named TCDS family");
        let make_relationship = if server.faa_manufacturer_name() == "TEXTRON AVIATION INC" {
            exact_relationship(server)
        } else {
            let lineage_ids = server
                .tcds_make_lineage_claim_ids()
                .expect("test server has TCDS lineage claims");
            FaaMakeRelationshipDecision {
                action: FaaMakeRelationshipAction::MatchTcdsMakeLineage,
                faa_manufacturer_name: server.faa_manufacturer_name().to_string(),
                canonical_make_name: "TEXTRON AVIATION INC".to_string(),
                existing_alias_id: None,
                valid_from_model_year: None,
                valid_to_model_year: None,
                evidence_ids: std::iter::once(server.make_claim_id().to_string())
                    .chain(lineage_ids.identity().into_iter().map(str::to_string))
                    .collect(),
                applicability_evidence_ids: lineage_ids
                    .applicability()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                rationale: "fixture uses exact TCDS holder lineage".to_string(),
            }
        };
        AircraftHierarchyAdjudication {
            confidence: CurationConfidence::VeryHigh,
            make: entity_with_evidence(
                EntityResolutionAction::MatchExisting,
                Some(1),
                Some(make_relationship.canonical_make_name.as_str()),
                vec![server.make_claim_id().to_string()],
            ),
            faa_make_relationship: make_relationship,
            family: entity_with_evidence(
                EntityResolutionAction::MatchExisting,
                Some(1),
                Some("Skylane"),
                identity_ids.hierarchy(),
            ),
            family_label_relationship: family_relationship,
            designation: entity_with_evidence(
                EntityResolutionAction::MatchExisting,
                Some(1),
                Some(server.faa_model_designation()),
                std::iter::once(server.designation_claim_id().to_string())
                    .chain(identity_ids.all().into_iter().map(str::to_string))
                    .collect(),
            ),
            generation: entity(EntityResolutionAction::NoSupportedSelection, None, None),
            package: entity(EntityResolutionAction::NoSupportedSelection, None, None),
            material_distinctions: vec!["182T remains distinct from T182T".to_string()],
            unresolved_questions: Vec::new(),
            rationale: "fixture is fully bound to exact FAA registry and TCDS evidence".to_string(),
        }
    }

    #[test]
    fn server_faa_only_verification_scope_uses_selected_claims_and_fails_closed() {
        let mut server = server_evidence("CESSNA", "182T");
        attach_test_tcds_lineage(&mut server);
        let adjudication = exact_server_tcds_adjudication(&server);
        let tcds_ids = server
            .tcds_family_claim_ids()
            .expect("fixture has a named TCDS family");
        let mut research = AircraftIdentityEvidenceResearch {
            subject_summary: "untrusted discovery summary mentions an unrelated publisher"
                .to_string(),
            claims: server
                .claims()
                .iter()
                .cloned()
                .chain(std::iter::once(claim("unused-web-discovery")))
                .collect(),
            family_candidates: vec![HierarchyCandidate {
                label: "Skylane".to_string(),
                evidence_ids: tcds_ids.hierarchy(),
            }],
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: Vec::new(),
            unresolved_questions: Vec::new(),
        };

        let selected = server_faa_only_verification_evidence_ids(&research, &server, &adjudication)
            .expect("an unused web-discovery claim must not poison exact server verification");
        assert!(!selected.contains("unused-web-discovery"));
        assert!(selected
            .iter()
            .all(|id| id.starts_with("server_faa_registry.") || id.starts_with("server_faa_drs.")));

        research.contradictions = vec!["the FAA registry and exact TCDS disagree".to_string()];
        assert!(
            server_faa_only_verification_evidence_ids(&research, &server, &adjudication).is_none(),
            "an actual contradiction must keep the fail-closed grounded path"
        );
        research.contradictions.clear();

        let mut web_selected = adjudication.clone();
        web_selected
            .family
            .evidence_ids
            .push("unused-web-discovery".to_string());
        assert!(
            server_faa_only_verification_evidence_ids(&research, &server, &web_selected).is_none(),
            "a selected web claim must retain ordinary direct-source verification"
        );

        server.tcds_selection_basis = None;
        assert!(
            server_faa_only_verification_evidence_ids(&research, &server, &adjudication).is_none(),
            "an incomplete TCDS provenance binding must never enter server-only verification"
        );
    }

    #[test]
    fn tcds_attachment_replaces_model_family_projection_with_exact_hierarchy_claims() {
        let mut server = server_evidence("TEXTRON AVIATION INC", "182T");
        attach_test_tcds(&mut server, "Skylane");
        let mut applicability = claim("claim_production_applicability");
        applicability.supports = [EvidenceClaimKind::ProductionApplicability]
            .into_iter()
            .collect();
        let mut research = AircraftIdentityEvidenceResearch {
            subject_summary: "Cessna 182T Skylane".to_string(),
            claims: vec![applicability],
            family_candidates: vec![
                HierarchyCandidate {
                    label: "182 Skylane".to_string(),
                    evidence_ids: vec!["claim_production_applicability".to_string()],
                },
                HierarchyCandidate {
                    label: "Skylane".to_string(),
                    evidence_ids: vec!["claim_production_applicability".to_string()],
                },
            ],
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: vec!["pre-existing contradiction".to_string()],
            unresolved_questions: vec![ResearchUnresolvedQuestion {
                scope: ResearchUnresolvedScope::Other,
                question: "pre-existing unresolved question".to_string(),
            }],
        };

        server.attach_to(&mut research).unwrap();

        assert_eq!(research.family_candidates.len(), 1);
        assert_eq!(research.family_candidates[0].label, "Skylane");
        assert_eq!(
            research.contradictions,
            vec!["pre-existing contradiction".to_string()]
        );
        assert_eq!(
            research.unresolved_questions,
            vec![ResearchUnresolvedQuestion {
                scope: ResearchUnresolvedScope::Other,
                question: "pre-existing unresolved question".to_string(),
            }]
        );
        assert_eq!(
            research.family_candidates[0].evidence_ids,
            server
                .tcds_family_claim_ids()
                .expect("test TCDS has exact claims")
                .hierarchy()
        );
        assert!(
            research
                .claims
                .iter()
                .any(|claim| claim.evidence_id == "claim_production_applicability"),
            "supplementary evidence remains available for decisions other than family identity"
        );
    }

    #[test]
    fn tcds_family_projection_audits_arbitrary_retained_label_without_claiming_a_heading() {
        let mut server = server_evidence_with_observed_model(
            "TEXTRON AVIATION INC",
            "182T",
            "182 SKYLANE",
            "182T",
        );
        attach_test_tcds(&mut server, "Skylane");

        assert_eq!(server.claims().len(), 4);
        assert!(server.claims().iter().all(|claim| {
            !claim.evidence_id.contains("observed_model_heading")
                && !claim.evidence_excerpt.starts_with("Model 182 SKYLANE")
        }));
        let relationship = server.tcds_family_relationship("Skylane").unwrap();
        assert_eq!(relationship.observed_family_label, "182 SKYLANE");
        assert_eq!(relationship.evidence_ids.len(), 2);
    }

    #[test]
    fn familyless_tcds_identity_keeps_oem_family_research_authoritative() {
        use crate::aircraft::curation::regulator::{
            SelectedTcdsExcerpt, TcdsIdentityBinding, TcdsSerialEligibility,
        };

        let mut server = server_evidence("TEXTRON AVIATION INC", "182R");
        let heading = "Model 182R, 4 PCLM (Normal Category), Approved August 29, 1980";
        let serial = "Serial Nos. Eligible Model 182R: 18260000 through 18289999";
        server
            .attach_tcds_identity_binding(TcdsIdentityBinding {
                document_guid: "01234567-89ab-cdef-0123-456789abcdef".to_string(),
                document_url: "https://drs.faa.gov/browse/TCDSMODEL/3A13".to_string(),
                tcds_number: "3A13".to_string(),
                revision_number: Some("75".to_string()),
                revision_date: Some("2024-08-07".to_string()),
                source_url: concat!(
                    "https://drs.faa.gov/api/drs/data-pull/download/",
                    "01234567-89ab-cdef-0123-456789abcdef"
                )
                .to_string(),
                pdf_sha256: "6".repeat(64),
                exact_faa_model: "182R".to_string(),
                faa_serial_key: "18283169".to_string(),
                faa_model_heading: SelectedTcdsExcerpt {
                    page_number: 19,
                    excerpt: heading.to_string(),
                    normalized_excerpt_sha256: format!("{:x}", Sha256::digest(heading.as_bytes())),
                },
                serial_eligibility: TcdsSerialEligibility {
                    page_number: 20,
                    excerpt: serial.to_string(),
                    normalized_excerpt_sha256: format!("{:x}", Sha256::digest(serial.as_bytes())),
                    model: "182R".to_string(),
                    first_serial_key: "18260000".to_string(),
                    last_serial_key: Some("18289999".to_string()),
                },
            })
            .unwrap();
        let mut research = AircraftIdentityEvidenceResearch {
            subject_summary: "Cessna 182R".to_string(),
            claims: vec![claim("oem_family")],
            family_candidates: vec![HierarchyCandidate {
                label: "Skylane".to_string(),
                evidence_ids: vec!["oem_family".to_string()],
            }],
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: Vec::new(),
            unresolved_questions: Vec::new(),
        };

        server.attach_to(&mut research).unwrap();

        assert!(server.tcds_identity_binding.is_some());
        assert!(server.tcds_family_binding.is_none());
        assert_eq!(server.tcds_identity_claim_ids().unwrap().all().len(), 2);
        assert_eq!(research.family_candidates.len(), 1);
        assert_eq!(research.family_candidates[0].label, "Skylane");
    }

    #[test]
    fn server_attachment_prunes_only_known_mis_scoped_candidate_evidence() {
        let mut server = server_evidence_with_observed_model(
            "TEXTRON AVIATION INC",
            "182R",
            "182 Skylane",
            "182R",
        );
        attach_familyless_test_tcds(&mut server);
        let mut hierarchy = claim("hierarchy");
        hierarchy.evidence_excerpt =
            "Today the manufacturer celebrates the Cessna Skylane 182.".to_string();
        let mut production_only = claim("production_only");
        production_only.supports = [EvidenceClaimKind::ProductionApplicability]
            .into_iter()
            .collect();
        production_only.evidence_excerpt =
            "More than 23,000 Skylane aircraft have been delivered since 1956.".to_string();
        let mut secondary_hierarchy = claim("secondary_hierarchy");
        secondary_hierarchy.source_kind = EvidenceSourceKind::RecognizedSecondary;
        secondary_hierarchy.evidence_excerpt =
            "A secondary reference calls the aircraft Skylane 182.".to_string();
        let mut research = AircraftIdentityEvidenceResearch {
            subject_summary: "familyless 182R structure fixture".to_string(),
            claims: vec![hierarchy, production_only, secondary_hierarchy],
            family_candidates: vec![HierarchyCandidate {
                label: "Skylane".to_string(),
                evidence_ids: vec![
                    "hierarchy".to_string(),
                    "production_only".to_string(),
                    "secondary_hierarchy".to_string(),
                    "unknown".to_string(),
                ],
            }],
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: vec!["a real publisher disagreement remains".to_string()],
            unresolved_questions: vec![
                ResearchUnresolvedQuestion {
                    scope: ResearchUnresolvedScope::Designation,
                    question: "The OEM history page does not repeat 182R.".to_string(),
                },
                ResearchUnresolvedQuestion {
                    scope: ResearchUnresolvedScope::SourceIntegrity,
                    question: "A required publisher span could not be verified.".to_string(),
                },
            ],
        };

        server.attach_to(&mut research).unwrap();

        assert_eq!(
            research.family_candidates[0].evidence_ids,
            vec![
                "hierarchy".to_string(),
                "secondary_hierarchy".to_string(),
                "unknown".to_string()
            ],
            "only a known non-hierarchy reference is pruned; hierarchy-typed secondary and unknown ids remain fail-closed"
        );
        let mut candidate_issues = Vec::new();
        validate_hierarchy_candidates(
            HierarchyEntityKind::Family,
            &research.family_candidates,
            &research,
            &server,
            &mut candidate_issues,
        );
        assert!(candidate_issues.iter().any(|issue| {
            issue.code == "hierarchy_candidate_non_primary_evidence"
                && issue.message.contains("secondary_hierarchy")
        }));
        assert_eq!(
            research.unresolved_questions,
            vec![
                ResearchUnresolvedQuestion {
                    scope: ResearchUnresolvedScope::Designation,
                    question: "The OEM history page does not repeat 182R.".to_string(),
                },
                ResearchUnresolvedQuestion {
                    scope: ResearchUnresolvedScope::SourceIntegrity,
                    question: "A required publisher span could not be verified.".to_string(),
                },
            ],
            "attachment never erases model-authored unresolved questions"
        );
        assert_eq!(
            research.contradictions,
            vec!["a real publisher disagreement remains".to_string()]
        );
        assert!(research
            .claims
            .iter()
            .any(|claim| claim.evidence_id == "production_only"));
    }

    #[test]
    fn server_attachment_rejects_reserved_ids_in_every_model_owned_candidate_kind() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        for (kind, reserved_id) in [
            (
                HierarchyEntityKind::Family,
                "server_faa_registry.make.model_owned",
            ),
            (
                HierarchyEntityKind::Generation,
                "server_faa_drs.faa_model_heading.model_owned",
            ),
            (
                HierarchyEntityKind::Package,
                "server_faa_registry.designation.model_owned",
            ),
        ] {
            let candidate = HierarchyCandidate {
                label: "model-owned candidate".to_string(),
                evidence_ids: vec![reserved_id.to_string()],
            };
            let mut research = AircraftIdentityEvidenceResearch {
                subject_summary: "reserved candidate reference fixture".to_string(),
                claims: Vec::new(),
                family_candidates: (kind == HierarchyEntityKind::Family)
                    .then_some(candidate.clone())
                    .into_iter()
                    .collect(),
                generation_candidates: (kind == HierarchyEntityKind::Generation)
                    .then_some(candidate.clone())
                    .into_iter()
                    .collect(),
                package_candidates: (kind == HierarchyEntityKind::Package)
                    .then_some(candidate)
                    .into_iter()
                    .collect(),
                contradictions: Vec::new(),
                unresolved_questions: Vec::new(),
            };

            let error = server
                .attach_to(&mut research)
                .expect_err("model-owned candidates must never pre-bind server evidence ids");
            assert!(error.contains("hierarchy-candidate evidence id reserved"));
            assert!(
                research.claims.is_empty(),
                "reserved candidate ids must be rejected before server claims are injected"
            );
        }
    }

    #[test]
    fn familyless_drs_model_heading_cannot_prove_a_family_candidate() {
        let mut server =
            server_evidence_with_observed_model("TEXTRON AVIATION INC", "182R", "182", "182R");
        attach_familyless_test_tcds(&mut server);
        let model_heading_id = server
            .tcds_identity_claim_ids()
            .expect("familyless fixture has exact TCDS identity claims")
            .faa_model_heading;
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "familyless DRS family misuse fixture".to_string(),
            claims: server.claims().to_vec(),
            family_candidates: vec![HierarchyCandidate {
                label: "182R".to_string(),
                evidence_ids: vec![model_heading_id.clone()],
            }],
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: Vec::new(),
            unresolved_questions: Vec::new(),
        };
        let mut issues = Vec::new();

        validate_hierarchy_candidates(
            HierarchyEntityKind::Family,
            &research.family_candidates,
            &research,
            &server,
            &mut issues,
        );

        assert!(issues.iter().any(|issue| {
            issue.code == "hierarchy_candidate_non_primary_evidence"
                && issue.message.contains(&model_heading_id)
        }));
    }

    #[test]
    fn manufacturer_series_family_relationship_resolves_familyless_182r_composition() {
        let mut server = server_evidence_with_observed_model(
            "TEXTRON AVIATION INC",
            "182R",
            "182 Skylane",
            "182R",
        );
        attach_familyless_test_tcds(&mut server);
        let mut research = research_with_server(&server, "Skylane");
        research.claims[0].evidence_excerpt =
            "Today, Textron Aviation celebrates 65 years of the Cessna Skylane 182.".to_string();
        let mut adjudication = base_adjudication(&server, "Skylane");
        adjudication.family_label_relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::MatchManufacturerSeriesFamily,
            observed_family_label: "182 Skylane".to_string(),
            canonical_family_name: "Skylane".to_string(),
            existing_alias_id: None,
            valid_from_model_year: None,
            valid_to_model_year: None,
            evidence_ids: vec!["identity".to_string()],
            applicability_evidence_ids: Vec::new(),
            rationale:
                "exact serial-bound 182R and OEM Skylane 182 evidence bind the two components"
                    .to_string(),
        };
        adjudication.designation.evidence_ids.extend(
            server
                .tcds_identity_claim_ids()
                .expect("familyless fixture has exact identity claims")
                .all()
                .into_iter()
                .map(str::to_string),
        );

        let proposal = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect("exact paired designation and OEM series/family co-naming resolve the composition");
        assert_eq!(proposal.model_family.display_name, "Skylane");
        assert_eq!(proposal.certified_variant.display_name, "182R");
        assert!(proposal.generation.is_none());
        assert!(proposal.tier.is_none());

        let mut alias_shaped = adjudication.family_label_relationship.clone();
        alias_shaped.valid_from_model_year = Some(1981);
        let mut issues = Vec::new();
        assert!(!validate_family_label_relationship(
            &alias_shaped,
            &adjudication.make,
            &adjudication.family,
            &research,
            &server,
            &exact_empty_catalog(&server),
            &mut issues,
        ));
        assert!(issues
            .iter()
            .any(|issue| { issue.code == "family_label_manufacturer_series_has_alias_fields" }));
    }

    #[test]
    fn manufacturer_series_family_composition_is_exact_and_never_erases_suffixes() {
        assert!(exact_series_family_composition(
            "182 Skylane",
            "182R",
            "Skylane"
        ));
        assert!(exact_series_family_composition(
            "Skylane 182",
            "182R",
            "Skylane"
        ));
        for (observed, designation, family) in [
            ("182 Skylane G6", "182R", "Skylane"),
            ("182T Skylane", "182R", "Skylane"),
            ("182 SkylaneX", "182R", "Skylane"),
            ("182 Skylane", "T182T", "Skylane"),
            ("SR22 Cirrus", "SR22T", "Cirrus"),
        ] {
            assert!(
                !exact_series_family_composition(observed, designation, family),
                "{observed:?} must not be accepted for {designation:?}/{family:?}"
            );
        }
        assert!(excerpt_conames_exact_series_and_family(
            "Textron celebrates the Cessna Skylane 182.",
            "182R",
            "Skylane"
        ));
        assert!(excerpt_conames_exact_series_and_family(
            "The Cessna 182 Skylane remains supported.",
            "182R",
            "Skylane"
        ));
        assert!(!excerpt_conames_exact_series_and_family(
            "The Cessna Skylane 182T remains supported.",
            "182R",
            "Skylane"
        ));
    }

    #[test]
    fn tcds_attachment_preserves_and_blocks_a_distinct_primary_family_candidate() {
        let mut server = server_evidence("TEXTRON AVIATION INC", "182T");
        attach_test_tcds(&mut server, "Skylane");
        let mut different_family = claim("different_family");
        different_family.evidence_excerpt =
            "The manufacturer identifies Cardinal as the aircraft family.".to_string();
        let different_family_url = different_family.source_url.clone();
        let mut research = AircraftIdentityEvidenceResearch {
            subject_summary: "conflicting family fixture".to_string(),
            claims: vec![different_family],
            family_candidates: vec![HierarchyCandidate {
                label: "Cardinal".to_string(),
                evidence_ids: vec!["different_family".to_string()],
            }],
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: Vec::new(),
            unresolved_questions: Vec::new(),
        };

        server.attach_to(&mut research).unwrap();

        assert_eq!(
            research
                .family_candidates
                .iter()
                .map(|candidate| candidate.label.as_str())
                .collect::<BTreeSet<_>>(),
            ["Cardinal", "Skylane"].into_iter().collect()
        );
        let grounding = GroundingAudit {
            mode: GroundingMode::FreshWeb,
            google_search_call_count: 1,
            url_context_call_count: 1,
            citation_urls: [different_family_url].into_iter().collect(),
            reused_verified_dossier: false,
        };
        let errors = validate_identity_evidence_research(&research, &grounding, &server)
            .expect_err("a distinct primary family must conflict with exact TCDS identity");
        assert!(errors
            .0
            .iter()
            .any(|issue| issue.code == "tcds_family_candidate_conflict"));
    }

    #[test]
    fn tcds_family_relationship_accepts_only_the_exact_case_bound_claim_set() {
        let mut server = server_evidence("TEXTRON AVIATION INC", "182T");
        attach_test_tcds(&mut server, "Skylane");
        let research = research_with_server(&server, "Skylane");
        let make = entity(
            EntityResolutionAction::ProposeNew,
            None,
            Some(server.faa_manufacturer_name()),
        );
        let family = entity(EntityResolutionAction::ProposeNew, None, Some("Skylane"));
        let catalog = exact_empty_catalog(&server);
        let relationship = server
            .tcds_family_relationship("Skylane")
            .expect("the exact TCDS binding produces the case-bound relationship");
        let mut issues = Vec::new();

        assert!(validate_family_label_relationship(
            &relationship,
            &make,
            &family,
            &research,
            &server,
            &catalog,
            &mut issues,
        ));
        assert!(issues.is_empty(), "exact binding should pass: {issues:?}");

        let mut missing_claim = relationship.clone();
        missing_claim.evidence_ids.pop();
        issues.clear();
        assert!(!validate_family_label_relationship(
            &missing_claim,
            &make,
            &family,
            &research,
            &server,
            &catalog,
            &mut issues,
        ));
        assert!(issues
            .iter()
            .any(|issue| issue.code == "family_label_type_certificate_evidence_mismatch"));

        let mut duplicated_claim = relationship.clone();
        duplicated_claim
            .evidence_ids
            .push(duplicated_claim.evidence_ids[0].clone());
        issues.clear();
        assert!(!validate_family_label_relationship(
            &duplicated_claim,
            &make,
            &family,
            &research,
            &server,
            &catalog,
            &mut issues,
        ));
        assert!(issues
            .iter()
            .any(|issue| issue.code == "family_label_type_certificate_evidence_mismatch"));

        let mut alias_shaped = relationship.clone();
        alias_shaped.valid_from_model_year = Some(2022);
        issues.clear();
        assert!(!validate_family_label_relationship(
            &alias_shaped,
            &make,
            &family,
            &research,
            &server,
            &catalog,
            &mut issues,
        ));
        assert!(issues
            .iter()
            .any(|issue| issue.code == "family_label_type_certificate_has_alias_fields"));

        let unbound_server = server_evidence("TEXTRON AVIATION INC", "182T");
        issues.clear();
        assert!(!validate_family_label_relationship(
            &relationship,
            &make,
            &family,
            &research,
            &unbound_server,
            &exact_empty_catalog(&unbound_server),
            &mut issues,
        ));
        assert!(issues
            .iter()
            .any(|issue| issue.code == "family_label_type_certificate_evidence_mismatch"));
    }

    fn research_with_server(
        server: &ServerFaaIdentityEvidence,
        family: &str,
    ) -> AircraftIdentityEvidenceResearch {
        let mut research = AircraftIdentityEvidenceResearch {
            subject_summary: format!(
                "{} {}",
                server.faa_manufacturer_name(),
                server.faa_model_designation()
            ),
            claims: vec![claim("identity")],
            family_candidates: vec![HierarchyCandidate {
                label: family.to_string(),
                evidence_ids: vec!["identity".to_string()],
            }],
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: vec![],
            unresolved_questions: vec![],
        };
        research.claims[0].evidence_excerpt =
            format!("The manufacturer identifies {family} as the aircraft family.");
        server.attach_to(&mut research).unwrap();
        research
    }

    fn normalized_test_span(value: &str) -> String {
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

    fn fetched_source_proofs(
        research: &AircraftIdentityEvidenceResearch,
        server: &ServerFaaIdentityEvidence,
    ) -> ServerFetchedAircraftSourceProofs {
        let proofs = research
            .claims
            .iter()
            .filter(|claim| !server.contains_exact_claim(claim))
            .map(|claim| {
                let normalized_span = normalized_test_span(&claim.evidence_excerpt);
                SourceEvidenceProof {
                    final_url: claim.source_url.clone(),
                    content_sha256: format!("{:x}", Sha256::digest(claim.source_url.as_bytes())),
                    evidence_spans: vec![SourceEvidenceSpanProof {
                        span_sha256: format!("{:x}", Sha256::digest(normalized_span.as_bytes())),
                        normalized_span,
                    }],
                }
            })
            .collect::<Vec<_>>();
        ServerFetchedAircraftSourceProofs::bind_research(research, server, &proofs)
            .expect("test research has exact fetched source proofs")
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
            evidence_ids: if action == EntityResolutionAction::NoSupportedSelection {
                Vec::new()
            } else {
                vec!["identity".to_string()]
            },
            rationale: "supported by primary identity evidence".to_string(),
        }
    }

    fn entity_with_evidence(
        action: EntityResolutionAction,
        id: Option<i64>,
        name: Option<&str>,
        evidence_ids: Vec<String>,
    ) -> CatalogEntityDecision {
        CatalogEntityDecision {
            evidence_ids,
            ..entity(action, id, name)
        }
    }

    fn exact_relationship(server: &ServerFaaIdentityEvidence) -> FaaMakeRelationshipDecision {
        FaaMakeRelationshipDecision {
            action: FaaMakeRelationshipAction::ExactCanonicalLabel,
            faa_manufacturer_name: server.faa_manufacturer_name().to_string(),
            canonical_make_name: server.faa_manufacturer_name().to_string(),
            existing_alias_id: None,
            valid_from_model_year: None,
            valid_to_model_year: None,
            evidence_ids: vec![server.make_claim_id().to_string()],
            applicability_evidence_ids: vec![],
            rationale: "canonical make preserves the exact FAA legal label".to_string(),
        }
    }

    fn exact_family_relationship(label: &str) -> FamilyLabelRelationshipDecision {
        FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::ExactCanonicalLabel,
            observed_family_label: label.to_string(),
            canonical_family_name: label.to_string(),
            existing_alias_id: None,
            valid_from_model_year: None,
            valid_to_model_year: None,
            evidence_ids: Vec::new(),
            applicability_evidence_ids: Vec::new(),
            rationale: "retained and canonical family labels are exact".to_string(),
        }
    }

    fn grounded_web() -> GroundingAudit {
        GroundingAudit {
            mode: GroundingMode::FreshWeb,
            google_search_call_count: 1,
            url_context_call_count: 1,
            citation_urls: ["https://manufacturer.example/identity".to_string()]
                .into_iter()
                .collect(),
            reused_verified_dossier: false,
        }
    }

    fn exact_empty_catalog(server: &ServerFaaIdentityEvidence) -> CatalogCandidateRegistry {
        let observed_family = server
            .observation_bindings
            .first()
            .expect("server fixture has one observation")
            .observed_model
            .clone();
        CatalogCandidateRegistry {
            catalog_revision: Some("sha256:fixture".to_string()),
            search_request: Some(AircraftCatalogSearchRequest {
                observed_make: server.faa_manufacturer_name().to_string(),
                observed_family,
                observed_designation: server.faa_model_designation().to_string(),
                observed_generation: None,
                observed_package: None,
                model_year: *server
                    .listing_model_years
                    .iter()
                    .next()
                    .expect("server fixture has one model year"),
            }),
            ..CatalogCandidateRegistry::default()
        }
    }

    fn catalog_response_with_designation_candidate(
        server: &ServerFaaIdentityEvidence,
        designation_display_name: &str,
        designation_authoritative_designator: &str,
        designation_parent_id: i64,
    ) -> AircraftCatalogSearchResponse {
        let search_request = exact_empty_catalog(server)
            .search_request
            .expect("fixture has an exact search request");
        let candidates = vec![
            AircraftCatalogCandidate {
                entity_kind: HierarchyEntityKind::Make,
                catalog_id: 3,
                display_name: server.faa_manufacturer_name().to_string(),
                authoritative_designator: None,
                parent_catalog_id: None,
                aliases: Vec::new(),
                approved_aliases: Vec::new(),
                identifiers: Vec::new(),
                retrieval_score: 1.0,
                retrieval_reasons: vec!["exact_display_retrieval_key".to_string()],
            },
            AircraftCatalogCandidate {
                entity_kind: HierarchyEntityKind::Family,
                catalog_id: 7,
                display_name: search_request.observed_family.clone(),
                authoritative_designator: None,
                parent_catalog_id: Some(3),
                aliases: Vec::new(),
                approved_aliases: Vec::new(),
                identifiers: Vec::new(),
                retrieval_score: 1.0,
                retrieval_reasons: vec!["exact_display_retrieval_key".to_string()],
            },
            AircraftCatalogCandidate {
                entity_kind: HierarchyEntityKind::Designation,
                catalog_id: 42,
                display_name: designation_display_name.to_string(),
                authoritative_designator: Some(designation_authoritative_designator.to_string()),
                parent_catalog_id: Some(designation_parent_id),
                aliases: Vec::new(),
                approved_aliases: Vec::new(),
                identifiers: Vec::new(),
                retrieval_score: 1.0,
                retrieval_reasons: vec!["designation_collision_candidate".to_string()],
            },
        ];
        let allowed_existing_ids_by_kind =
            allowed_existing_catalog_ids(&search_request, candidates.as_slice());
        AircraftCatalogSearchResponse {
            catalog_revision: "sha256:designation-binding".to_string(),
            catalog_is_empty: false,
            search_request,
            allowed_existing_ids_by_kind,
            candidates,
            generation_designations: Vec::new(),
            package_applicability: Vec::new(),
            warning: "fixture".to_string(),
        }
    }

    fn adjudication_matching_catalog_branch(
        server: &ServerFaaIdentityEvidence,
    ) -> AircraftHierarchyAdjudication {
        let family = server.observation_bindings[0].observed_model.clone();
        let mut adjudication = base_adjudication(server, &family);
        adjudication.make.action = EntityResolutionAction::MatchExisting;
        adjudication.make.existing_catalog_id = Some(3);
        adjudication.family.action = EntityResolutionAction::MatchExisting;
        adjudication.family.existing_catalog_id = Some(7);
        adjudication.designation.action = EntityResolutionAction::MatchExisting;
        adjudication.designation.existing_catalog_id = Some(42);
        adjudication
    }

    fn base_adjudication(
        server: &ServerFaaIdentityEvidence,
        family: &str,
    ) -> AircraftHierarchyAdjudication {
        AircraftHierarchyAdjudication {
            confidence: CurationConfidence::VeryHigh,
            make: entity_with_evidence(
                EntityResolutionAction::ProposeNew,
                None,
                Some(server.faa_manufacturer_name()),
                vec![server.make_claim_id().to_string()],
            ),
            faa_make_relationship: exact_relationship(server),
            family: entity(EntityResolutionAction::ProposeNew, None, Some(family)),
            family_label_relationship: exact_family_relationship(family),
            designation: entity_with_evidence(
                EntityResolutionAction::ProposeNew,
                None,
                Some(server.faa_model_designation()),
                vec![server.designation_claim_id().to_string()],
            ),
            generation: entity(EntityResolutionAction::NoSupportedSelection, None, None),
            package: entity(EntityResolutionAction::NoSupportedSelection, None, None),
            material_distinctions: vec![],
            unresolved_questions: vec![],
            rationale: "each dimension is classified from its appropriate evidence".to_string(),
        }
    }

    #[test]
    fn only_exact_case_bound_server_claims_bypass_gemini_citations() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "FAA identity".to_string(),
            claims: server.claims().to_vec(),
            family_candidates: Vec::new(),
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: vec![],
            unresolved_questions: vec![],
        };
        let grounding = GroundingAudit {
            google_search_call_count: 1,
            url_context_call_count: 1,
            ..GroundingAudit::default()
        };
        validate_identity_evidence_research(&research, &grounding, &server)
            .expect("exact server claims do not need Gemini to re-cite imported FAA data");

        let mut forged = research;
        forged.claims[0]
            .evidence_excerpt
            .push_str(" model-created alteration");
        let error = validate_identity_evidence_research(&forged, &grounding, &server)
            .expect_err("a reserved-id lookalike must never receive the server citation exception");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "forged_server_faa_evidence"));
    }

    #[test]
    fn research_contradictions_and_unresolved_questions_fail_closed() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let mut research = research_with_server(&server, "182");
        research.contradictions = vec!["official sources disagree on the model family".to_string()];
        research.unresolved_questions = vec![ResearchUnresolvedQuestion {
            scope: ResearchUnresolvedScope::Package,
            question: "whether Skylane is a family or a package".to_string(),
        }];

        let error = validate_identity_evidence_research(&research, &grounded_web(), &server)
            .expect_err("explicit research uncertainty must not reach adjudication");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "research_contradictions_present"));
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "research_unresolved_questions_present"));

        research.contradictions = vec![" \t".to_string()];
        research.unresolved_questions = vec![ResearchUnresolvedQuestion {
            scope: ResearchUnresolvedScope::Other,
            question: "\n".to_string(),
        }];
        let error = validate_identity_evidence_research(&research, &grounded_web(), &server)
            .expect_err("a typed unresolved item must retain non-empty question text");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "research_unresolved_question_missing_text"));
        research.unresolved_questions.clear();
        validate_identity_evidence_research(&research, &grounded_web(), &server)
            .expect("blank contradiction annotations alone do not represent unresolved evidence");
    }

    #[test]
    fn model_selected_research_scope_never_authorizes_an_unresolved_question_bypass() {
        let mut server = server_evidence("TEXTRON AVIATION INC", "182T");
        attach_test_tcds(&mut server, "Skylane");
        for (scope, question) in [
            (
                ResearchUnresolvedScope::FaaMakeBrandRelationship,
                "Is Cessna a valid 2022 alias for the FAA legal make?",
            ),
            (
                ResearchUnresolvedScope::FamilyIdentity,
                "Does the OEM family page independently name Skylane?",
            ),
            (
                ResearchUnresolvedScope::FamilyLabelRelationship,
                "Does an OEM page separately map 182 to Skylane?",
            ),
            (
                ResearchUnresolvedScope::FamilyProductionApplicability,
                "What finite OEM production years include 2022?",
            ),
            (
                ResearchUnresolvedScope::Designation,
                "Does the certified designation preserve the exact FAA model?",
            ),
        ] {
            let mut research = research_with_server(&server, "Skylane");
            research.unresolved_questions = vec![ResearchUnresolvedQuestion {
                scope,
                question: question.to_string(),
            }];
            let errors = validate_identity_evidence_research(&research, &grounded_web(), &server)
                .expect_err("a model-selected scope must never authorize bypass");
            assert!(
                errors
                    .0
                    .iter()
                    .any(|issue| issue.code == "research_unresolved_questions_present"),
                "scope {scope:?} did not remain fail-closed"
            );
        }
    }

    #[test]
    fn direct_source_proof_binding_rejects_missing_url_and_span_proofs() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");

        let missing = ServerFetchedAircraftSourceProofs::bind_research(&research, &server, &[])
            .expect_err("a cited web claim without a fetched publisher proof must fail");
        assert!(missing
            .0
            .iter()
            .any(|issue| issue.code == "direct_source_proof_missing"));

        let claim = research
            .claims
            .iter()
            .find(|claim| claim.evidence_id == "identity")
            .unwrap();
        let wrong_span = normalized_test_span("A different publisher sentence.");
        let mismatched = ServerFetchedAircraftSourceProofs::bind_research(
            &research,
            &server,
            &[SourceEvidenceProof {
                final_url: claim.source_url.clone(),
                content_sha256: "a".repeat(64),
                evidence_spans: vec![SourceEvidenceSpanProof {
                    span_sha256: format!("{:x}", Sha256::digest(wrong_span.as_bytes())),
                    normalized_span: wrong_span,
                }],
            }],
        )
        .expect_err("a fetched URL with no exact normalized claim span must fail");
        assert!(mismatched
            .0
            .iter()
            .any(|issue| issue.code == "direct_source_proof_missing"));
    }

    #[test]
    fn adjudication_unresolved_questions_block_reviewability() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let mut adjudication = base_adjudication(&server, "182");
        adjudication.unresolved_questions =
            vec!["whether another hierarchy dimension applies".to_string()];

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("an adjudication with open questions is not reviewable");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "adjudication_unresolved_questions_present"));
    }

    #[test]
    fn independent_verifier_errors_block_reviewability() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let adjudication = base_adjudication(&server, "182");
        let verification = AircraftHierarchyVerification {
            verdict: VerificationVerdict::Confirm,
            confidence: CurationConfidence::VeryHigh,
            verified_evidence_ids: vec![
                "identity".to_string(),
                server.make_claim_id().to_string(),
                server.designation_claim_id().to_string(),
            ],
            differentiation_checks: vec![],
            errors: vec!["the reported collision was not resolved".to_string()],
            rationale: "the verifier found an unresolved error".to_string(),
        };

        let error = build_reviewable_aircraft_hierarchy(
            &research,
            &grounded_web(),
            &server,
            &fetched_source_proofs(&research, &server),
            adjudication,
            &exact_empty_catalog(&server),
            1,
            verification,
            &grounded_web(),
            false,
        )
        .expect_err("verifier errors must override a nominal confirm verdict");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "independent_verifier_errors_present"));
    }

    #[test]
    fn used_web_evidence_without_bound_direct_source_proof_is_not_reviewable() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let adjudication = base_adjudication(&server, "182");
        let verification = AircraftHierarchyVerification {
            verdict: VerificationVerdict::Confirm,
            confidence: CurationConfidence::VeryHigh,
            verified_evidence_ids: vec![
                "identity".to_string(),
                server.make_claim_id().to_string(),
                server.designation_claim_id().to_string(),
            ],
            differentiation_checks: vec![],
            errors: vec![],
            rationale: "confirmed".to_string(),
        };

        let error = build_reviewable_aircraft_hierarchy(
            &research,
            &grounded_web(),
            &server,
            &ServerFetchedAircraftSourceProofs::default(),
            adjudication,
            &exact_empty_catalog(&server),
            1,
            verification,
            &grounded_web(),
            false,
        )
        .expect_err("used web identity evidence must retain a fetched source proof");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "direct_source_proof_mismatch"));
    }

    #[test]
    fn ordinary_exact_designation_accepts_no_supported_optional_selections() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let adjudication = base_adjudication(&server, "182");

        let proposal = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect("exact FAA-only variant and empty exact catalog relations permit operational NULL");
        assert!(proposal.generation.is_none());
        assert!(proposal.tier.is_none());
    }

    #[test]
    fn no_supported_selection_accounts_a_primary_supported_family_token() {
        let mut server = server_evidence("TEXTRON AVIATION INC", "182T");
        server.observation_bindings[0].observed_make = "Cessna".to_string();
        server.observation_bindings[0].observed_model = "182".to_string();
        server.observation_bindings[0].observed_variant = "182T Skylane".to_string();
        let mut research = research_with_server(&server, "Skylane");
        research.claims[0].evidence_excerpt =
            "The Cessna Skylane 182 continues in production for model year 2022.".to_string();
        research.claims[0]
            .supports
            .insert(EvidenceClaimKind::ProductionApplicability);
        let mut adjudication = base_adjudication(&server, "Skylane");
        adjudication.family_label_relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::ProposeAlias,
            observed_family_label: "182".to_string(),
            canonical_family_name: "Skylane".to_string(),
            existing_alias_id: None,
            valid_from_model_year: Some(2022),
            valid_to_model_year: Some(2022),
            evidence_ids: vec!["identity".to_string()],
            applicability_evidence_ids: vec!["identity".to_string()],
            rationale: "direct OEM evidence co-names Skylane and 182 for 2022".to_string(),
        };

        let proposal = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect("an evidence-bound 182-to-Skylane relationship accounts for the retained label");
        assert!(proposal.generation.is_none());
        assert!(proposal.tier.is_none());
    }

    #[test]
    fn family_label_relationship_never_derives_182_from_182t() {
        let mut server = server_evidence("TEXTRON AVIATION INC", "182T");
        server.observation_bindings[0].observed_model = "182".to_string();
        let mut research = research_with_server(&server, "Skylane");
        research.claims[0].evidence_excerpt =
            "Textron Aviation identifies this aircraft as the Cessna Skylane 182T.".to_string();
        research.claims[0]
            .supports
            .insert(EvidenceClaimKind::ProductionApplicability);
        let family = entity(EntityResolutionAction::ProposeNew, None, Some("Skylane"));
        let relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::ProposeAlias,
            observed_family_label: "182".to_string(),
            canonical_family_name: "Skylane".to_string(),
            existing_alias_id: None,
            valid_from_model_year: Some(2022),
            valid_to_model_year: Some(2022),
            evidence_ids: vec!["identity".to_string()],
            applicability_evidence_ids: vec!["identity".to_string()],
            rationale: "prefix-only evidence must fail".to_string(),
        };
        let mut issues = Vec::new();

        assert!(!validate_family_label_relationship(
            &relationship,
            &entity(
                EntityResolutionAction::ProposeNew,
                None,
                Some(server.faa_manufacturer_name()),
            ),
            &family,
            &research,
            &server,
            &exact_empty_catalog(&server),
            &mut issues,
        ));
        assert!(issues
            .iter()
            .any(|issue| { issue.code == "family_label_relationship_missing_conaming_evidence" }));
    }

    #[test]
    fn family_label_relationship_requires_listing_year_applicability() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let mut research = research_with_server(&server, "Skylane");
        research.claims[0].evidence_excerpt =
            "Textron Aviation identifies the Cessna Skylane 182.".to_string();
        research.claims[0]
            .supports
            .insert(EvidenceClaimKind::ProductionApplicability);
        let family = entity(EntityResolutionAction::ProposeNew, None, Some("Skylane"));
        let relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::ProposeAlias,
            observed_family_label: "182".to_string(),
            canonical_family_name: "Skylane".to_string(),
            existing_alias_id: None,
            valid_from_model_year: Some(1956),
            valid_to_model_year: Some(2021),
            evidence_ids: vec!["identity".to_string()],
            applicability_evidence_ids: vec!["identity".to_string()],
            rationale: "out-of-scope historical evidence".to_string(),
        };
        let mut issues = Vec::new();

        assert!(!validate_family_label_relationship(
            &relationship,
            &entity(
                EntityResolutionAction::ProposeNew,
                None,
                Some(server.faa_manufacturer_name()),
            ),
            &family,
            &research,
            &server,
            &exact_empty_catalog(&server),
            &mut issues,
        ));
        assert!(issues
            .iter()
            .any(|issue| issue.code == "family_label_relationship_year_out_of_scope"));
    }

    #[test]
    fn proposed_family_alias_requires_finite_evidence_bound_years() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let mut research = research_with_server(&server, "Skylane");
        research.claims[0].evidence_excerpt =
            "The Cessna Skylane 182 is offered for model year 2022.".to_string();
        research.claims[0]
            .supports
            .insert(EvidenceClaimKind::ProductionApplicability);
        let make = entity(
            EntityResolutionAction::ProposeNew,
            None,
            Some(server.faa_manufacturer_name()),
        );
        let family = entity(EntityResolutionAction::ProposeNew, None, Some("Skylane"));
        let mut relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::ProposeAlias,
            observed_family_label: "182".to_string(),
            canonical_family_name: "Skylane".to_string(),
            existing_alias_id: None,
            valid_from_model_year: None,
            valid_to_model_year: None,
            evidence_ids: vec!["identity".to_string()],
            applicability_evidence_ids: vec!["identity".to_string()],
            rationale: "the source proves only the listing year".to_string(),
        };
        let mut issues = Vec::new();

        assert!(!validate_family_label_relationship(
            &relationship,
            &make,
            &family,
            &research,
            &server,
            &exact_empty_catalog(&server),
            &mut issues,
        ));
        assert!(issues.iter().any(|issue| {
            issue.code == "family_label_relationship_new_alias_requires_finite_bounds"
        }));

        relationship.valid_from_model_year = Some(1956);
        relationship.valid_to_model_year = Some(2022);
        issues.clear();
        assert!(!validate_family_label_relationship(
            &relationship,
            &make,
            &family,
            &research,
            &server,
            &exact_empty_catalog(&server),
            &mut issues,
        ));
        assert!(issues.iter().any(|issue| {
            issue.code == "family_label_relationship_bound_missing_from_applicability_evidence"
                && issue.message.contains("1956")
        }));
    }

    #[test]
    fn family_label_relationship_accepts_only_the_retrieved_alias_owner_and_scope() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "Skylane");
        let family = entity(
            EntityResolutionAction::MatchExisting,
            Some(7),
            Some("Skylane"),
        );
        let alias = AircraftCatalogAliasCandidate {
            alias_id: 41,
            owner_catalog_id: 7,
            alias: "182".to_string(),
            valid_from_model_year: Some(1956),
            valid_to_model_year: None,
            market_code: Some("US".to_string()),
        };
        let mut catalog = exact_empty_catalog(&server);
        catalog.family_aliases_by_id.insert(alias.alias_id, alias);
        let relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::MatchApprovedAlias,
            observed_family_label: "182".to_string(),
            canonical_family_name: "Skylane".to_string(),
            existing_alias_id: Some(41),
            valid_from_model_year: Some(1956),
            valid_to_model_year: None,
            evidence_ids: Vec::new(),
            applicability_evidence_ids: Vec::new(),
            rationale: "the exact approved family alias applies".to_string(),
        };
        let mut issues = Vec::new();

        assert!(validate_family_label_relationship(
            &relationship,
            &entity(
                EntityResolutionAction::ProposeNew,
                None,
                Some(server.faa_manufacturer_name()),
            ),
            &family,
            &research,
            &server,
            &catalog,
            &mut issues,
        ));
        assert!(issues.is_empty());

        let mut wrong_owner = family;
        wrong_owner.existing_catalog_id = Some(8);
        assert!(!validate_family_label_relationship(
            &relationship,
            &entity(
                EntityResolutionAction::ProposeNew,
                None,
                Some(server.faa_manufacturer_name()),
            ),
            &wrong_owner,
            &research,
            &server,
            &catalog,
            &mut Vec::new(),
        ));
    }

    #[test]
    fn match_existing_family_binds_returned_label_and_parent_make() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let mut adjudication = base_adjudication(&server, "182");
        adjudication.make.action = EntityResolutionAction::MatchExisting;
        adjudication.make.existing_catalog_id = Some(3);
        adjudication.family.action = EntityResolutionAction::MatchExisting;
        adjudication.family.existing_catalog_id = Some(7);

        let response = AircraftCatalogSearchResponse {
            catalog_revision: "sha256:family-binding".to_string(),
            catalog_is_empty: false,
            search_request: exact_empty_catalog(&server)
                .search_request
                .expect("fixture has an exact search request"),
            allowed_existing_ids_by_kind: BTreeMap::from([
                (HierarchyEntityKind::Make, vec![3]),
                (HierarchyEntityKind::Family, vec![7]),
            ]),
            candidates: vec![
                AircraftCatalogCandidate {
                    entity_kind: HierarchyEntityKind::Make,
                    catalog_id: 3,
                    display_name: server.faa_manufacturer_name().to_string(),
                    authoritative_designator: None,
                    parent_catalog_id: None,
                    aliases: Vec::new(),
                    approved_aliases: Vec::new(),
                    identifiers: Vec::new(),
                    retrieval_score: 1.0,
                    retrieval_reasons: vec!["exact_display_retrieval_key".to_string()],
                },
                AircraftCatalogCandidate {
                    entity_kind: HierarchyEntityKind::Family,
                    catalog_id: 7,
                    display_name: "182".to_string(),
                    authoritative_designator: None,
                    parent_catalog_id: Some(3),
                    aliases: Vec::new(),
                    approved_aliases: Vec::new(),
                    identifiers: Vec::new(),
                    retrieval_score: 1.0,
                    retrieval_reasons: vec!["exact_display_retrieval_key".to_string()],
                },
            ],
            generation_designations: Vec::new(),
            package_applicability: Vec::new(),
            warning: "fixture".to_string(),
        };
        let exact_catalog = response.candidate_registry();

        validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_catalog,
            1,
        )
        .expect("the exact returned family label and parent make are bound");

        let mut substituted_kind_catalog = exact_catalog.clone();
        substituted_kind_catalog
            .identities_by_kind
            .get_mut(&HierarchyEntityKind::Family)
            .and_then(|identities| identities.get_mut(&7))
            .expect("fixture has returned family identity")
            .entity_kind = HierarchyEntityKind::Designation;
        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &substituted_kind_catalog,
            1,
        )
        .expect_err("an ID cannot authorize an entity from another catalog kind");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "family_catalog_candidate_kind_mismatch"));

        let mut substituted_label = adjudication.clone();
        substituted_label.family.display_name = Some("Skylane".to_string());
        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &substituted_label,
            &exact_catalog,
            1,
        )
        .expect_err("an ID cannot authorize a substituted family display label");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "family_catalog_candidate_label_mismatch"));

        let mut wrong_parent_response = response;
        wrong_parent_response.candidates[1].parent_catalog_id = Some(4);
        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &wrong_parent_response.candidate_registry(),
            1,
        )
        .expect_err("an ID cannot authorize a family owned by another make");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "family_catalog_candidate_parent_mismatch"));
    }

    #[test]
    fn proposed_family_alias_rejects_same_make_normalized_catalog_collisions() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let mut research = research_with_server(&server, "Skylane");
        research.claims[0].evidence_excerpt =
            "The Cessna Skylane 182 is offered for model year 2022.".to_string();
        research.claims[0]
            .supports
            .insert(EvidenceClaimKind::ProductionApplicability);
        let make = entity(
            EntityResolutionAction::MatchExisting,
            Some(3),
            Some(server.faa_manufacturer_name()),
        );
        let family = entity(
            EntityResolutionAction::MatchExisting,
            Some(7),
            Some("Skylane"),
        );
        let colliding_alias = AircraftCatalogAliasCandidate {
            alias_id: 41,
            owner_catalog_id: 8,
            alias: "182!".to_string(),
            valid_from_model_year: Some(1900),
            valid_to_model_year: Some(1901),
            market_code: Some("EU".to_string()),
        };
        let response = AircraftCatalogSearchResponse {
            catalog_revision: "sha256:family-collisions".to_string(),
            catalog_is_empty: false,
            search_request: exact_empty_catalog(&server)
                .search_request
                .expect("fixture has an exact search request"),
            allowed_existing_ids_by_kind: BTreeMap::new(),
            candidates: vec![
                AircraftCatalogCandidate {
                    entity_kind: HierarchyEntityKind::Family,
                    catalog_id: 7,
                    display_name: "Skylane".to_string(),
                    authoritative_designator: None,
                    parent_catalog_id: Some(3),
                    aliases: Vec::new(),
                    approved_aliases: Vec::new(),
                    identifiers: Vec::new(),
                    retrieval_score: 1.0,
                    retrieval_reasons: vec!["family_identity".to_string()],
                },
                AircraftCatalogCandidate {
                    entity_kind: HierarchyEntityKind::Family,
                    catalog_id: 8,
                    display_name: "Cardinal".to_string(),
                    authoritative_designator: None,
                    parent_catalog_id: Some(3),
                    aliases: vec![colliding_alias.alias.clone()],
                    approved_aliases: vec![colliding_alias],
                    identifiers: Vec::new(),
                    retrieval_score: 1.0,
                    retrieval_reasons: vec!["exact_alias_retrieval_key".to_string()],
                },
                AircraftCatalogCandidate {
                    entity_kind: HierarchyEntityKind::Family,
                    catalog_id: 9,
                    display_name: "182!".to_string(),
                    authoritative_designator: None,
                    parent_catalog_id: Some(3),
                    aliases: Vec::new(),
                    approved_aliases: Vec::new(),
                    identifiers: Vec::new(),
                    retrieval_score: 1.0,
                    retrieval_reasons: vec!["exact_display_retrieval_key".to_string()],
                },
            ],
            generation_designations: Vec::new(),
            package_applicability: Vec::new(),
            warning: "fixture".to_string(),
        };
        let relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::ProposeAlias,
            observed_family_label: "182".to_string(),
            canonical_family_name: "Skylane".to_string(),
            existing_alias_id: None,
            valid_from_model_year: Some(2022),
            valid_to_model_year: Some(2022),
            evidence_ids: vec!["identity".to_string()],
            applicability_evidence_ids: vec!["identity".to_string()],
            rationale: "the proposed alias must be collision-free".to_string(),
        };
        let mut issues = Vec::new();

        assert!(!validate_family_label_relationship(
            &relationship,
            &make,
            &family,
            &research,
            &server,
            &response.candidate_registry(),
            &mut issues,
        ));
        assert!(issues
            .iter()
            .any(|issue| issue.code == "family_label_relationship_same_make_alias_collision"));
        assert!(issues.iter().any(|issue| {
            issue.code == "family_label_relationship_same_make_canonical_collision"
        }));
    }

    #[test]
    fn token_accounting_requires_exact_contiguous_label_order() {
        let observed_tokens = alphanumeric_tokens("182 Skylane");
        let mut consumed = vec![false; observed_tokens.len()];

        consume_exact_label_tokens(&observed_tokens, &mut consumed, "Skylane 182");

        assert_eq!(consumed, vec![false, false]);
        assert!(contains_exact_contiguous_label(
            "The Cessna Skylane (182) continues today.",
            "Skylane"
        ));
        assert!(contains_exact_contiguous_label(
            "The Cessna Skylane (182) continues today.",
            "182"
        ));
        assert!(!contains_exact_contiguous_label(
            "The Cessna Skylane 182T continues today.",
            "182"
        ));
    }

    #[test]
    fn proof_gated_numeric_series_stem_accounts_for_only_the_broad_model_token() {
        assert!(
            unaccounted_observed_hierarchy_tokens(
                "182 Skylane",
                "182Q",
                Some("Skylane"),
                None,
                Some("TEXTRON AVIATION INC"),
                None,
                None,
                "CESSNA",
                true,
                true,
                false,
                None,
            )
            .is_empty(),
            "exact 182Q in the other field plus TCDS proof may account for broad numeric series 182"
        );
        assert!(
            unaccounted_observed_hierarchy_tokens(
                "182 Skylane",
                "182Q",
                Some("Skylane"),
                None,
                Some("TEXTRON AVIATION INC"),
                None,
                None,
                "CESSNA",
                true,
                false,
                false,
                Some("Skylane"),
            )
            .is_empty(),
            "an exact serial-bound named-family TCDS independently accounts for its numeric series stem"
        );
        assert_eq!(
            unaccounted_observed_hierarchy_tokens(
                "182 Skylane",
                "182Q",
                Some("Skylane"),
                None,
                Some("TEXTRON AVIATION INC"),
                None,
                None,
                "CESSNA",
                true,
                false,
                false,
                None,
            ),
            vec!["182"],
            "the numeric series stem requires either the exact paired designation or an exact named-family TCDS binding"
        );
        assert_eq!(
            unaccounted_observed_hierarchy_tokens(
                "182 Skylane G6 GTS NXi",
                "182Q",
                Some("Skylane"),
                None,
                Some("TEXTRON AVIATION INC"),
                None,
                None,
                "CESSNA",
                true,
                true,
                false,
                Some("Skylane"),
            ),
            vec!["g6", "gts", "nxi"],
            "family, designation, and numeric-series proof must never erase optional or equipment tokens"
        );
    }

    #[test]
    fn exact_named_tcds_family_accounts_only_for_safe_numeric_series_stems() {
        let unaccounted = |observed: &str,
                           designation: &str,
                           resolved_family: Option<&str>,
                           named_tcds_family: Option<&str>| {
            unaccounted_observed_hierarchy_tokens(
                observed,
                designation,
                resolved_family,
                None,
                Some("TEXTRON AVIATION INC"),
                None,
                None,
                "CESSNA",
                true,
                false,
                false,
                named_tcds_family,
            )
        };

        assert!(unaccounted("182", "182J", Some("Skylane"), Some("Skylane")).is_empty());
        assert!(
            unaccounted("182 Skylane", "182K", Some("Skylane"), Some("Skylane")).is_empty(),
            "the exact family and proof-gated numeric series are accounted independently"
        );
        for (observed, designation, resolved_family, tcds_family) in [
            ("182 G6", "182J", Some("Skylane"), Some("Skylane")),
            ("182T", "182J", Some("Skylane"), Some("Skylane")),
            ("182T", "182K", Some("Skylane"), Some("Skylane")),
            ("C182H", "182K", Some("Skylane"), Some("Skylane")),
            ("T182T", "182K", Some("Skylane"), Some("Skylane")),
            ("SR22T", "182K", Some("Skylane"), Some("Skylane")),
            ("182RG", "182K", Some("Skylane"), Some("Skylane")),
            ("182", "182J", Some("Skylane"), None),
            ("182", "182J", Some("Skyhawk"), Some("Skylane")),
            ("182", "T182T", Some("Skylane"), Some("Skylane")),
            ("SR22", "SR22T", Some("Cirrus"), Some("Cirrus")),
            ("182", "182RG", Some("Skylane"), Some("Skylane")),
            ("182", "C182H", Some("Skylane"), Some("Skylane")),
            ("C 182", "182K", Some("Skylane"), Some("Skylane")),
            ("C182K", "182K", Some("Skylane"), Some("Skylane")),
        ] {
            assert!(
                !unaccounted(observed, designation, resolved_family, tcds_family).is_empty(),
                "unsafe generic series specialization accepted observed {observed:?}, FAA {designation:?}"
            );
        }
        assert_eq!(
            unaccounted_observed_hierarchy_tokens(
                "182",
                "T182T",
                Some("Skylane"),
                None,
                None,
                None,
                None,
                "CESSNA",
                true,
                true,
                false,
                Some("Skylane"),
            ),
            vec!["182"],
            "even an exact paired T182T must not expose 182 by stripping its prefix"
        );
        assert_eq!(
            unaccounted_observed_hierarchy_tokens(
                "182 Skylane",
                "182K",
                Some("Skylane"),
                None,
                None,
                None,
                None,
                "CESSNA",
                false,
                false,
                false,
                Some("Skylane"),
            ),
            vec!["182"],
            "a named-family value cannot replace exact designation-and-serial TCDS proof"
        );
        assert_eq!(
            unaccounted("182 182 Skylane", "182K", Some("Skylane"), Some("Skylane")),
            vec!["182"],
            "the proof gate consumes only one exact numeric stem token per retained field"
        );
    }

    #[test]
    fn exact_turbo_family_phrase_cross_field_authorizes_only_one_base_series_token() {
        assert!(retained_field_is_exact_turbo_designation_family_phrase(
            "Turbo 182T Skylane",
            "T182T",
            "Skylane",
            true,
        ));
        for (retained, designation, family, proof) in [
            ("Turbo 182T Skylane", "T182T", "Skylane", false),
            ("Turbo 182T Skyhawk", "T182T", "Skylane", true),
            ("Turbo 182T Skylane G6", "T182T", "Skylane", true),
            ("182T Skylane", "T182T", "Skylane", true),
            ("Skylane Turbo 182T", "T182T", "Skylane", true),
            ("Turbo 182T Skylane", "182T", "Skylane", true),
            ("Turbo 182T Skylane", "X182T", "Skylane", true),
            ("Turbo 182T Skylane", "TT182T", "Skylane", true),
            ("Turbo 182T Skylane", "C182H", "Skylane", true),
        ] {
            assert!(
                !retained_field_is_exact_turbo_designation_family_phrase(
                    retained,
                    designation,
                    family,
                    proof,
                ),
                "unsafe turbo cross-field authority accepted retained={retained:?}, FAA={designation:?}, family={family:?}, proof={proof}"
            );
        }

        assert!(
            unaccounted_observed_hierarchy_tokens(
                "182",
                "T182T",
                Some("Skylane"),
                None,
                None,
                None,
                None,
                "CESSNA",
                true,
                true,
                true,
                Some("Skylane"),
            )
            .is_empty(),
            "the exact atomic paired Turbo phrase may authorize one standalone base-series token"
        );
        assert_eq!(
            unaccounted_observed_hierarchy_tokens(
                "182 182",
                "T182T",
                Some("Skylane"),
                None,
                None,
                None,
                None,
                "CESSNA",
                true,
                true,
                true,
                Some("Skylane"),
            ),
            vec!["182"],
            "atomic turbo authority must consume only one standalone base-series token"
        );
        assert_eq!(
            unaccounted_observed_hierarchy_tokens(
                "182",
                "T182T",
                Some("Skyhawk"),
                None,
                None,
                None,
                None,
                "CESSNA",
                true,
                true,
                true,
                Some("Skylane"),
            ),
            vec!["182"],
            "a selected family different from the named TCDS family must block turbo base-series authority"
        );
    }

    #[test]
    fn regulator_complete_accepts_named_tcds_bound_numeric_series_labels() {
        let mut generic = server_evidence_with_observed_model("CESSNA", "182J", "182", "Skylane");
        attach_test_tcds_lineage(&mut generic);
        generic
            .regulator_complete_research()
            .expect("standalone numeric series plus exact named family is regulator-complete");

        let mut composite =
            server_evidence_with_observed_model("CESSNA", "182K", "182 Skylane", "182");
        attach_test_tcds_lineage(&mut composite);
        composite.regulator_complete_research().expect(
            "exact serial-bound `Model 182K, Skylane` proof accounts for `182 Skylane` and `182` without rewriting either field",
        );
    }

    #[test]
    fn regulator_complete_numeric_series_requires_exact_family_serial_and_tcds_proof() {
        let mut wrong_family =
            server_evidence_with_observed_model("CESSNA", "182K", "182 Skyhawk", "182");
        attach_test_tcds_lineage(&mut wrong_family);
        assert!(
            wrong_family.regulator_complete_research().is_none(),
            "a different retained family must remain unaccounted"
        );

        let mut wrong_serial =
            server_evidence_with_observed_model("CESSNA", "182K", "182 Skylane", "182");
        attach_test_tcds_lineage(&mut wrong_serial);
        wrong_serial.observation_bindings[0]
            .grounding
            .manufacturer_serial_key = Some("18299999".to_string());
        assert!(
            wrong_serial.regulator_complete_research().is_none(),
            "a TCDS proof for a different FAA-matched serial must not authorize the numeric stem"
        );

        let without_tcds =
            server_evidence_with_observed_model("CESSNA", "182K", "182 Skylane", "182");
        assert!(
            without_tcds.regulator_complete_research().is_none(),
            "registry identity without exact current TCDS proof is insufficient"
        );

        for designation in ["T182T", "SR22T", "182RG", "C182H"] {
            let observed_stem = if designation == "SR22T" {
                "SR22"
            } else {
                "182"
            };
            let family = if designation == "SR22T" {
                "Cirrus"
            } else {
                "Skylane"
            };
            let mut prefixed_or_multi_suffix = server_evidence_with_observed_model(
                "CESSNA",
                designation,
                &format!("{observed_stem} {family}"),
                observed_stem,
            );
            attach_test_tcds_lineage(&mut prefixed_or_multi_suffix);
            assert!(
                prefixed_or_multi_suffix
                    .regulator_complete_research()
                    .is_none(),
                "designation {designation:?} must not expose a mechanically stripped broad-series stem"
            );
        }
    }

    #[test]
    fn regulator_complete_accepts_exact_tcds_bound_composite_family_label() {
        let mut server =
            server_evidence_with_observed_model("CESSNA", "182Q", "182 Skylane", "182Q");
        attach_test_tcds_lineage(&mut server);

        server
            .regulator_complete_research()
            .expect("exact registry, serial, TCDS, family, and lineage proof is complete");
    }

    #[test]
    fn regulator_complete_rejects_unaccounted_tokens_and_cross_observation_authorization() {
        let mut suffixed =
            server_evidence_with_observed_model("CESSNA", "182Q", "182 Skylane G6", "182Q");
        attach_test_tcds_lineage(&mut suffixed);
        assert!(
            suffixed.regulator_complete_research().is_none(),
            "a TCDS family binding must not erase an optional-dimension suffix"
        );

        let mut listing_five_shaped_extras =
            server_evidence_with_observed_model("CESSNA", "182K", "182 SKYLANE G6 GTS NXi", "182");
        attach_test_tcds_lineage(&mut listing_five_shaped_extras);
        assert_eq!(
            listing_five_shaped_extras.unaccounted_observed_regulator_hierarchy_tokens(),
            vec!["G6", "GTS", "NXi"],
            "the proof-gated numeric stem and family must leave every optional token visible"
        );
        assert!(
            listing_five_shaped_extras
                .regulator_complete_research()
                .is_none(),
            "listing-shaped optional tokens must block regulator-complete mode"
        );

        let mut mixed =
            server_evidence_with_observed_model("CESSNA", "182Q", "182 Skylane", "182Q");
        let mut second = mixed.observation_bindings[0].clone();
        second.listing_id += 1;
        second.observation_sha256 = "f".repeat(64);
        second.observed_variant = "Skylane G6".to_string();
        mixed.observation_bindings.push(second);
        attach_test_tcds_lineage(&mut mixed);
        assert!(
            mixed.regulator_complete_research().is_none(),
            "one observation's exact designation must not authorize another observation's numeric stem"
        );
    }

    #[test]
    fn turbo_designation_display_expansion_is_atomic_and_proof_gated() {
        let unaccounted = |observed: &str, designation: &str, exact_tcds_proof: bool| {
            unaccounted_observed_hierarchy_tokens(
                observed,
                designation,
                None,
                None,
                None,
                None,
                None,
                "",
                exact_tcds_proof,
                false,
                false,
                None,
            )
        };

        assert!(unaccounted("Turbo 182T", "T182T", true).is_empty());
        for rejected in [
            ("Turbo 182T", "T182T", false),
            ("182T", "T182T", true),
            ("182T Turbo", "T182T", true),
            ("Turbo 182", "T182T", true),
            ("Turbo 182T", "182T", true),
            ("Turbo 182T", "X182T", true),
            ("Turbo 182T", "TT182T", true),
        ] {
            assert!(
                !unaccounted(rejected.0, rejected.1, rejected.2).is_empty(),
                "unsafe display expansion accepted observed {:?}, FAA {:?}, proof={}",
                rejected.0,
                rejected.1,
                rejected.2
            );
        }
    }

    #[test]
    fn regulator_complete_mode_is_recomputed_from_exact_turbo_tcds_case() {
        let mut server = server_evidence_with_observed_model(
            "TEXTRON AVIATION INC",
            "T182T",
            "182",
            "Turbo 182T Skylane",
        );
        attach_test_tcds_lineage(&mut server);
        let research = server
            .regulator_complete_research()
            .expect("exact T182T registry/TCDS/family/lineage proof should be complete");
        let grounding = GroundingAudit {
            mode: GroundingMode::RegulatorComplete,
            google_search_call_count: 0,
            url_context_call_count: 0,
            citation_urls: BTreeSet::new(),
            reused_verified_dossier: false,
        };

        validate_identity_evidence_research(&research, &grounding, &server)
            .expect("the exact server-recomputed regulator dossier should validate");

        let mut forged = research.clone();
        forged.subject_summary.push_str(" model-authored addition");
        let error = validate_identity_evidence_research(&forged, &grounding, &server)
            .expect_err("a caller cannot mark a modified dossier regulator-complete");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "regulator_complete_grounding_invalid"));
    }

    #[test]
    fn no_supported_selection_cannot_spoof_regulator_complete_mode() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let adjudication = base_adjudication(&server, "182");
        let forged_grounding = GroundingAudit {
            mode: GroundingMode::RegulatorComplete,
            google_search_call_count: 0,
            url_context_call_count: 0,
            citation_urls: BTreeSet::new(),
            reused_verified_dossier: false,
        };

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &forged_grounding,
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("a mode flag cannot replace missing exact TCDS/family/lineage proof");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "regulator_complete_grounding_invalid"));
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "no_supported_selection_grounding_required"));
    }

    #[test]
    fn no_supported_selection_has_no_legacy_serde_alias() {
        assert_eq!(
            serde_json::from_str::<EntityResolutionAction>("\"no_supported_selection\"").unwrap(),
            EntityResolutionAction::NoSupportedSelection
        );
        assert!(serde_json::from_str::<EntityResolutionAction>("\"not_applicable\"").is_err());
    }

    #[test]
    fn no_supported_selection_rejects_an_unaccounted_g6_gts_variant() {
        let mut server = server_evidence("CIRRUS DESIGN CORP", "SR22");
        server.observation_bindings[0].observed_variant = "SR22 G6 GTS".to_string();
        let research = research_with_server(&server, "SR22");
        let adjudication = base_adjudication(&server, "SR22");

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("extra generation/package-like listing labels must remain unresolved");
        assert!(error
            .0
            .iter()
            .any(|issue| { issue.code == "no_supported_selection_unaccounted_observed_label" }));
    }

    #[test]
    fn designation_display_cannot_erase_optional_hierarchy_tokens() {
        let mut server = server_evidence("CIRRUS DESIGN CORP", "SR22");
        server.observation_bindings[0].observed_variant = "SR22 G6 GTS".to_string();
        let research = research_with_server(&server, "SR22");
        let mut adjudication = base_adjudication(&server, "SR22");
        adjudication.designation.display_name = Some("SR22 G6 GTS".to_string());

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("a friendly designation display is not an accounting authority");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "no_supported_selection_unaccounted_observed_label"));
    }

    #[test]
    fn family_decision_must_match_a_typed_primary_candidate() {
        let server = server_evidence("CIRRUS DESIGN CORP", "SR22");
        let research = research_with_server(&server, "SR22");
        let adjudication = base_adjudication(&server, "SR22 G6 GTS");

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("generic hierarchy evidence cannot authorize an untyped family label");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "entity_missing_exact_typed_candidate"));
    }

    #[test]
    fn no_supported_selection_rejects_an_unaccounted_model_field_label() {
        let mut server = server_evidence("CIRRUS DESIGN CORP", "SR22");
        server.observation_bindings[0].observed_model = "SR22 G6".to_string();
        let research = research_with_server(&server, "SR22");
        let adjudication = base_adjudication(&server, "SR22");

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("material model-field labels must not bypass optional-dimension liveness");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "no_supported_selection_unaccounted_observed_label"));
    }

    #[test]
    fn positive_generation_accounts_for_package_null_liveness() {
        let mut server = server_evidence("CIRRUS DESIGN CORP", "SR22");
        server.observation_bindings[0].observed_variant = "SR22 G6".to_string();
        let mut research = research_with_server(&server, "SR22");
        research.claims[0].evidence_excerpt =
            "The manufacturer identifies SR22 as the aircraft family and G6 as its generation."
                .to_string();
        research.generation_candidates.push(HierarchyCandidate {
            label: "G6".to_string(),
            evidence_ids: vec!["identity".to_string()],
        });
        let mut adjudication = base_adjudication(&server, "SR22");
        adjudication.generation = entity(EntityResolutionAction::ProposeNew, None, Some("G6"));

        let proposal = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect("a positively typed generation accounts for its token during package NULL checks");
        assert_eq!(
            proposal
                .generation
                .as_ref()
                .map(|value| value.display_name.as_str()),
            Some("G6")
        );
        assert!(proposal.tier.is_none());
    }

    #[test]
    fn positive_package_accounts_for_generation_null_liveness() {
        let mut server = server_evidence("CIRRUS DESIGN CORP", "SR22");
        server.observation_bindings[0].observed_variant = "SR22 GTS".to_string();
        let mut research = research_with_server(&server, "SR22");
        research.claims[0].evidence_excerpt =
            "The manufacturer identifies SR22 as the aircraft family and GTS as its factory package."
                .to_string();
        research.package_candidates.push(HierarchyCandidate {
            label: "GTS".to_string(),
            evidence_ids: vec!["identity".to_string()],
        });
        let mut adjudication = base_adjudication(&server, "SR22");
        adjudication.package = entity(EntityResolutionAction::ProposeNew, None, Some("GTS"));

        let proposal = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect("a positively typed package accounts for its token during generation NULL checks");
        assert!(proposal.generation.is_none());
        assert_eq!(
            proposal
                .tier
                .as_ref()
                .map(|value| value.display_name.as_str()),
            Some("GTS")
        );
    }

    #[test]
    fn no_supported_selection_rejects_untyped_avionics_like_suffixes() {
        let mut server = server_evidence("TEXTRON AVIATION INC", "182T");
        server.observation_bindings[0].observed_variant = "182T G1000 NXi".to_string();
        let research = research_with_server(&server, "Skylane");
        let adjudication = base_adjudication(&server, "Skylane");

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("equipment-like leftovers cannot be erased as hierarchy NULLs");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "no_supported_selection_unaccounted_observed_label"));
    }

    #[test]
    fn no_supported_selection_never_consumes_designation_prefix_collisions() {
        for (faa_designation, observed_variant) in [("182", "182T Skylane"), ("SR22", "SR22T")] {
            let mut server = server_evidence("EXACT FAA MAKE", faa_designation);
            server.observation_bindings[0].observed_variant = observed_variant.to_string();
            let research = research_with_server(&server, "Skylane");
            let adjudication = base_adjudication(&server, "Skylane");

            let error = validate_aircraft_hierarchy_adjudication(
                &research,
                &grounded_web(),
                &server,
                &adjudication,
                &exact_empty_catalog(&server),
                1,
            )
            .expect_err("a shorter designation must not consume a longer observed token");
            assert!(error
                .0
                .iter()
                .any(|issue| issue.code == "no_supported_selection_unaccounted_observed_label"));
        }
    }

    #[test]
    fn no_supported_selection_accepts_an_exact_reused_verified_dossier() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let adjudication = base_adjudication(&server, "182");
        let reused_grounding = GroundingAudit {
            mode: GroundingMode::ReusedVerifiedDossier,
            google_search_call_count: 0,
            url_context_call_count: 0,
            citation_urls: ["https://manufacturer.example/identity".to_string()]
                .into_iter()
                .collect(),
            reused_verified_dossier: true,
        };

        validate_aircraft_hierarchy_adjudication(
            &research,
            &reused_grounding,
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect("an exact case-bound verified dossier preserves its grounding provenance");
    }

    #[test]
    fn no_supported_selection_requires_exact_server_catalog_query_scope() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let adjudication = base_adjudication(&server, "182");
        for mutate in [
            |request: &mut AircraftCatalogSearchRequest| {
                request.observed_make = "Cessna".to_string()
            },
            |request: &mut AircraftCatalogSearchRequest| {
                request.observed_designation = "182-T".to_string()
            },
        ] {
            let mut catalog = exact_empty_catalog(&server);
            mutate(
                catalog
                    .search_request
                    .as_mut()
                    .expect("catalog fixture has an exact query"),
            );
            let error = validate_aircraft_hierarchy_adjudication(
                &research,
                &grounded_web(),
                &server,
                &adjudication,
                &catalog,
                1,
            )
            .expect_err("catalog query echoes must preserve the exact server-owned scope");
            assert!(error
                .0
                .iter()
                .any(|issue| issue.code == "no_supported_selection_catalog_scope_mismatch"));
        }
    }

    #[test]
    fn no_supported_generation_rejects_an_existing_designation_relation() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let adjudication = adjudication_matching_catalog_branch(&server);
        let mut catalog = catalog_response_with_designation_candidate(&server, "182T", "182T", 7)
            .candidate_registry();
        catalog.generation_designations.insert((7, 42));

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &catalog,
            1,
        )
        .expect_err("an existing generation relation makes NULL unsafe");
        assert!(error
            .0
            .iter()
            .any(|issue| { issue.code == "no_supported_selection_generation_relation_exists" }));
    }

    #[test]
    fn no_supported_package_checks_exact_model_year_applicability() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let adjudication = adjudication_matching_catalog_branch(&server);
        let mut catalog = catalog_response_with_designation_candidate(&server, "182T", "182T", 7)
            .candidate_registry();
        catalog
            .package_applicability
            .push(AircraftCatalogPackageApplicabilityRow {
                applicability_id: 3,
                aircraft_factory_package_id: 9,
                package_kind: "trim_tier".to_string(),
                aircraft_designation_id: 42,
                aircraft_generation_id: None,
                valid_from_model_year: Some(2022),
                valid_to_model_year: Some(2022),
            });

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &catalog,
            1,
        )
        .expect_err("an applicable package relation makes NULL unsafe");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "no_supported_selection_package_relation_exists"));

        catalog.package_applicability[0].valid_from_model_year = Some(2023);
        catalog.package_applicability[0].valid_to_model_year = Some(2024);
        validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &catalog,
            1,
        )
        .expect("an out-of-year package relation does not apply to the exact 2022 case");

        catalog.package_applicability[0].package_kind = "option_bundle".to_string();
        catalog.package_applicability[0].valid_from_model_year = Some(2022);
        catalog.package_applicability[0].valid_to_model_year = Some(2022);
        validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &catalog,
            1,
        )
        .expect("an option bundle is not a trim-tier hierarchy selection");
    }

    #[test]
    fn no_supported_selection_rejects_required_dimensions_and_entity_fields() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let mut adjudication = base_adjudication(&server, "182");
        adjudication.make = CatalogEntityDecision {
            action: EntityResolutionAction::NoSupportedSelection,
            existing_catalog_id: None,
            display_name: None,
            authoritative_designator: None,
            evidence_ids: Vec::new(),
            rationale: "invalid required NULL".to_string(),
        };
        adjudication.generation.display_name = Some("G6".to_string());
        adjudication.generation.evidence_ids = vec!["identity".to_string()];

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("required dimensions and entity-shaped NULL decisions must fail");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "required_entity_no_supported_selection"));
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "no_supported_selection_has_entity_fields"));
    }

    #[test]
    fn no_supported_selection_rejects_a_positive_primary_candidate() {
        let server = server_evidence("CIRRUS DESIGN CORP", "SR22");
        let mut research = research_with_server(&server, "SR22");
        research.generation_candidates.push(HierarchyCandidate {
            label: "G6".to_string(),
            evidence_ids: vec!["identity".to_string()],
        });
        let adjudication = base_adjudication(&server, "SR22");

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("a positive generation candidate must be resolved or remain unresolved");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "no_supported_selection_positive_candidate_exists"));
    }

    #[test]
    fn faa_model_claim_cannot_authorize_skylane_as_a_package() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let mut adjudication = base_adjudication(&server, "182");
        adjudication.package = entity_with_evidence(
            EntityResolutionAction::ProposeNew,
            None,
            Some("Skylane"),
            vec![server.designation_claim_id().to_string()],
        );

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("FAA registry model identity does not establish a marketing package");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "missing_web_identity_evidence"));
    }

    #[test]
    fn exact_faa_legal_make_is_valid_without_inventing_a_brand_alias() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let adjudication = base_adjudication(&server, "182");

        let proposal = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect("the exact FAA legal make is the fail-closed canonical fallback");

        assert_eq!(proposal.manufacturer.display_name, "TEXTRON AVIATION INC");
        assert_eq!(
            proposal
                .certified_variant
                .authoritative_designator
                .as_deref(),
            Some("182T")
        );
        assert!(proposal.tier.is_none());
    }

    #[test]
    fn exact_tcds_holder_candidate_forbids_a_new_faa_label_make_branch() {
        let mut server = server_evidence("CESSNA", "182T");
        attach_test_tcds_lineage(&mut server);
        let research = research_with_server(&server, "Skylane");
        let mut catalog = exact_empty_catalog(&server);
        catalog
            .ids_by_kind
            .entry(HierarchyEntityKind::Make)
            .or_default()
            .insert(17);
        catalog
            .identities_by_kind
            .entry(HierarchyEntityKind::Make)
            .or_default()
            .insert(
                17,
                AircraftCatalogCandidateIdentity {
                    entity_kind: HierarchyEntityKind::Make,
                    catalog_id: 17,
                    display_name: "TEXTRON AVIATION INC".to_string(),
                    authoritative_designator: None,
                    parent_catalog_id: None,
                },
            );
        let proposed_faa_make = entity_with_evidence(
            EntityResolutionAction::ProposeNew,
            None,
            Some("CESSNA"),
            vec![server.make_claim_id().to_string()],
        );
        let relationship = exact_relationship(&server);
        let mut issues = Vec::new();

        validate_faa_make_relationship(
            &relationship,
            &proposed_faa_make,
            &research,
            &server,
            &catalog,
            &mut issues,
        );

        assert!(issues
            .iter()
            .any(|issue| issue.code == "faa_make_tcds_lineage_required"));

        catalog
            .ids_by_kind
            .entry(HierarchyEntityKind::Make)
            .or_default()
            .insert(18);
        catalog
            .identities_by_kind
            .entry(HierarchyEntityKind::Make)
            .or_default()
            .insert(
                18,
                AircraftCatalogCandidateIdentity {
                    entity_kind: HierarchyEntityKind::Make,
                    catalog_id: 18,
                    display_name: "Textron Aviation Inc.".to_string(),
                    authoritative_designator: None,
                    parent_catalog_id: None,
                },
            );
        issues.clear();
        validate_faa_make_relationship(
            &relationship,
            &proposed_faa_make,
            &research,
            &server,
            &catalog,
            &mut issues,
        );
        assert!(issues
            .iter()
            .any(|issue| issue.code == "faa_make_tcds_lineage_holder_ambiguous"));
    }

    #[test]
    fn non_exact_faa_make_mapping_requires_web_alias_and_applicability_proof() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let research = research_with_server(&server, "182");
        let mut adjudication = base_adjudication(&server, "182");
        adjudication.make.display_name = Some("Cessna".to_string());
        adjudication.make.authoritative_designator = Some("Cessna".to_string());
        adjudication.faa_make_relationship = FaaMakeRelationshipDecision {
            action: FaaMakeRelationshipAction::ProposeAlias,
            faa_manufacturer_name: "TEXTRON AVIATION INC".to_string(),
            canonical_make_name: "Cessna".to_string(),
            existing_alias_id: None,
            valid_from_model_year: None,
            valid_to_model_year: None,
            evidence_ids: vec![server.make_claim_id().to_string(), "identity".to_string()],
            applicability_evidence_ids: vec![],
            rationale: "identity claim alone lacks applicability".to_string(),
        };

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("one listing cannot imply an unbounded legal-make alias");
        assert!(error
            .0
            .iter()
            .any(|issue| { issue.code == "faa_make_relationship_missing_applicability_evidence" }));
    }

    #[test]
    fn proposed_faa_make_alias_requires_finite_evidence_bound_years() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let mut research = research_with_server(&server, "182");
        research.claims[0].evidence_excerpt =
            "Textron Aviation identifies Cessna aircraft for model year 2022.".to_string();
        research.claims[0]
            .supports
            .insert(EvidenceClaimKind::ProductionApplicability);
        let make = entity(EntityResolutionAction::ProposeNew, None, Some("Cessna"));
        let mut relationship = FaaMakeRelationshipDecision {
            action: FaaMakeRelationshipAction::ProposeAlias,
            faa_manufacturer_name: "TEXTRON AVIATION INC".to_string(),
            canonical_make_name: "Cessna".to_string(),
            existing_alias_id: None,
            valid_from_model_year: None,
            valid_to_model_year: None,
            evidence_ids: vec![server.make_claim_id().to_string(), "identity".to_string()],
            applicability_evidence_ids: vec!["identity".to_string()],
            rationale: "the source proves only the listing year".to_string(),
        };
        let mut issues = Vec::new();

        validate_faa_make_relationship(
            &relationship,
            &make,
            &research,
            &server,
            &exact_empty_catalog(&server),
            &mut issues,
        );
        assert!(issues.iter().any(|issue| {
            issue.code == "faa_make_relationship_new_alias_requires_finite_bounds"
        }));

        relationship.valid_from_model_year = Some(1956);
        relationship.valid_to_model_year = Some(2022);
        issues.clear();
        validate_faa_make_relationship(
            &relationship,
            &make,
            &research,
            &server,
            &exact_empty_catalog(&server),
            &mut issues,
        );
        assert!(issues.iter().any(|issue| {
            issue.code == "faa_make_relationship_bound_missing_from_applicability_evidence"
                && issue.message.contains("1956")
        }));
    }

    #[tokio::test]
    async fn catalog_search_reads_alias_identity_and_scope_columns_on_an_empty_catalog() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let response = search_approved_aircraft_catalog(
            &db,
            &AircraftCatalogSearchRequest {
                observed_make: "TEXTRON AVIATION INC".to_string(),
                observed_family: "182".to_string(),
                observed_designation: "182T".to_string(),
                observed_generation: None,
                observed_package: None,
                model_year: 2022,
            },
        )
        .await
        .expect("catalog query with typed alias metadata must be valid SQL");

        assert!(response.catalog_is_empty);
        assert!(response.candidates.is_empty());
    }

    #[test]
    fn designation_collisions_remain_visible_but_only_exact_canonical_designators_are_allowed() {
        for (faa_designation, catalog_designator, expected_allowed) in [
            ("T182T", "182T", false),
            ("182", "182T", false),
            ("182T", "182T", true),
        ] {
            let server = server_evidence("CESSNA AIRCRAFT COMPANY", faa_designation);
            let response = catalog_response_with_designation_candidate(
                &server,
                catalog_designator,
                catalog_designator,
                7,
            );

            assert!(
                response.candidates.iter().any(|candidate| {
                    candidate.entity_kind == HierarchyEntityKind::Designation
                        && candidate.catalog_id == 42
                }),
                "the {catalog_designator:?} collision must remain visible for FAA {faa_designation:?}"
            );
            assert_eq!(
                response
                    .allowed_existing_ids_by_kind
                    .get(&HierarchyEntityKind::Designation)
                    .is_some_and(|ids| ids.contains(&42)),
                expected_allowed,
                "FAA {faa_designation:?}, catalog {catalog_designator:?}"
            );
            let registry = response.candidate_registry();
            assert!(
                registry
                    .identity(HierarchyEntityKind::Designation, 42)
                    .is_some(),
                "forbidden collision identity must remain auditable"
            );
            assert_eq!(
                registry.contains(HierarchyEntityKind::Designation, 42),
                expected_allowed,
                "validation allowlist must preserve the search decision"
            );
        }
    }

    #[test]
    fn existing_designation_requires_exact_canonical_designator_label_and_parent() {
        let server = server_evidence("CESSNA AIRCRAFT COMPANY", "T182T");
        let family = server.observation_bindings[0].observed_model.clone();
        let research = research_with_server(&server, &family);
        let adjudication = adjudication_matching_catalog_branch(&server);

        let exact = catalog_response_with_designation_candidate(&server, "T182T", "T182T", 7);
        validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact.candidate_registry(),
            1,
        )
        .expect("an exact canonical T182T candidate on the selected family is allowed");

        let mut wrong_authoritative =
            catalog_response_with_designation_candidate(&server, "T182T", "182T", 7);
        // Simulate a malformed/stale function allowlist. Validation must
        // recompute the exact identity invariant instead of trusting it.
        wrong_authoritative
            .allowed_existing_ids_by_kind
            .entry(HierarchyEntityKind::Designation)
            .or_default()
            .push(42);
        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &wrong_authoritative.candidate_registry(),
            1,
        )
        .expect_err("catalog 182T cannot satisfy exact FAA T182T");
        assert!(error.0.iter().any(|issue| {
            issue.code == "designation_catalog_authoritative_designator_mismatch"
        }));

        let wrong_display =
            catalog_response_with_designation_candidate(&server, "182T", "T182T", 7);
        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &wrong_display.candidate_registry(),
            1,
        )
        .expect_err("an existing ID cannot be paired with a substituted display label");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "designation_catalog_candidate_label_mismatch"));

        let wrong_parent =
            catalog_response_with_designation_candidate(&server, "T182T", "T182T", 8);
        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &wrong_parent.candidate_registry(),
            1,
        )
        .expect_err("an exact designation ID from another family branch is forbidden");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "designation_catalog_candidate_parent_mismatch"));

        let mut proposed_family = adjudication;
        proposed_family.family.action = EntityResolutionAction::ProposeNew;
        proposed_family.family.existing_catalog_id = None;
        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &proposed_family,
            &exact.candidate_registry(),
            1,
        )
        .expect_err("an existing designation cannot be attached to a new family");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "designation_catalog_existing_family_required"));
    }

    #[test]
    fn proposed_designation_display_must_literally_preserve_the_faa_designation() {
        let server = server_evidence("CESSNA AIRCRAFT COMPANY", "T182T");
        let family = server.observation_bindings[0].observed_model.clone();
        let research = research_with_server(&server, &family);
        let mut adjudication = base_adjudication(&server, &family);
        adjudication.designation.display_name = Some("182T".to_string());

        let error = validate_aircraft_hierarchy_adjudication(
            &research,
            &grounded_web(),
            &server,
            &adjudication,
            &exact_empty_catalog(&server),
            1,
        )
        .expect_err("a friendly 182T display cannot create the distinct FAA T182T identity");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "new_designation_display_name_mismatch"));

        let prompt = build_hierarchy_adjudication_prompt(&[], &research);
        assert!(prompt.contains(
            "a `propose_new` designation's `display_name` must literally preserve that same exact FAA value"
        ));
    }

    #[test]
    fn catalog_registry_keeps_family_aliases_separate_from_make_aliases() {
        let family_alias = AircraftCatalogAliasCandidate {
            alias_id: 41,
            owner_catalog_id: 7,
            alias: "182".to_string(),
            valid_from_model_year: Some(1956),
            valid_to_model_year: None,
            market_code: Some("US".to_string()),
        };
        let response = AircraftCatalogSearchResponse {
            catalog_revision: "sha256:family-alias".to_string(),
            catalog_is_empty: false,
            search_request: AircraftCatalogSearchRequest {
                observed_make: "TEXTRON AVIATION INC".to_string(),
                observed_family: "182".to_string(),
                observed_designation: "182T".to_string(),
                observed_generation: None,
                observed_package: None,
                model_year: 2022,
            },
            allowed_existing_ids_by_kind: BTreeMap::new(),
            candidates: vec![AircraftCatalogCandidate {
                entity_kind: HierarchyEntityKind::Family,
                catalog_id: 7,
                display_name: "Skylane".to_string(),
                authoritative_designator: None,
                parent_catalog_id: Some(3),
                aliases: vec!["182".to_string()],
                approved_aliases: vec![family_alias.clone()],
                identifiers: Vec::new(),
                retrieval_score: 1.0,
                retrieval_reasons: vec!["exact_alias".to_string()],
            }],
            generation_designations: Vec::new(),
            package_applicability: Vec::new(),
            warning: "fixture".to_string(),
        };

        let registry = response.candidate_registry();
        assert_eq!(registry.family_aliases_by_id.get(&41), Some(&family_alias));
        assert!(registry.make_aliases_by_id.is_empty());
        assert_eq!(
            registry
                .identity(HierarchyEntityKind::Family, 7)
                .map(|candidate| (
                    candidate.entity_kind,
                    candidate.display_name.as_str(),
                    candidate.parent_catalog_id,
                )),
            Some((HierarchyEntityKind::Family, "Skylane", Some(3)))
        );
    }

    #[test]
    fn catalog_revision_includes_generation_and_package_relationships() {
        let base_rows = Vec::new();
        let lookup_rows = Vec::new();
        let empty_revision = catalog_revision(&base_rows, &lookup_rows, &[], &[]);
        let generation_designations = vec![AircraftCatalogGenerationDesignationRow {
            aircraft_generation_id: 7,
            aircraft_designation_id: 11,
        }];
        let generation_revision =
            catalog_revision(&base_rows, &lookup_rows, &generation_designations, &[]);
        let package_applicability = vec![AircraftCatalogPackageApplicabilityRow {
            applicability_id: 13,
            aircraft_factory_package_id: 17,
            package_kind: "trim_tier".to_string(),
            aircraft_designation_id: 11,
            aircraft_generation_id: Some(7),
            valid_from_model_year: Some(2020),
            valid_to_model_year: Some(2023),
        }];
        let complete_revision = catalog_revision(
            &base_rows,
            &lookup_rows,
            &generation_designations,
            &package_applicability,
        );

        assert_ne!(empty_revision, generation_revision);
        assert_ne!(generation_revision, complete_revision);
    }

    #[test]
    fn evidence_requires_an_observed_search_and_cited_urls() {
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "Cessna 182T".to_string(),
            claims: vec![claim("identity")],
            family_candidates: Vec::new(),
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: vec![],
            unresolved_questions: vec![],
        };
        let error = validate_identity_evidence_research(
            &research,
            &GroundingAudit::default(),
            &server_evidence("Cessna", "182T"),
        )
        .unwrap_err();
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
    fn identity_research_prompt_requires_first_party_source_selection() {
        let prompt = build_identity_evidence_prompt(&[]);

        assert!(prompt.contains(AIRCRAFT_IDENTITY_PROMPT_VERSION));
        assert!(prompt.contains("site-restricted searches"));
        assert!(prompt.contains("legal-manufacturer-to-marketing-brand relationship"));
        assert!(prompt.contains("exact model family"));
        assert!(prompt.contains("listing model year"));
        assert!(prompt.contains("no supported selection"));
        assert!(prompt.contains("family_candidates"));
        assert!(prompt.contains("generation_candidates"));
        assert!(prompt.contains("Do not turn G1000/G1000 NXi"));
        assert!(prompt.contains("reseller-hosted copies"));
        assert!(prompt.contains("Do not cite rejected secondary pages"));
    }

    #[test]
    fn adjudication_contract_exposes_the_case_bound_tcds_family_action() {
        let schema = hierarchy_adjudication_response_schema();
        let action = &schema["properties"]["family_label_relationship"]["properties"]["action"];
        let actions = action["enum"]
            .as_array()
            .expect("family relationship actions are an enum");
        assert!(actions
            .iter()
            .any(|value| value == "match_faa_type_certificate_family"));
        assert!(actions
            .iter()
            .any(|value| value == "match_manufacturer_series_family"));
        let action_description = action["description"]
            .as_str()
            .expect("TCDS action has a semantic description");
        assert!(action_description.contains("case-bound server FAA DRS projection"));
        assert!(action_description.contains("retained label remains audit input"));
        assert!(action_description
            .contains("neither case-bound action is a catalog alias or year interval"));
        let evidence_description = schema["properties"]["family_label_relationship"]["properties"]
            ["evidence_ids"]["description"]
            .as_str()
            .expect("TCDS evidence has an exact-set description");
        assert!(evidence_description.contains("exactly every server_faa_drs.* claim ID"));
        assert!(evidence_description.contains("none omitted, added, or substituted"));

        let prompt = build_hierarchy_adjudication_prompt(
            &[],
            &AircraftIdentityEvidenceResearch {
                subject_summary: "TCDS contract fixture".to_string(),
                claims: Vec::new(),
                family_candidates: Vec::new(),
                generation_candidates: Vec::new(),
                package_candidates: Vec::new(),
                contradictions: Vec::new(),
                unresolved_questions: Vec::new(),
            },
        );
        assert!(prompt.contains("MUST instead use `match_faa_type_certificate_family`"));
        assert!(prompt
            .contains("include exactly all of that projection's `server_faa_drs.*` claim IDs"));
        assert!(prompt.contains("set alias id and both model-year bounds to null"));
        assert!(prompt.contains("must never create a catalog alias"));
        assert!(prompt.contains("Use `match_manufacturer_series_family` only when"));
        assert!(prompt.contains("never consumes the complete label wholesale"));
    }

    #[test]
    fn structure_contract_prefers_covering_intervals_and_exact_family_components() {
        let observation = AircraftIdentityObservation {
            listing_id: 91,
            submission_id: Some(12),
            source_url: Some("https://listing.invalid/sentinel".to_string()),
            rendered_html_sha256: Some("a".repeat(64)),
            manufacturer: "Skyloom".to_string(),
            model: "417".to_string(),
            variant: "ZX9 Aurora".to_string(),
            model_year: 2017,
            serial_number: Some("sentinel-serial".to_string()),
            registration_number: Some("N123AB".to_string()),
            source_excerpt: Some("sentinel observation".to_string()),
            source_excerpt_is_exact: true,
            source_kind: "rendered_html".to_string(),
            observation_sha256: "e".repeat(64),
            cluster_key: "sentinel:skyloom:417:2017".to_string(),
            requires_human_review: false,
            review_reasons: vec![],
        };
        let observations = [&observation];
        let prompt = build_identity_evidence_prompt(&observations);

        assert!(prompt.contains("\"manufacturer\": \"Skyloom\""));
        assert!(prompt.contains("\"model\": \"417\""));
        assert!(prompt.contains("\"variant\": \"ZX9 Aurora\""));
        assert!(prompt.contains("\"model_year\": 2017"));
        assert!(prompt.contains("inventory every direct-primary source span"));
        assert!(prompt.contains("explicitly supplies a finite interval"));
        assert!(prompt.contains("containing all listing model years"));
        assert!(prompt.contains("never justify choosing a non-covering interval"));
        assert!(prompt.contains("only the exact OEM family-name component"));
        assert!(prompt.contains("Never copy the entire co-naming heading or excerpt"));
        assert!(prompt.contains("evidence coverage for every distinct proof need"));
        assert!(prompt.contains("do not duplicate the same span under redundant IDs"));
        assert!(prompt.contains("exact family co-naming relationship"));
        assert!(prompt.contains("finite family production applicability"));
        assert!(prompt.contains("finite FAA-legal-make-to-brand applicability"));
        assert!(prompt.contains("case-bound manufacturer-series/family path"));
        assert!(prompt
            .contains("requires that hierarchy co-naming but no production-year bounds and no `production_applicability` claim"));

        let schema = identity_evidence_response_schema();
        let claims_description = schema["properties"]["claims"]["description"]
            .as_str()
            .expect("claims schema has a semantic description");
        assert!(claims_description.contains("distinct proof needs"));
        assert!(claims_description.contains("One exact span may support multiple kinds"));
        assert!(claims_description.contains("family co-naming"));
        assert!(claims_description.contains("legal-make-to-brand applicability"));
        assert!(claims_description.contains(
            "case-bound numeric-series/family relationship requires exact OEM hierarchy co-naming but no production-year or production-applicability claim"
        ));
        let family_description = schema["properties"]["family_candidates"]["description"]
            .as_str()
            .expect("family schema has a semantic description");
        assert!(family_description.contains("exact OEM family-name component"));
        assert!(family_description.contains("retained numeric model label"));
        assert!(family_description.contains("certified designation"));
        let optional_evidence_description = schema["properties"]["generation_candidates"]["items"]
            ["properties"]["evidence_ids"]["description"]
            .as_str()
            .expect("candidate evidence schema has a semantic description");
        assert!(optional_evidence_description
            .contains("exact excerpt names this exact candidate label"));

        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "sentinel".to_string(),
            claims: Vec::new(),
            family_candidates: Vec::new(),
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: Vec::new(),
            unresolved_questions: Vec::new(),
        };
        let adjudication_prompt = build_hierarchy_adjudication_prompt(&observations, &research);
        assert!(adjudication_prompt.contains("compare every explicit finite interval"));
        assert!(adjudication_prompt.contains("select its claim ID"));
        assert!(adjudication_prompt.contains("distinct proof needs"));
        assert!(adjudication_prompt.contains("one exact span may satisfy more than one need"));
    }

    #[test]
    fn evidence_schema_constrains_unresolved_scopes_to_the_server_allowlist() {
        let schema = identity_evidence_response_schema_with_unresolved_scopes(&[
            ResearchUnresolvedScope::SourceIntegrity,
            ResearchUnresolvedScope::Other,
            ResearchUnresolvedScope::SourceIntegrity,
        ]);
        assert_eq!(
            schema["properties"]["unresolved_questions"]["items"]["properties"]["scope"]["enum"],
            json!(["source_integrity"]),
            "the schema preserves the server order, removes duplicates, and never exposes the legacy catch-all"
        );
        assert!(
            schema["properties"]["unresolved_questions"]
                .get("maxItems")
                .is_none(),
            "a non-empty allowlist must still permit unresolved reports"
        );
        assert_eq!(
            schema["properties"]["claims"]["items"]["properties"]["supports"]["items"]["enum"],
            json!(["hierarchy_identity", "production_applicability"]),
            "aircraft identity research must not be primed with configuration or valuation claim kinds"
        );
        assert_eq!(
            schema["properties"]["claims"]["items"]["properties"]["supports"]["minItems"],
            json!(1),
            "every generated aircraft identity claim must state at least one supported proof kind"
        );

        let default_schema = identity_evidence_response_schema();
        assert_eq!(
            default_schema["properties"]["unresolved_questions"]["items"]["properties"]["scope"]
                ["enum"]
                .as_array()
                .map(Vec::len),
            Some(ALL_RESEARCH_UNRESOLVED_SCOPES.len() - 1)
        );
        assert!(
            !default_schema["properties"]["unresolved_questions"]["items"]["properties"]["scope"]
                ["enum"]
                .as_array()
                .expect("unresolved scopes are an enum")
                .iter()
                .any(|scope| scope == "other")
        );
    }

    #[test]
    fn evidence_schema_empty_unresolved_scope_allowlist_is_valid_and_forbids_items() {
        let schema = identity_evidence_response_schema_with_unresolved_scopes(&[]);
        assert_eq!(
            schema["properties"]["unresolved_questions"]["maxItems"],
            json!(0)
        );
        assert_eq!(
            schema["properties"]["unresolved_questions"]["items"]["properties"]["scope"]["enum"],
            json!(["other"]),
            "the unreachable item schema must retain a valid non-empty enum"
        );
    }

    #[test]
    fn evidence_contract_excludes_non_identity_research_gaps_without_a_catch_all() {
        let schema = identity_evidence_response_schema_with_unresolved_scopes(&[
            ResearchUnresolvedScope::Other,
        ]);
        assert_eq!(
            schema["properties"]["unresolved_questions"]["maxItems"],
            json!(0),
            "a catch-all-only allowlist must not let Gemini manufacture an identity blocker"
        );
        let unresolved_description = schema["properties"]["unresolved_questions"]["description"]
            .as_str()
            .expect("unresolved questions have a semantic description");
        assert!(unresolved_description.contains("never a general research backlog"));
        assert!(unresolved_description.contains("factory-default equipment"));
        assert!(unresolved_description.contains("price"));
        assert!(unresolved_description.contains("FAA/listing disagreement"));
        assert!(unresolved_description.contains("optional make/brand-alias search is not a gap"));
        assert!(
            unresolved_description.contains("exact FAA legal make is the deterministic fallback")
        );
        let contradictions_description = schema["properties"]["contradictions"]["description"]
            .as_str()
            .expect("contradictions have a semantic description");
        assert!(contradictions_description.contains("Actual aircraft-identity conflicts only"));
        assert!(contradictions_description.contains("controlling FAA data"));
        assert!(contradictions_description.contains("equipment"));
        assert!(contradictions_description.contains("value is not a contradiction"));

        let prompt = build_identity_evidence_prompt(&[]);
        assert!(prompt.contains("not a general research backlog"));
        assert!(prompt.contains("There is intentionally no generated catch-all scope"));
        assert!(prompt.contains(
            "Never report installation applicability, actual or factory-default equipment"
        ));
        assert!(prompt.contains("an equipment label such as G1000 or NXi is outside"));
        assert!(prompt.contains("actual source or FAA/listing disagreement"));
        assert!(prompt.contains("citation, retrieval, publisher-authority, or provenance"));
        assert!(prompt.contains("opportunistic alias proposal, not a required identity gap"));
        assert!(prompt.contains("exact FAA legal make is the deterministic fallback"));
        assert!(prompt.contains("neither source-integrity uncertainty nor an identity gap"));
    }

    fn sentinel_server_evidence() -> ServerFaaIdentityEvidence {
        let mut server = server_evidence("ORBITAL AIRFRAME GROUP", "ZX9");
        server.observation_bindings[0].observed_make = "Skyloom".to_string();
        server.observation_bindings[0].observed_model = "9".to_string();
        server.observation_bindings[0].observed_variant = "ZX9 Falcon".to_string();
        server.observation_bindings[0].listing_model_year = 2007;
        server.observation_bindings[0].grounding.year_manufactured = Some(2006);
        server.listing_model_years = BTreeSet::from([2007]);
        server.faa_years_manufactured = BTreeSet::from([2006]);
        server
    }

    fn sentinel_claim(
        evidence_id: &str,
        excerpt: &str,
        supports: impl IntoIterator<Item = EvidenceClaimKind>,
    ) -> EvidenceClaimProposal {
        EvidenceClaimProposal {
            evidence_id: evidence_id.to_string(),
            source_url: format!("https://manufacturer.example/{evidence_id}"),
            source_title: "Sentinel official history".to_string(),
            evidence_excerpt: excerpt.to_string(),
            source_kind: EvidenceSourceKind::Manufacturer,
            supports: supports.into_iter().collect(),
        }
    }

    #[test]
    fn optional_candidates_require_primary_evidence_to_exact_name_the_label() {
        let server = sentinel_server_evidence();
        for (kind, label) in [
            (HierarchyEntityKind::Generation, "G6"),
            (HierarchyEntityKind::Package, "GTS"),
        ] {
            let evidence_id = format!("unnamed-{}", kind.as_str());
            let candidate = HierarchyCandidate {
                label: label.to_string(),
                evidence_ids: vec![evidence_id.clone()],
            };
            let research = AircraftIdentityEvidenceResearch {
                subject_summary: "optional label evidence fixture".to_string(),
                claims: vec![sentinel_claim(
                    &evidence_id,
                    "The manufacturer describes the current aircraft product line.",
                    [EvidenceClaimKind::HierarchyIdentity],
                )],
                family_candidates: Vec::new(),
                generation_candidates: (kind == HierarchyEntityKind::Generation)
                    .then_some(candidate.clone())
                    .into_iter()
                    .collect(),
                package_candidates: (kind == HierarchyEntityKind::Package)
                    .then_some(candidate.clone())
                    .into_iter()
                    .collect(),
                contradictions: Vec::new(),
                unresolved_questions: Vec::new(),
            };
            let candidates = if kind == HierarchyEntityKind::Generation {
                &research.generation_candidates
            } else {
                &research.package_candidates
            };
            let mut issues = Vec::new();

            validate_hierarchy_candidates(kind, candidates, &research, &server, &mut issues);

            assert!(issues.iter().any(|issue| {
                issue.code == "hierarchy_candidate_evidence_label_mismatch"
                    && issue.message.contains(&evidence_id)
            }));
            let decision = entity_with_evidence(
                EntityResolutionAction::ProposeNew,
                None,
                Some(label),
                vec![evidence_id],
            );
            assert!(!decision_has_exact_typed_candidate(
                kind, &decision, &research, &server
            ));
        }
    }

    #[test]
    fn optional_candidate_rejects_mixed_naming_and_unrelated_primary_evidence() {
        let server = sentinel_server_evidence();
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "mixed optional evidence fixture".to_string(),
            claims: vec![
                sentinel_claim(
                    "g6-naming",
                    "The manufacturer identifies G6 as this aircraft generation.",
                    [EvidenceClaimKind::HierarchyIdentity],
                ),
                sentinel_claim(
                    "unrelated-hierarchy",
                    "The manufacturer identifies Perspective as an avionics suite.",
                    [EvidenceClaimKind::HierarchyIdentity],
                ),
            ],
            family_candidates: Vec::new(),
            generation_candidates: vec![HierarchyCandidate {
                label: "G6".to_string(),
                evidence_ids: vec!["g6-naming".to_string(), "unrelated-hierarchy".to_string()],
            }],
            package_candidates: Vec::new(),
            contradictions: Vec::new(),
            unresolved_questions: Vec::new(),
        };
        let mut issues = Vec::new();

        validate_hierarchy_candidates(
            HierarchyEntityKind::Generation,
            &research.generation_candidates,
            &research,
            &server,
            &mut issues,
        );

        assert!(issues.iter().any(|issue| {
            issue.code == "hierarchy_candidate_evidence_label_mismatch"
                && issue.message.contains("unrelated-hierarchy")
        }));
        assert!(!issues.iter().any(|issue| {
            issue.code == "hierarchy_candidate_evidence_label_mismatch"
                && issue.message.contains("g6-naming")
        }));
        let decision = entity_with_evidence(
            EntityResolutionAction::ProposeNew,
            None,
            Some("G6"),
            vec!["g6-naming".to_string(), "unrelated-hierarchy".to_string()],
        );
        assert!(!decision_has_exact_typed_candidate(
            HierarchyEntityKind::Generation,
            &decision,
            &research,
            &server,
        ));
    }

    #[test]
    fn family_candidate_must_be_an_exact_evidenced_component_not_a_composite() {
        let server = sentinel_server_evidence();
        let identity = sentinel_claim(
            "family-identity",
            "The Skyloom Falcon 9 ZX9 family appears in the official product history.",
            [EvidenceClaimKind::HierarchyIdentity],
        );
        let mut research = AircraftIdentityEvidenceResearch {
            subject_summary: "sentinel".to_string(),
            claims: vec![identity],
            family_candidates: vec![HierarchyCandidate {
                label: "Skyloom Falcon 9 ZX9".to_string(),
                evidence_ids: vec!["family-identity".to_string()],
            }],
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: Vec::new(),
            unresolved_questions: Vec::new(),
        };
        let mut issues = Vec::new();

        validate_hierarchy_candidates(
            HierarchyEntityKind::Family,
            &research.family_candidates,
            &research,
            &server,
            &mut issues,
        );
        assert!(issues
            .iter()
            .any(|issue| issue.code == "family_candidate_not_exact_oem_component"));
        assert!(!issues
            .iter()
            .any(|issue| { issue.code == "family_candidate_label_absent_from_primary_evidence" }));

        for forbidden_exact_label in ["9", "ZX9"] {
            research.family_candidates[0].label = forbidden_exact_label.to_string();
            issues.clear();
            validate_hierarchy_candidates(
                HierarchyEntityKind::Family,
                &research.family_candidates,
                &research,
                &server,
                &mut issues,
            );
            assert!(
                issues
                    .iter()
                    .any(|issue| issue.code == "family_candidate_not_exact_oem_component"),
                "distinct retained family hint must prevent {forbidden_exact_label:?} from becoming the family: {issues:?}"
            );
        }

        research.family_candidates[0].label = "Aurora".to_string();
        issues.clear();
        validate_hierarchy_candidates(
            HierarchyEntityKind::Family,
            &research.family_candidates,
            &research,
            &server,
            &mut issues,
        );
        assert!(issues
            .iter()
            .any(|issue| { issue.code == "family_candidate_label_absent_from_primary_evidence" }));

        research.family_candidates[0].label = "Falcon".to_string();
        issues.clear();
        validate_hierarchy_candidates(
            HierarchyEntityKind::Family,
            &research.family_candidates,
            &research,
            &server,
            &mut issues,
        );
        assert!(!issues.iter().any(|issue| {
            issue.code == "family_candidate_not_exact_oem_component"
                || issue.code == "family_candidate_label_absent_from_primary_evidence"
        }));
    }

    #[test]
    fn alias_evidence_requires_one_coherent_interval_and_exact_make_brand_conaming() {
        let server = sentinel_server_evidence();
        let claims = vec![
            sentinel_claim(
                "make-identity",
                "ORBITAL AIRFRAME GROUP and Skyloom formed the aircraft brand relationship in 2007.",
                [
                    EvidenceClaimKind::HierarchyIdentity,
                    EvidenceClaimKind::ProductionApplicability,
                ],
            ),
            sentinel_claim(
                "lower-bound",
                "The applicable relationship began in 2005.",
                [EvidenceClaimKind::ProductionApplicability],
            ),
            sentinel_claim(
                "upper-bound",
                "The applicable relationship continued through 2010.",
                [EvidenceClaimKind::ProductionApplicability],
            ),
            sentinel_claim(
                "coherent-interval",
                "The applicable relationship ran from 2005 through 2010.",
                [EvidenceClaimKind::ProductionApplicability],
            ),
            sentinel_claim(
                "anniversary-years",
                "Production began in 1955, and the aircraft anniversary was celebrated in 2025.",
                [EvidenceClaimKind::ProductionApplicability],
            ),
        ];
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "sentinel".to_string(),
            claims,
            family_candidates: Vec::new(),
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: Vec::new(),
            unresolved_questions: Vec::new(),
        };
        let mut issues = Vec::new();

        validate_proposed_alias_evidence_bounds(
            Some(2005),
            Some(2010),
            &["lower-bound".to_string(), "upper-bound".to_string()],
            &research,
            &server,
            "sentinel_alias",
            "sentinel alias",
            &mut issues,
        );
        assert!(issues.iter().any(|issue| {
            issue.code
                == "sentinel_alias_finite_interval_missing_from_single_applicability_evidence"
        }));

        issues.clear();
        validate_proposed_alias_evidence_bounds(
            Some(2005),
            Some(2010),
            &["coherent-interval".to_string()],
            &research,
            &server,
            "sentinel_alias",
            "sentinel alias",
            &mut issues,
        );
        assert!(issues.is_empty(), "coherent covering interval: {issues:?}");

        issues.clear();
        validate_proposed_alias_evidence_bounds(
            Some(1955),
            Some(2025),
            &["anniversary-years".to_string()],
            &research,
            &server,
            "sentinel_alias",
            "sentinel alias",
            &mut issues,
        );
        assert!(issues.iter().any(|issue| {
            issue.code
                == "sentinel_alias_finite_interval_missing_from_single_applicability_evidence"
        }));

        let relationship = FaaMakeRelationshipDecision {
            action: FaaMakeRelationshipAction::ProposeAlias,
            faa_manufacturer_name: "ORBITAL AIRFRAME GROUP".to_string(),
            canonical_make_name: "Skyloom".to_string(),
            existing_alias_id: None,
            valid_from_model_year: Some(2007),
            valid_to_model_year: Some(2007),
            evidence_ids: vec!["make-identity".to_string()],
            applicability_evidence_ids: vec!["make-identity".to_string()],
            rationale: "sentinel".to_string(),
        };
        issues.clear();
        validate_alias_web_evidence_and_scope(&relationship, &research, &server, &mut issues);
        assert!(!issues
            .iter()
            .any(|issue| issue.code == "faa_make_relationship_missing_web_evidence"));
        assert!(!issues
            .iter()
            .any(|issue| issue.code == "faa_make_relationship_year_out_of_scope"));

        let mut missing_legal_make = research.clone();
        missing_legal_make.claims[0].evidence_excerpt =
            "Skyloom formed the aircraft brand relationship in 2007.".to_string();
        issues.clear();
        validate_alias_web_evidence_and_scope(
            &relationship,
            &missing_legal_make,
            &server,
            &mut issues,
        );
        assert!(issues
            .iter()
            .any(|issue| issue.code == "faa_make_relationship_missing_web_evidence"));

        let family_relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::ProposeAlias,
            observed_family_label: "9".to_string(),
            canonical_family_name: "Falcon".to_string(),
            existing_alias_id: None,
            valid_from_model_year: Some(2007),
            valid_to_model_year: Some(2007),
            evidence_ids: Vec::new(),
            applicability_evidence_ids: Vec::new(),
            rationale: "sentinel".to_string(),
        };
        issues.clear();
        validate_family_label_year_scope(&family_relationship, &server, &mut issues);
        assert!(!issues
            .iter()
            .any(|issue| issue.code == "family_label_relationship_year_out_of_scope"));
    }

    #[test]
    fn obvious_secondary_copies_cannot_masquerade_as_primary_identity_evidence() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        for source_url in [
            "https://en.wikipedia.org/wiki/Cessna_182_Skylane",
            "https://airmart.com/wp-content/uploads/Cessna_182_2022_Skylane_Brochure.pdf",
            "https://manuals.plus/m/cessna-182t-maintenance-manual",
        ] {
            let mut research = research_with_server(&server, "182");
            research.claims[0].source_url = source_url.to_string();
            research.claims[0].source_kind = EvidenceSourceKind::Manufacturer;
            let grounding = GroundingAudit {
                mode: GroundingMode::FreshWeb,
                google_search_call_count: 1,
                url_context_call_count: 1,
                citation_urls: [source_url.to_string()].into_iter().collect(),
                reused_verified_dossier: false,
            };

            let error = validate_identity_evidence_research(&research, &grounding, &server)
                .expect_err("a third-party host cannot be relabeled as a primary publisher");
            assert!(
                error
                    .0
                    .iter()
                    .any(|issue| issue.code == "third_party_source_mislabeled_primary"),
                "missing source-host error for {source_url}: {error}"
            );
        }
    }

    #[test]
    fn direct_official_manufacturer_hosts_remain_eligible_for_primary_evidence() {
        let server = server_evidence("TEXTRON AVIATION INC", "182T");
        let source_url =
            "https://media.txtav.com/197032-celebrating-65-years-of-the-legendary-cessna-skylane/";
        let mut research = research_with_server(&server, "182");
        research.claims[0].source_url = source_url.to_string();
        let grounding = GroundingAudit {
            mode: GroundingMode::FreshWeb,
            google_search_call_count: 1,
            url_context_call_count: 1,
            citation_urls: [source_url.to_string()].into_iter().collect(),
            reused_verified_dossier: false,
        };

        validate_identity_evidence_research(&research, &grounding, &server)
            .expect("an OEM-hosted page remains valid primary evidence");
    }

    #[test]
    fn n_registered_identity_cannot_label_a_non_faa_host_as_regulator_authority() {
        let mut regulator_claim = claim("identity");
        regulator_claim.source_url = "https://regulator.example/identity".to_string();
        regulator_claim.source_kind = EvidenceSourceKind::Regulator;
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "Cessna 182T".to_string(),
            claims: vec![regulator_claim],
            family_candidates: Vec::new(),
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: vec![],
            unresolved_questions: vec![],
        };
        let grounding = GroundingAudit {
            mode: GroundingMode::FreshWeb,
            google_search_call_count: 1,
            url_context_call_count: 1,
            citation_urls: ["https://regulator.example/identity".to_string()]
                .into_iter()
                .collect(),
            reused_verified_dossier: false,
        };

        let error = validate_identity_evidence_research(
            &research,
            &grounding,
            &server_evidence("Cessna", "182T"),
        )
        .expect_err("FAA identity authority must come from an FAA host");

        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "non_faa_regulator_source"));
    }

    #[test]
    fn match_existing_must_come_from_live_catalog_results() {
        let server = server_evidence("Cessna", "182T");
        let research = research_with_server(&server, "182");
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
            faa_make_relationship: exact_relationship(&server),
            family: entity(EntityResolutionAction::ProposeNew, None, Some("182")),
            family_label_relationship: exact_family_relationship("182"),
            designation: entity_with_evidence(
                EntityResolutionAction::ProposeNew,
                None,
                Some("182T"),
                vec![server.designation_claim_id().to_string()],
            ),
            generation: entity(EntityResolutionAction::NoSupportedSelection, None, None),
            package: entity(EntityResolutionAction::NoSupportedSelection, None, None),
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
            &server,
            &fetched_source_proofs(&research, &server),
            adjudication,
            &exact_empty_catalog(&server),
            1,
            verification,
            &grounding,
            false,
        )
        .unwrap_err();
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "catalog_id_not_retrieved"));
    }

    #[test]
    fn unresolved_optional_dimension_blocks_reviewability() {
        let server = server_evidence("Cirrus", "SR22");
        let research = research_with_server(&server, "SR22");
        let grounding = GroundingAudit {
            google_search_call_count: 1,
            citation_urls: ["https://manufacturer.example/identity".to_string()]
                .into_iter()
                .collect(),
            ..GroundingAudit::default()
        };
        let adjudication = AircraftHierarchyAdjudication {
            confidence: CurationConfidence::VeryHigh,
            make: entity_with_evidence(
                EntityResolutionAction::ProposeNew,
                None,
                Some("Cirrus"),
                vec![server.make_claim_id().to_string()],
            ),
            faa_make_relationship: exact_relationship(&server),
            family: entity(EntityResolutionAction::ProposeNew, None, Some("SR22")),
            family_label_relationship: exact_family_relationship("SR22"),
            designation: entity_with_evidence(
                EntityResolutionAction::ProposeNew,
                None,
                Some("SR22"),
                vec![server.designation_claim_id().to_string()],
            ),
            generation: entity(EntityResolutionAction::Unresolved, None, None),
            package: entity(EntityResolutionAction::NoSupportedSelection, None, None),
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
            &server,
            &fetched_source_proofs(&research, &server),
            adjudication,
            &exact_empty_catalog(&server),
            1,
            verification,
            &grounding,
            false,
        )
        .unwrap_err();
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "unresolved_hierarchy_dimension"));
    }

    #[test]
    fn verifier_accepts_only_an_exact_reused_verified_dossier() {
        let server = server_evidence("Cessna", "182T");
        let research = research_with_server(&server, "182");
        let evidence_grounding = GroundingAudit {
            mode: GroundingMode::FreshWeb,
            google_search_call_count: 1,
            url_context_call_count: 1,
            citation_urls: ["https://manufacturer.example/identity".to_string()]
                .into_iter()
                .collect(),
            reused_verified_dossier: false,
        };
        let adjudication = AircraftHierarchyAdjudication {
            confidence: CurationConfidence::VeryHigh,
            make: entity_with_evidence(
                EntityResolutionAction::ProposeNew,
                None,
                Some("Cessna"),
                vec![server.make_claim_id().to_string()],
            ),
            faa_make_relationship: exact_relationship(&server),
            family: entity(EntityResolutionAction::ProposeNew, None, Some("182")),
            family_label_relationship: exact_family_relationship("182"),
            designation: entity_with_evidence(
                EntityResolutionAction::ProposeNew,
                None,
                Some("182T"),
                vec![server.designation_claim_id().to_string()],
            ),
            generation: entity(EntityResolutionAction::NoSupportedSelection, None, None),
            package: entity(EntityResolutionAction::NoSupportedSelection, None, None),
            material_distinctions: vec!["182T differs from T182T".to_string()],
            unresolved_questions: vec![],
            rationale: "primary sources agree".to_string(),
        };
        let verification = AircraftHierarchyVerification {
            verdict: VerificationVerdict::Confirm,
            confidence: CurationConfidence::VeryHigh,
            verified_evidence_ids: vec![
                "identity".to_string(),
                server.make_claim_id().to_string(),
                server.designation_claim_id().to_string(),
            ],
            differentiation_checks: vec![],
            errors: vec![],
            rationale: "confirmed from the bound dossier".to_string(),
        };
        let reused_grounding = GroundingAudit {
            mode: GroundingMode::ReusedVerifiedDossier,
            google_search_call_count: 0,
            url_context_call_count: 0,
            citation_urls: evidence_grounding.citation_urls.clone(),
            reused_verified_dossier: true,
        };

        let reviewable = build_reviewable_aircraft_hierarchy(
            &research,
            &evidence_grounding,
            &server,
            &fetched_source_proofs(&research, &server),
            adjudication.clone(),
            &exact_empty_catalog(&server),
            1,
            verification.clone(),
            &reused_grounding,
            false,
        )
        .expect("exact reused dossier remains valid grounding provenance");
        let serialized = serde_json::to_value(&reviewable).unwrap();
        let serialized_proof = &serialized["direct_source_proofs"]["by_evidence_id"]["identity"];
        assert_eq!(
            serialized_proof["final_url"],
            "https://manufacturer.example/identity"
        );
        assert_eq!(
            serialized_proof["content_sha256"].as_str().map(str::len),
            Some(64)
        );
        assert_eq!(
            serialized_proof["normalized_span_sha256"]
                .as_str()
                .map(str::len),
            Some(64)
        );
        assert!(
            serialized_proof.get("span_proof").is_none(),
            "transient normalized source material must not be serialized"
        );
        let exact_binding = &server.observation_bindings[0];
        reviewable
            .require_server_faa_observation_binding(
                exact_binding.listing_id,
                &exact_binding.observation_sha256,
                exact_binding.listing_model_year,
                &exact_binding.grounding,
            )
            .expect("the opaque approval remains bound to its exact FAA case member");
        let mut replayed_grounding = exact_binding.grounding.clone();
        replayed_grounding.source_record_sha256 = "9".repeat(64);
        assert!(reviewable
            .require_server_faa_observation_binding(
                exact_binding.listing_id + 1,
                &exact_binding.observation_sha256,
                exact_binding.listing_model_year,
                &exact_binding.grounding,
            )
            .is_err());
        assert!(reviewable
            .require_server_faa_observation_binding(
                exact_binding.listing_id,
                &exact_binding.observation_sha256,
                exact_binding.listing_model_year,
                &replayed_grounding,
            )
            .is_err());

        let mismatched = GroundingAudit {
            citation_urls: ["https://manufacturer.example/different".to_string()]
                .into_iter()
                .collect(),
            ..reused_grounding
        };
        let error = build_reviewable_aircraft_hierarchy(
            &research,
            &evidence_grounding,
            &server,
            &fetched_source_proofs(&research, &server),
            adjudication,
            &exact_empty_catalog(&server),
            1,
            verification,
            &mismatched,
            false,
        )
        .expect_err("a reused dossier with a different URL set must fail closed");
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "verifier_grounding_not_observed"));
    }
}
