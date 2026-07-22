//! Pure aircraft-catalog domain rules.
//!
//! This module intentionally has no database, HTTP, or LLM dependencies.  It
//! describes proposed hierarchy/reference records, validates the facts needed
//! to approve them, and resolves an immutable approved profile for a listing.
//! Text normalization in this module is for candidate retrieval only: a
//! normalized key is never evidence that two aircraft identities are equal.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

pub const MIN_AIRCRAFT_MODEL_YEAR: i32 = 1900;
pub const MAX_AIRCRAFT_MODEL_YEAR: i32 = 2200;

/// Normalize prose-like catalog text for candidate retrieval.
///
/// The result must not be stored as the canonical display value and must not
/// be used as an automatic merge key.  In particular, this function contains
/// no maker aliases and no aircraft-specific rewrites.
pub fn normalize_aircraft_retrieval_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            normalized.push(character);
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalize a designation for broad candidate retrieval while preserving all
/// letters and digits.  Separator variants converge, but material prefixes and
/// suffixes do not: `182T != T182T` and `SR22 != SR22T`.
pub fn normalize_aircraft_designator_retrieval_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Normalize a serial for exact/range applicability lookup only.
pub fn normalize_aircraft_serial_retrieval_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}

fn normalize_market_code(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
}

impl ValidationIssue {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ValidationErrors(pub Vec<ValidationIssue>);

impl ValidationErrors {
    pub fn from_unsorted(mut issues: Vec<ValidationIssue>) -> Self {
        issues.sort();
        issues.dedup();
        Self(issues)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = self
            .0
            .iter()
            .map(|issue| format!("{}: {}", issue.code, issue.message))
            .collect::<Vec<_>>()
            .join("; ");
        formatter.write_str(&message)
    }
}

impl Error for ValidationErrors {}

fn validation_result(issues: Vec<ValidationIssue>) -> Result<(), ValidationErrors> {
    if issues.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_unsorted(issues))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSourceKind {
    Manufacturer,
    Regulator,
    TypeCertificate,
    ApprovedFlightManual,
    ManufacturerServicePublication,
    RecognizedSecondary,
    MarketplaceListing,
}

impl EvidenceSourceKind {
    pub fn is_primary(self) -> bool {
        matches!(
            self,
            Self::Manufacturer
                | Self::Regulator
                | Self::TypeCertificate
                | Self::ApprovedFlightManual
                | Self::ManufacturerServicePublication
        )
    }

    /// Claim-specific authority. A regulator is controlling for registered
    /// identity and certification facts, while the manufacturer is controlling
    /// for commercial configuration and price facts the registry does not
    /// contain. This intentionally is not one global source score.
    pub fn authority_for(self, claim: EvidenceClaimKind) -> EvidenceAuthority {
        use EvidenceAuthority::{ContextOnly, Controlling, Primary, Secondary, Unsupported};
        use EvidenceClaimKind::{
            ComponentIdentity, FactoryConfiguration, HierarchyIdentity,
            InstalledMarketContribution, MarketApplicability, MaterialFeature,
            ProductionApplicability, ReferencePrice,
        };
        match (self, claim) {
            (Self::Regulator | Self::TypeCertificate, HierarchyIdentity)
            | (Self::Regulator | Self::TypeCertificate, ProductionApplicability) => Controlling,
            (Self::Manufacturer, FactoryConfiguration)
            | (Self::Manufacturer, ReferencePrice)
            | (Self::Manufacturer, MarketApplicability)
            | (Self::Manufacturer, ComponentIdentity)
            | (Self::Manufacturer, MaterialFeature)
            | (Self::ManufacturerServicePublication, ComponentIdentity)
            | (Self::ManufacturerServicePublication, FactoryConfiguration)
            | (Self::ManufacturerServicePublication, MaterialFeature) => Controlling,
            (Self::ApprovedFlightManual, HierarchyIdentity)
            | (Self::ApprovedFlightManual, ProductionApplicability)
            | (Self::ApprovedFlightManual, FactoryConfiguration)
            | (Self::ApprovedFlightManual, ComponentIdentity)
            | (Self::ApprovedFlightManual, MaterialFeature)
            | (Self::Manufacturer, HierarchyIdentity)
            | (Self::Manufacturer, ProductionApplicability)
            | (Self::ManufacturerServicePublication, HierarchyIdentity)
            | (Self::ManufacturerServicePublication, ProductionApplicability)
            | (Self::TypeCertificate, MarketApplicability)
            | (Self::Regulator | Self::TypeCertificate, ComponentIdentity)
            | (Self::Regulator | Self::TypeCertificate, MaterialFeature) => Primary,
            (Self::RecognizedSecondary, _) => Secondary,
            (Self::MarketplaceListing, _) => ContextOnly,
            (Self::Regulator | Self::TypeCertificate, FactoryConfiguration)
            | (Self::Regulator | Self::TypeCertificate, ReferencePrice)
            | (Self::ApprovedFlightManual, ReferencePrice)
            | (Self::ManufacturerServicePublication, ReferencePrice)
            | (Self::ApprovedFlightManual, MarketApplicability)
            | (Self::ManufacturerServicePublication, MarketApplicability)
            | (Self::Regulator, MarketApplicability)
            | (
                Self::Manufacturer
                | Self::Regulator
                | Self::TypeCertificate
                | Self::ApprovedFlightManual
                | Self::ManufacturerServicePublication,
                InstalledMarketContribution,
            ) => Unsupported,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAuthority {
    Unsupported,
    ContextOnly,
    Secondary,
    Primary,
    Controlling,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClaimKind {
    HierarchyIdentity,
    ProductionApplicability,
    MarketApplicability,
    FactoryConfiguration,
    ReferencePrice,
    ComponentIdentity,
    InstalledMarketContribution,
    MaterialFeature,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceClaimProposal {
    pub evidence_id: String,
    pub source_url: String,
    pub source_title: String,
    pub evidence_excerpt: String,
    pub source_kind: EvidenceSourceKind,
    pub supports: BTreeSet<EvidenceClaimKind>,
}

impl EvidenceClaimProposal {
    fn validate(&self, path: &str, issues: &mut Vec<ValidationIssue>) {
        let evidence_id = self.evidence_id.trim();
        if evidence_id.is_empty() {
            issues.push(ValidationIssue::new(
                "empty_evidence_id",
                format!("{path} has an empty evidence id"),
            ));
        } else if !evidence_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        {
            issues.push(ValidationIssue::new(
                "noncanonical_evidence_id",
                format!("{path} evidence id must use only ASCII letters, digits, '-', '_', or '.'"),
            ));
        }
        if !(self.source_url.starts_with("https://") || self.source_url.starts_with("http://")) {
            issues.push(ValidationIssue::new(
                "invalid_evidence_url",
                format!("{path} source URL is not http(s)"),
            ));
        }
        if self.source_title.trim().len() < 4 {
            issues.push(ValidationIssue::new(
                "weak_evidence_title",
                format!("{path} source title is too short"),
            ));
        }
        if self.evidence_excerpt.trim().len() < 12 {
            issues.push(ValidationIssue::new(
                "weak_evidence_excerpt",
                format!("{path} evidence excerpt is too short"),
            ));
        }
        if self.supports.is_empty() {
            issues.push(ValidationIssue::new(
                "unscoped_evidence",
                format!("{path} does not identify the claim it supports"),
            ));
        }
    }
}

/// Validate a reusable evidence claim independently of a hierarchy or profile.
pub fn validate_evidence_claim_proposal(
    proposal: &EvidenceClaimProposal,
) -> Result<(), ValidationErrors> {
    let mut issues = Vec::new();
    proposal.validate("evidence", &mut issues);
    validation_result(issues)
}

fn validate_evidence_claim_set(
    evidence: &[EvidenceClaimProposal],
    issues: &mut Vec<ValidationIssue>,
) {
    let mut evidence_ids = BTreeSet::new();
    for (index, claim) in evidence.iter().enumerate() {
        claim.validate(&format!("evidence[{index}]"), issues);
        let evidence_id = claim.evidence_id.trim();
        if !evidence_id.is_empty() && !evidence_ids.insert(evidence_id) {
            issues.push(ValidationIssue::new(
                "duplicate_evidence_id",
                format!("evidence id {evidence_id} appears more than once"),
            ));
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogEntityProposal {
    pub existing_catalog_id: Option<i64>,
    pub display_name: String,
    pub authoritative_designator: Option<String>,
}

impl CatalogEntityProposal {
    fn validate(&self, path: &str, issues: &mut Vec<ValidationIssue>) {
        if self.existing_catalog_id.is_some_and(|id| id <= 0) {
            issues.push(ValidationIssue::new(
                "invalid_catalog_id",
                format!("{path} catalog id must be positive"),
            ));
        }
        let name = self.display_name.trim();
        if name.is_empty() {
            issues.push(ValidationIssue::new(
                "empty_catalog_name",
                format!("{path} display name is empty"),
            ));
        } else if name.len() > 160 {
            issues.push(ValidationIssue::new(
                "catalog_name_too_long",
                format!("{path} display name exceeds 160 bytes"),
            ));
        }
        if self
            .authoritative_designator
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            issues.push(ValidationIssue::new(
                "empty_authoritative_designator",
                format!("{path} authoritative designator is empty"),
            ));
        }
        if contains_standalone_model_year(name) {
            issues.push(ValidationIssue::new(
                "year_in_hierarchy_label",
                format!("{path} display name contains a model year"),
            ));
        }
    }
}

fn contains_standalone_model_year(value: &str) -> bool {
    normalize_aircraft_retrieval_text(value)
        .split_whitespace()
        .any(|token| {
            token.len() == 4
                && token.parse::<i32>().is_ok_and(|year| {
                    (MIN_AIRCRAFT_MODEL_YEAR..=MAX_AIRCRAFT_MODEL_YEAR).contains(&year)
                })
        })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AircraftHierarchyProposal {
    pub manufacturer: CatalogEntityProposal,
    pub model_family: CatalogEntityProposal,
    pub certified_variant: CatalogEntityProposal,
    pub generation: Option<CatalogEntityProposal>,
    pub tier: Option<CatalogEntityProposal>,
    pub evidence: Vec<EvidenceClaimProposal>,
}

pub fn validate_aircraft_hierarchy_proposal(
    proposal: &AircraftHierarchyProposal,
) -> Result<(), ValidationErrors> {
    let mut issues = Vec::new();
    proposal.manufacturer.validate("manufacturer", &mut issues);
    proposal.model_family.validate("model_family", &mut issues);
    proposal
        .certified_variant
        .validate("certified_variant", &mut issues);
    if let Some(generation) = &proposal.generation {
        generation.validate("generation", &mut issues);
    }
    if let Some(tier) = &proposal.tier {
        tier.validate("tier", &mut issues);
    }
    validate_evidence_claim_set(&proposal.evidence, &mut issues);
    if !proposal.evidence.iter().any(|evidence| {
        evidence.source_kind.is_primary()
            && evidence
                .supports
                .contains(&EvidenceClaimKind::HierarchyIdentity)
    }) {
        issues.push(ValidationIssue::new(
            "missing_primary_hierarchy_evidence",
            "hierarchy approval requires primary identity evidence",
        ));
    }
    validation_result(issues)
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct AircraftHierarchy {
    pub manufacturer_id: i64,
    pub model_family_id: i64,
    pub certified_variant_id: i64,
    pub generation_id: Option<i64>,
    pub tier_id: Option<i64>,
}

impl AircraftHierarchy {
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut issues = Vec::new();
        for (name, id) in [
            ("manufacturer_id", self.manufacturer_id),
            ("model_family_id", self.model_family_id),
            ("certified_variant_id", self.certified_variant_id),
        ] {
            if id <= 0 {
                issues.push(ValidationIssue::new(
                    "invalid_hierarchy_id",
                    format!("{name} must be positive"),
                ));
            }
        }
        for (name, id) in [
            ("generation_id", self.generation_id),
            ("tier_id", self.tier_id),
        ] {
            if id.is_some_and(|id| id <= 0) {
                issues.push(ValidationIssue::new(
                    "invalid_hierarchy_id",
                    format!("{name} must be positive when present"),
                ));
            }
        }
        validation_result(issues)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialConstraint {
    Exact(String),
    NumericRange {
        prefix: String,
        first: u64,
        last: u64,
    },
}

impl SerialConstraint {
    pub fn exact(value: impl Into<String>) -> Self {
        Self::Exact(normalize_aircraft_serial_retrieval_key(&value.into()))
    }

    pub fn numeric_range(prefix: impl Into<String>, first: u64, last: u64) -> Self {
        Self::NumericRange {
            prefix: normalize_aircraft_serial_retrieval_key(&prefix.into()),
            first,
            last,
        }
    }

    fn validate(&self, path: &str, issues: &mut Vec<ValidationIssue>) {
        match self {
            Self::Exact(value) if value.trim().is_empty() => issues.push(ValidationIssue::new(
                "empty_serial_constraint",
                format!("{path} exact serial is empty"),
            )),
            Self::NumericRange { first, last, .. } if first > last => {
                issues.push(ValidationIssue::new(
                    "reversed_serial_range",
                    format!("{path} starts after it ends"),
                ));
            }
            _ => {}
        }
    }

    pub fn matches(&self, serial_number: &str) -> bool {
        let serial = normalize_aircraft_serial_retrieval_key(serial_number);
        match self {
            Self::Exact(expected) => serial == *expected,
            Self::NumericRange {
                prefix,
                first,
                last,
            } => parse_prefixed_serial_number(&serial, prefix)
                .is_some_and(|number| (*first..=*last).contains(&number)),
        }
    }

    fn overlaps(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Exact(left), Self::Exact(right)) => left == right,
            (
                Self::NumericRange {
                    prefix: left_prefix,
                    first: left_first,
                    last: left_last,
                },
                Self::NumericRange {
                    prefix: right_prefix,
                    first: right_first,
                    last: right_last,
                },
            ) => {
                left_prefix == right_prefix && left_first <= right_last && right_first <= left_last
            }
            (
                Self::Exact(exact),
                Self::NumericRange {
                    prefix,
                    first,
                    last,
                },
            )
            | (
                Self::NumericRange {
                    prefix,
                    first,
                    last,
                },
                Self::Exact(exact),
            ) => parse_prefixed_serial_number(exact, prefix)
                .is_some_and(|number| (*first..=*last).contains(&number)),
        }
    }
}

fn parse_prefixed_serial_number(serial: &str, expected_prefix: &str) -> Option<u64> {
    let digits = serial.strip_prefix(expected_prefix)?;
    if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AircraftApplicability {
    pub first_model_year: i32,
    pub last_model_year: i32,
    /// Empty means every serial in the model-year range.  Otherwise entries are
    /// alternatives and at least one must match.
    pub serial_constraints: Vec<SerialConstraint>,
    /// Empty means every market.  Values are normalized market identifiers such
    /// as `US`, `EU`, or a documented manufacturer market code.
    pub markets: BTreeSet<String>,
}

impl AircraftApplicability {
    pub fn exact_model_year(model_year: i32) -> Self {
        Self {
            first_model_year: model_year,
            last_model_year: model_year,
            serial_constraints: Vec::new(),
            markets: BTreeSet::new(),
        }
    }

    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut issues = Vec::new();
        if !(MIN_AIRCRAFT_MODEL_YEAR..=MAX_AIRCRAFT_MODEL_YEAR).contains(&self.first_model_year)
            || !(MIN_AIRCRAFT_MODEL_YEAR..=MAX_AIRCRAFT_MODEL_YEAR).contains(&self.last_model_year)
        {
            issues.push(ValidationIssue::new(
                "invalid_model_year_range",
                format!(
                    "model years must be in {MIN_AIRCRAFT_MODEL_YEAR}..={MAX_AIRCRAFT_MODEL_YEAR}"
                ),
            ));
        }
        if self.first_model_year > self.last_model_year {
            issues.push(ValidationIssue::new(
                "reversed_model_year_range",
                "first model year is after last model year",
            ));
        }
        for (index, constraint) in self.serial_constraints.iter().enumerate() {
            constraint.validate(&format!("serial_constraints[{index}]"), &mut issues);
        }
        let unique_serials = self
            .serial_constraints
            .iter()
            .collect::<BTreeSet<_>>()
            .len();
        if unique_serials != self.serial_constraints.len() {
            issues.push(ValidationIssue::new(
                "duplicate_serial_constraint",
                "serial constraints contain duplicates",
            ));
        }
        if self
            .markets
            .iter()
            .any(|market| market.trim().is_empty() || normalize_market_code(market) != *market)
        {
            issues.push(ValidationIssue::new(
                "noncanonical_market_code",
                "market codes must be non-empty uppercase alphanumeric retrieval keys",
            ));
        }
        validation_result(issues)
    }

    fn contains_year(&self, model_year: i32) -> bool {
        (self.first_model_year..=self.last_model_year).contains(&model_year)
    }

    fn markets_overlap(&self, other: &Self) -> bool {
        self.markets.is_empty()
            || other.markets.is_empty()
            || !self.markets.is_disjoint(&other.markets)
    }

    fn serials_overlap(&self, other: &Self) -> bool {
        self.serial_constraints.is_empty()
            || other.serial_constraints.is_empty()
            || self.serial_constraints.iter().any(|left| {
                other
                    .serial_constraints
                    .iter()
                    .any(|right| left.overlaps(right))
            })
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.first_model_year <= other.last_model_year
            && other.first_model_year <= self.last_model_year
            && self.markets_overlap(other)
            && self.serials_overlap(other)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceProfileProposal {
    pub profile_version_id: i64,
    pub catalog_revision: u64,
    pub supersedes_profile_version_id: Option<i64>,
    pub hierarchy: AircraftHierarchy,
    pub applicability: AircraftApplicability,
    pub evidence: Vec<EvidenceClaimProposal>,
}

pub fn validate_reference_profile_proposal(
    proposal: &ReferenceProfileProposal,
) -> Result<(), ValidationErrors> {
    let mut issues = Vec::new();
    if proposal.profile_version_id <= 0 {
        issues.push(ValidationIssue::new(
            "invalid_profile_version_id",
            "profile version id must be positive",
        ));
    }
    if proposal.catalog_revision == 0 {
        issues.push(ValidationIssue::new(
            "invalid_catalog_revision",
            "catalog revision must be positive",
        ));
    }
    if proposal
        .supersedes_profile_version_id
        .is_some_and(|id| id <= 0 || id == proposal.profile_version_id)
    {
        issues.push(ValidationIssue::new(
            "invalid_superseded_profile",
            "superseded profile must be a different positive id",
        ));
    }
    if let Err(errors) = proposal.hierarchy.validate() {
        issues.extend(errors.0);
    }
    if let Err(errors) = proposal.applicability.validate() {
        issues.extend(errors.0);
    }
    validate_evidence_claim_set(&proposal.evidence, &mut issues);
    for required in [
        EvidenceClaimKind::HierarchyIdentity,
        EvidenceClaimKind::ProductionApplicability,
        EvidenceClaimKind::FactoryConfiguration,
    ] {
        if !proposal.evidence.iter().any(|evidence| {
            evidence.source_kind.is_primary() && evidence.supports.contains(&required)
        }) {
            issues.push(ValidationIssue::new(
                "missing_primary_profile_evidence",
                format!("approved profile lacks primary evidence for {required:?}"),
            ));
        }
    }
    if !proposal.applicability.markets.is_empty()
        && !proposal.evidence.iter().any(|evidence| {
            evidence.source_kind.is_primary()
                && evidence
                    .supports
                    .contains(&EvidenceClaimKind::MarketApplicability)
        })
    {
        issues.push(ValidationIssue::new(
            "missing_primary_market_evidence",
            "market-restricted profile lacks primary market-applicability evidence",
        ));
    }
    validation_result(issues)
}

/// An approved profile has no mutation API.  Corrections are represented by a
/// newly approved version whose proposal names this version in `supersedes`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApprovedReferenceProfile {
    profile_version_id: i64,
    catalog_revision: u64,
    supersedes_profile_version_id: Option<i64>,
    hierarchy: AircraftHierarchy,
    applicability: AircraftApplicability,
    evidence_ids: Vec<String>,
    canonical_profile_key: String,
}

impl ApprovedReferenceProfile {
    pub fn approve(proposal: ReferenceProfileProposal) -> Result<Self, ValidationErrors> {
        validate_reference_profile_proposal(&proposal)?;
        let mut evidence_ids = proposal
            .evidence
            .iter()
            .map(|evidence| evidence.evidence_id.trim().to_string())
            .collect::<Vec<_>>();
        evidence_ids.sort();
        evidence_ids.dedup();
        let canonical_profile_key = canonical_profile_key(
            proposal.profile_version_id,
            proposal.catalog_revision,
            &proposal.hierarchy,
            &proposal.applicability,
            &evidence_ids,
        );
        Ok(Self {
            profile_version_id: proposal.profile_version_id,
            catalog_revision: proposal.catalog_revision,
            supersedes_profile_version_id: proposal.supersedes_profile_version_id,
            hierarchy: proposal.hierarchy,
            applicability: proposal.applicability,
            evidence_ids,
            canonical_profile_key,
        })
    }

    pub fn profile_version_id(&self) -> i64 {
        self.profile_version_id
    }

    pub fn catalog_revision(&self) -> u64 {
        self.catalog_revision
    }

    pub fn supersedes_profile_version_id(&self) -> Option<i64> {
        self.supersedes_profile_version_id
    }

    pub fn hierarchy(&self) -> &AircraftHierarchy {
        &self.hierarchy
    }

    pub fn applicability(&self) -> &AircraftApplicability {
        &self.applicability
    }

    pub fn evidence_ids(&self) -> &[String] {
        &self.evidence_ids
    }

    pub fn canonical_profile_key(&self) -> &str {
        &self.canonical_profile_key
    }
}

fn canonical_profile_key(
    profile_version_id: i64,
    catalog_revision: u64,
    hierarchy: &AircraftHierarchy,
    applicability: &AircraftApplicability,
    evidence_ids: &[String],
) -> String {
    let serials = applicability
        .serial_constraints
        .iter()
        .map(|constraint| match constraint {
            SerialConstraint::Exact(serial) => format!("exact:{serial}"),
            SerialConstraint::NumericRange {
                prefix,
                first,
                last,
            } => format!("range:{prefix}:{first}:{last}"),
        })
        .collect::<Vec<_>>()
        .join(",");
    let markets = applicability
        .markets
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    let evidence = evidence_ids.join(",");
    format!(
        "profile={profile_version_id}|revision={catalog_revision}|maker={}|family={}|variant={}|generation={}|tier={}|years={}-{}|serials={serials}|markets={markets}|evidence={evidence}",
        hierarchy.manufacturer_id,
        hierarchy.model_family_id,
        hierarchy.certified_variant_id,
        hierarchy.generation_id.unwrap_or_default(),
        hierarchy.tier_id.unwrap_or_default(),
        applicability.first_model_year,
        applicability.last_model_year,
    )
}

pub fn validate_approved_profile_successor(
    current: &ApprovedReferenceProfile,
    successor: &ApprovedReferenceProfile,
) -> Result<(), ValidationErrors> {
    let mut issues = Vec::new();
    if successor.supersedes_profile_version_id() != Some(current.profile_version_id()) {
        issues.push(ValidationIssue::new(
            "successor_does_not_supersede_current",
            "successor must name the current profile version",
        ));
    }
    if successor.profile_version_id() == current.profile_version_id() {
        issues.push(ValidationIssue::new(
            "successor_reuses_version_id",
            "successor must have a new immutable version id",
        ));
    }
    if successor.catalog_revision() <= current.catalog_revision() {
        issues.push(ValidationIssue::new(
            "successor_revision_not_newer",
            "successor catalog revision must be newer",
        ));
    }
    validation_result(issues)
}

pub fn validate_approved_reference_profile_set(
    profiles: &[ApprovedReferenceProfile],
) -> Result<(), ValidationErrors> {
    let mut issues = Vec::new();
    let mut ids = BTreeSet::new();
    for profile in profiles {
        if !ids.insert(profile.profile_version_id()) {
            issues.push(ValidationIssue::new(
                "duplicate_profile_version_id",
                format!(
                    "profile version {} appears twice",
                    profile.profile_version_id()
                ),
            ));
        }
    }
    for left_index in 0..profiles.len() {
        for right in &profiles[left_index + 1..] {
            let left = &profiles[left_index];
            if left.hierarchy() == right.hierarchy()
                && left.applicability().overlaps(right.applicability())
            {
                issues.push(ValidationIssue::new(
                    "overlapping_approved_profiles",
                    format!(
                        "profiles {} and {} overlap for the same hierarchy",
                        left.profile_version_id(),
                        right.profile_version_id()
                    ),
                ));
            }
        }
    }
    validation_result(issues)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionalHierarchySelection {
    CatalogEntry(i64),
    ExplicitNone,
    Unspecified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceProfileQuery {
    pub manufacturer_id: i64,
    pub model_family_id: i64,
    pub certified_variant_id: i64,
    pub generation: OptionalHierarchySelection,
    pub tier: OptionalHierarchySelection,
    pub model_year: i32,
    pub serial_number: Option<String>,
    pub market: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionAmbiguity {
    GenerationUnspecified,
    TierUnspecified,
    SerialRequired,
    MarketRequired,
    MultipleApplicableProfiles,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceProfileResolution {
    Exact {
        profile_version_id: i64,
        used_serial: bool,
        used_market: bool,
    },
    Ambiguous {
        candidate_profile_version_ids: Vec<i64>,
        reasons: BTreeSet<ResolutionAmbiguity>,
    },
    NotFound,
}

pub fn resolve_reference_profile(
    profiles: &[ApprovedReferenceProfile],
    query: &ReferenceProfileQuery,
) -> ReferenceProfileResolution {
    let query_market = query.market.as_deref().map(normalize_market_code);
    let query_serial = query
        .serial_number
        .as_deref()
        .map(normalize_aircraft_serial_retrieval_key);
    let mut candidates = profiles
        .iter()
        .filter(|profile| {
            let hierarchy = profile.hierarchy();
            hierarchy.manufacturer_id == query.manufacturer_id
                && hierarchy.model_family_id == query.model_family_id
                && hierarchy.certified_variant_id == query.certified_variant_id
                && optional_dimension_matches(query.generation, hierarchy.generation_id)
                && optional_dimension_matches(query.tier, hierarchy.tier_id)
                && profile.applicability().contains_year(query.model_year)
        })
        .filter(|profile| {
            let applicability = profile.applicability();
            match &query_market {
                Some(market) => {
                    applicability.markets.is_empty() || applicability.markets.contains(market)
                }
                None => true,
            }
        })
        .filter(|profile| {
            let applicability = profile.applicability();
            match &query_serial {
                Some(serial) => {
                    applicability.serial_constraints.is_empty()
                        || applicability
                            .serial_constraints
                            .iter()
                            .any(|constraint| constraint.matches(serial))
                }
                None => true,
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|profile| profile.profile_version_id());

    if candidates.is_empty() {
        return ReferenceProfileResolution::NotFound;
    }

    let mut reasons = BTreeSet::new();
    // Unknown is not an alias for `ExplicitNone`, even when this catalog
    // snapshot happens to contain only one otherwise-applicable profile.
    if query.generation == OptionalHierarchySelection::Unspecified {
        reasons.insert(ResolutionAmbiguity::GenerationUnspecified);
    }
    if query.tier == OptionalHierarchySelection::Unspecified {
        reasons.insert(ResolutionAmbiguity::TierUnspecified);
    }
    if query_serial.is_none()
        && candidates
            .iter()
            .any(|profile| !profile.applicability().serial_constraints.is_empty())
    {
        reasons.insert(ResolutionAmbiguity::SerialRequired);
    }
    if query_market.is_none()
        && candidates
            .iter()
            .any(|profile| !profile.applicability().markets.is_empty())
    {
        reasons.insert(ResolutionAmbiguity::MarketRequired);
    }
    if candidates.len() > 1 {
        reasons.insert(ResolutionAmbiguity::MultipleApplicableProfiles);
    }

    if !reasons.is_empty() {
        return ReferenceProfileResolution::Ambiguous {
            candidate_profile_version_ids: candidates
                .iter()
                .map(|profile| profile.profile_version_id())
                .collect(),
            reasons,
        };
    }

    let profile = candidates[0];
    ReferenceProfileResolution::Exact {
        profile_version_id: profile.profile_version_id(),
        used_serial: query_serial.is_some()
            && !profile.applicability().serial_constraints.is_empty(),
        used_market: query_market.is_some() && !profile.applicability().markets.is_empty(),
    }
}

fn optional_dimension_matches(
    selection: OptionalHierarchySelection,
    profile_value: Option<i64>,
) -> bool {
    match selection {
        OptionalHierarchySelection::CatalogEntry(id) => profile_value == Some(id),
        OptionalHierarchySelection::ExplicitNone => profile_value.is_none(),
        OptionalHierarchySelection::Unspecified => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_authority_is_claim_specific() {
        assert_eq!(
            EvidenceSourceKind::Regulator.authority_for(EvidenceClaimKind::HierarchyIdentity),
            EvidenceAuthority::Controlling
        );
        assert_eq!(
            EvidenceSourceKind::Manufacturer.authority_for(EvidenceClaimKind::FactoryConfiguration),
            EvidenceAuthority::Controlling
        );
        assert_eq!(
            EvidenceSourceKind::Regulator.authority_for(EvidenceClaimKind::ReferencePrice),
            EvidenceAuthority::Unsupported
        );
        assert_eq!(
            EvidenceSourceKind::Regulator.authority_for(EvidenceClaimKind::MarketApplicability),
            EvidenceAuthority::Unsupported
        );
        assert_eq!(
            EvidenceSourceKind::Manufacturer.authority_for(EvidenceClaimKind::MarketApplicability),
            EvidenceAuthority::Controlling
        );
        assert_eq!(
            EvidenceSourceKind::MarketplaceListing
                .authority_for(EvidenceClaimKind::HierarchyIdentity),
            EvidenceAuthority::ContextOnly
        );
    }

    fn evidence(id: &str, supports: &[EvidenceClaimKind]) -> EvidenceClaimProposal {
        EvidenceClaimProposal {
            evidence_id: id.to_string(),
            source_url: format!("https://manufacturer.example/{id}"),
            source_title: "Official aircraft reference".to_string(),
            evidence_excerpt: "Official designation and production applicability evidence."
                .to_string(),
            source_kind: EvidenceSourceKind::Manufacturer,
            supports: supports.iter().copied().collect(),
        }
    }

    fn profile(
        id: i64,
        variant: i64,
        generation: Option<i64>,
        tier: Option<i64>,
        applicability: AircraftApplicability,
    ) -> ApprovedReferenceProfile {
        ApprovedReferenceProfile::approve(ReferenceProfileProposal {
            profile_version_id: id,
            catalog_revision: id as u64,
            supersedes_profile_version_id: None,
            hierarchy: AircraftHierarchy {
                manufacturer_id: 1,
                model_family_id: 10,
                certified_variant_id: variant,
                generation_id: generation,
                tier_id: tier,
            },
            applicability,
            evidence: vec![evidence(
                &format!("profile-{id}"),
                &[
                    EvidenceClaimKind::HierarchyIdentity,
                    EvidenceClaimKind::ProductionApplicability,
                    EvidenceClaimKind::FactoryConfiguration,
                    EvidenceClaimKind::MarketApplicability,
                ],
            )],
        })
        .unwrap()
    }

    fn query(variant: i64, year: i32) -> ReferenceProfileQuery {
        ReferenceProfileQuery {
            manufacturer_id: 1,
            model_family_id: 10,
            certified_variant_id: variant,
            generation: OptionalHierarchySelection::ExplicitNone,
            tier: OptionalHierarchySelection::ExplicitNone,
            model_year: year,
            serial_number: None,
            market: None,
        }
    }

    #[test]
    fn retrieval_normalization_never_collapses_material_designators() {
        assert_eq!(normalize_aircraft_designator_retrieval_key("182-T"), "182t");
        assert_ne!(
            normalize_aircraft_designator_retrieval_key("182T"),
            normalize_aircraft_designator_retrieval_key("T182T")
        );
        assert_ne!(
            normalize_aircraft_designator_retrieval_key("SR22"),
            normalize_aircraft_designator_retrieval_key("SR22T")
        );
        assert_ne!(
            normalize_aircraft_designator_retrieval_key("G6"),
            normalize_aircraft_designator_retrieval_key("GTS")
        );
    }

    #[test]
    fn cessna_182t_and_t182t_are_exact_certified_variants() {
        const CESSNA_182T_VARIANT_ID: i64 = 182;
        const CESSNA_T182T_VARIANT_ID: i64 = 18_218;
        let profiles = vec![
            profile(
                1,
                CESSNA_182T_VARIANT_ID,
                None,
                None,
                AircraftApplicability::exact_model_year(2020),
            ),
            profile(
                2,
                CESSNA_T182T_VARIANT_ID,
                None,
                None,
                AircraftApplicability::exact_model_year(2020),
            ),
        ];
        assert_eq!(
            resolve_reference_profile(&profiles, &query(CESSNA_182T_VARIANT_ID, 2020)),
            ReferenceProfileResolution::Exact {
                profile_version_id: 1,
                used_serial: false,
                used_market: false,
            }
        );
        assert_eq!(
            resolve_reference_profile(&profiles, &query(CESSNA_T182T_VARIANT_ID, 2020)),
            ReferenceProfileResolution::Exact {
                profile_version_id: 2,
                used_serial: false,
                used_market: false,
            }
        );
    }

    #[test]
    fn sr22_and_sr22t_are_distinct_certified_variants() {
        let profiles = vec![
            profile(
                1,
                22,
                Some(6),
                None,
                AircraftApplicability::exact_model_year(2020),
            ),
            profile(
                2,
                220,
                Some(6),
                None,
                AircraftApplicability::exact_model_year(2020),
            ),
        ];
        let mut sr22 = query(22, 2020);
        sr22.generation = OptionalHierarchySelection::CatalogEntry(6);
        let mut sr22t = sr22.clone();
        sr22t.certified_variant_id = 220;
        assert!(matches!(
            resolve_reference_profile(&profiles, &sr22),
            ReferenceProfileResolution::Exact {
                profile_version_id: 1,
                ..
            }
        ));
        assert!(matches!(
            resolve_reference_profile(&profiles, &sr22t),
            ReferenceProfileResolution::Exact {
                profile_version_id: 2,
                ..
            }
        ));
    }

    #[test]
    fn g6_base_and_gts_require_an_explicit_tier() {
        let profiles = vec![
            profile(
                1,
                22,
                Some(6),
                None,
                AircraftApplicability::exact_model_year(2020),
            ),
            profile(
                2,
                22,
                Some(6),
                Some(100),
                AircraftApplicability::exact_model_year(2020),
            ),
        ];
        let mut unspecified = query(22, 2020);
        unspecified.generation = OptionalHierarchySelection::CatalogEntry(6);
        unspecified.tier = OptionalHierarchySelection::Unspecified;
        let resolution = resolve_reference_profile(&profiles, &unspecified);
        let ReferenceProfileResolution::Ambiguous { reasons, .. } = resolution else {
            panic!("tierless G6 query should be ambiguous");
        };
        assert!(reasons.contains(&ResolutionAmbiguity::TierUnspecified));

        let mut base = unspecified.clone();
        base.tier = OptionalHierarchySelection::ExplicitNone;
        assert!(matches!(
            resolve_reference_profile(&profiles, &base),
            ReferenceProfileResolution::Exact {
                profile_version_id: 1,
                ..
            }
        ));
        let mut gts = unspecified;
        gts.tier = OptionalHierarchySelection::CatalogEntry(100);
        assert!(matches!(
            resolve_reference_profile(&profiles, &gts),
            ReferenceProfileResolution::Exact {
                profile_version_id: 2,
                ..
            }
        ));
    }

    #[test]
    fn a_single_candidate_does_not_turn_unknown_generation_or_tier_into_none() {
        let profiles = vec![profile(
            1,
            22,
            Some(6),
            Some(100),
            AircraftApplicability::exact_model_year(2020),
        )];
        let mut unknown = query(22, 2020);
        unknown.generation = OptionalHierarchySelection::Unspecified;
        unknown.tier = OptionalHierarchySelection::Unspecified;

        let ReferenceProfileResolution::Ambiguous { reasons, .. } =
            resolve_reference_profile(&profiles, &unknown)
        else {
            panic!("unknown hierarchy dimensions must fail closed");
        };
        assert!(reasons.contains(&ResolutionAmbiguity::GenerationUnspecified));
        assert!(reasons.contains(&ResolutionAmbiguity::TierUnspecified));
    }

    #[test]
    fn year_serial_and_market_constraints_fail_closed_when_unknown() {
        let mut applicability = AircraftApplicability::exact_model_year(2021);
        applicability.serial_constraints = vec![SerialConstraint::numeric_range("SR", 100, 199)];
        applicability.markets = BTreeSet::from(["US".to_string()]);
        let profiles = vec![profile(1, 22, Some(6), Some(100), applicability)];
        let query = ReferenceProfileQuery {
            manufacturer_id: 1,
            model_family_id: 10,
            certified_variant_id: 22,
            generation: OptionalHierarchySelection::CatalogEntry(6),
            tier: OptionalHierarchySelection::CatalogEntry(100),
            model_year: 2021,
            serial_number: None,
            market: None,
        };
        let ReferenceProfileResolution::Ambiguous { reasons, .. } =
            resolve_reference_profile(&profiles, &query)
        else {
            panic!("missing serial and market should be ambiguous");
        };
        assert!(reasons.contains(&ResolutionAmbiguity::SerialRequired));
        assert!(reasons.contains(&ResolutionAmbiguity::MarketRequired));

        let exact = ReferenceProfileQuery {
            serial_number: Some("SR-150".to_string()),
            market: Some("us".to_string()),
            ..query
        };
        assert_eq!(
            resolve_reference_profile(&profiles, &exact),
            ReferenceProfileResolution::Exact {
                profile_version_id: 1,
                used_serial: true,
                used_market: true,
            }
        );
        let wrong_year = ReferenceProfileQuery {
            model_year: 2020,
            ..exact
        };
        assert_eq!(
            resolve_reference_profile(&profiles, &wrong_year),
            ReferenceProfileResolution::NotFound
        );
    }

    #[test]
    fn overlapping_approved_profiles_are_rejected() {
        let left = profile(
            1,
            22,
            Some(6),
            Some(100),
            AircraftApplicability {
                first_model_year: 2020,
                last_model_year: 2021,
                serial_constraints: vec![],
                markets: BTreeSet::new(),
            },
        );
        let right = profile(
            2,
            22,
            Some(6),
            Some(100),
            AircraftApplicability::exact_model_year(2021),
        );
        let error = validate_approved_reference_profile_set(&[left, right]).unwrap_err();
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "overlapping_approved_profiles"));
    }

    #[test]
    fn approved_profiles_are_replaced_only_by_immutable_successors() {
        let current = profile(
            1,
            22,
            Some(6),
            None,
            AircraftApplicability::exact_model_year(2020),
        );
        let mut proposal = ReferenceProfileProposal {
            profile_version_id: 2,
            catalog_revision: 2,
            supersedes_profile_version_id: Some(1),
            hierarchy: current.hierarchy().clone(),
            applicability: current.applicability().clone(),
            evidence: vec![evidence(
                "successor",
                &[
                    EvidenceClaimKind::HierarchyIdentity,
                    EvidenceClaimKind::ProductionApplicability,
                    EvidenceClaimKind::FactoryConfiguration,
                ],
            )],
        };
        let successor = ApprovedReferenceProfile::approve(proposal.clone()).unwrap();
        validate_approved_profile_successor(&current, &successor).unwrap();

        proposal.supersedes_profile_version_id = None;
        let unrelated = ApprovedReferenceProfile::approve(proposal).unwrap();
        assert!(validate_approved_profile_successor(&current, &unrelated).is_err());
    }

    #[test]
    fn hierarchy_proposals_reject_year_labels_and_nonprimary_identity_evidence() {
        let entity = |display_name: &str| CatalogEntityProposal {
            existing_catalog_id: None,
            display_name: display_name.to_string(),
            authoritative_designator: None,
        };
        let proposal = AircraftHierarchyProposal {
            manufacturer: entity("Cessna"),
            model_family: entity("182"),
            certified_variant: entity("2020 182T"),
            generation: None,
            tier: None,
            evidence: vec![EvidenceClaimProposal {
                source_kind: EvidenceSourceKind::MarketplaceListing,
                ..evidence("listing-identity", &[EvidenceClaimKind::HierarchyIdentity])
            }],
        };

        let first = validate_aircraft_hierarchy_proposal(&proposal).unwrap_err();
        let second = validate_aircraft_hierarchy_proposal(&proposal).unwrap_err();
        assert_eq!(first, second);
        assert!(first
            .0
            .iter()
            .any(|issue| issue.code == "year_in_hierarchy_label"));
        assert!(first
            .0
            .iter()
            .any(|issue| issue.code == "missing_primary_hierarchy_evidence"));
        assert!(first.0.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn evidence_ids_are_safe_for_deterministic_reference_keys() {
        let mut claim = evidence("valid-id.1", &[EvidenceClaimKind::HierarchyIdentity]);
        validate_evidence_claim_proposal(&claim).unwrap();
        claim.evidence_id = "unsafe,id".to_string();
        let error = validate_evidence_claim_proposal(&claim).unwrap_err();
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "noncanonical_evidence_id"));
    }

    #[test]
    fn profile_proposals_reject_duplicate_evidence_ids() {
        let duplicate = evidence(
            "same-source",
            &[
                EvidenceClaimKind::HierarchyIdentity,
                EvidenceClaimKind::ProductionApplicability,
                EvidenceClaimKind::FactoryConfiguration,
            ],
        );
        let proposal = ReferenceProfileProposal {
            profile_version_id: 1,
            catalog_revision: 1,
            supersedes_profile_version_id: None,
            hierarchy: AircraftHierarchy {
                manufacturer_id: 1,
                model_family_id: 10,
                certified_variant_id: 22,
                generation_id: Some(6),
                tier_id: None,
            },
            applicability: AircraftApplicability::exact_model_year(2020),
            evidence: vec![duplicate.clone(), duplicate],
        };
        let error = validate_reference_profile_proposal(&proposal).unwrap_err();
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "duplicate_evidence_id"));
    }
}
