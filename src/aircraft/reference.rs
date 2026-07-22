//! Pure reference-configuration and listing-delta domain rules.
//!
//! The module deliberately separates a full factory reference price from a
//! listing's configuration delta.  Listing-only estimators receive features;
//! the reference estimator receives the full standard-configuration price and
//! only the delta away from that standard.  No API here adds standard avionics
//! to a price which already includes them.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::aircraft::catalog::{
    validate_evidence_claim_proposal, ApprovedReferenceProfile, EvidenceClaimKind,
    EvidenceClaimProposal, EvidenceSourceKind, ValidationErrors, ValidationIssue,
    MAX_AIRCRAFT_MODEL_YEAR, MIN_AIRCRAFT_MODEL_YEAR,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AircraftComponentKind {
    Avionics,
    Engine,
    Propeller,
    AirframeFeature,
}

impl AircraftComponentKind {
    fn token(self) -> &'static str {
        match self {
            Self::Avionics => "avionics",
            Self::Engine => "engine",
            Self::Propeller => "propeller",
            Self::AirframeFeature => "airframe_feature",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CatalogComponentKey {
    pub kind: AircraftComponentKind,
    pub catalog_id: i64,
}

impl CatalogComponentKey {
    pub fn new(kind: AircraftComponentKind, catalog_id: i64) -> Self {
        Self { kind, catalog_id }
    }

    fn validate(self, path: &str, issues: &mut Vec<ValidationIssue>) {
        if self.catalog_id <= 0 {
            issues.push(ValidationIssue::new(
                "invalid_component_catalog_id",
                format!("{path} catalog id must be positive"),
            ));
        }
    }

    fn feature_token(self) -> String {
        format!("{}:{}", self.kind.token(), self.catalog_id)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryInclusion {
    Standard,
    Mandatory,
    IncludedInTier,
    Optional,
}

impl FactoryInclusion {
    pub fn is_standard_configuration(self) -> bool {
        !matches!(self, Self::Optional)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceComponentProposal {
    pub component: CatalogComponentKey,
    pub quantity: u32,
    pub inclusion: FactoryInclusion,
    pub evidence_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceFeatureProposal {
    pub feature_catalog_id: i64,
    pub value: String,
    pub unit: Option<String>,
    pub evidence_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SuiteMembershipProposal {
    pub suite: CatalogComponentKey,
    pub component: CatalogComponentKey,
    pub quantity: u32,
    pub evidence_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferencePriceBasis {
    /// The published amount includes the complete standard configuration in
    /// this version.  This is the only basis accepted for reference valuation.
    FullStandardConfiguration,
    BaseAircraftOnly,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NominalUsd {
    pub amount_cents: i64,
    pub reference_year: i32,
}

impl NominalUsd {
    pub fn checked_difference(self, other: Self) -> Option<Self> {
        if self.reference_year != other.reference_year {
            return None;
        }
        Some(Self {
            amount_cents: self.amount_cents.checked_sub(other.amount_cents)?,
            reference_year: self.reference_year,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferencePriceProposal {
    /// Aircraft model year whose full standard configuration is priced.
    pub model_year: i32,
    /// Nominal amount and the actual dollar year of the cited price.  The
    /// dollar year may differ from model year (for example, an MY2020 price
    /// list that became effective in 2019).
    pub nominal_usd: NominalUsd,
    pub basis: ReferencePriceBasis,
    pub evidence_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ComponentValueProposal {
    pub component: CatalogComponentKey,
    /// Conservative installed resale contribution, not retail replacement
    /// cost.  Values with different reference years are never added here.
    pub installed_contribution: NominalUsd,
    pub evidence_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceConfigurationProposal {
    pub configuration_version_id: i64,
    pub reference_catalog_snapshot_id: u64,
    pub supersedes_configuration_version_id: Option<i64>,
    pub profile_version_id: i64,
    pub components: Vec<ReferenceComponentProposal>,
    pub features: Vec<ReferenceFeatureProposal>,
    pub suite_memberships: Vec<SuiteMembershipProposal>,
    pub reference_prices: Vec<ReferencePriceProposal>,
    pub component_values: Vec<ComponentValueProposal>,
    pub evidence: Vec<EvidenceClaimProposal>,
}

pub fn validate_reference_configuration_proposal(
    proposal: &ReferenceConfigurationProposal,
) -> Result<(), ValidationErrors> {
    let mut issues = Vec::new();
    if proposal.configuration_version_id <= 0 {
        issues.push(ValidationIssue::new(
            "invalid_configuration_version_id",
            "configuration version id must be positive",
        ));
    }
    if proposal.reference_catalog_snapshot_id == 0 {
        issues.push(ValidationIssue::new(
            "invalid_reference_catalog_snapshot_id",
            "reference catalog snapshot id must be positive",
        ));
    }
    if proposal
        .supersedes_configuration_version_id
        .is_some_and(|id| id <= 0 || id == proposal.configuration_version_id)
    {
        issues.push(ValidationIssue::new(
            "invalid_superseded_configuration",
            "superseded configuration must be a different positive id",
        ));
    }

    let evidence_by_id = validate_evidence(&proposal.evidence, &mut issues);
    for required in [
        EvidenceClaimKind::FactoryConfiguration,
        EvidenceClaimKind::ProductionApplicability,
    ] {
        if !proposal.evidence.iter().any(|evidence| {
            evidence.source_kind.is_primary() && evidence.supports.contains(&required)
        }) {
            issues.push(ValidationIssue::new(
                "missing_primary_configuration_evidence",
                format!("configuration lacks primary evidence for {required:?}"),
            ));
        }
    }

    let mut component_keys = BTreeSet::new();
    for (index, component) in proposal.components.iter().enumerate() {
        component
            .component
            .validate(&format!("components[{index}]"), &mut issues);
        if component.quantity == 0 {
            issues.push(ValidationIssue::new(
                "zero_reference_component_quantity",
                format!("components[{index}] quantity must be positive"),
            ));
        }
        if !component_keys.insert(component.component) {
            issues.push(ValidationIssue::new(
                "duplicate_reference_component",
                format!(
                    "component {}:{} appears more than once",
                    component.component.kind.token(),
                    component.component.catalog_id
                ),
            ));
        }
        require_evidence(
            &component.evidence_id,
            EvidenceClaimKind::FactoryConfiguration,
            &evidence_by_id,
            &format!("components[{index}]"),
            &mut issues,
        );
    }

    let mut feature_ids = BTreeSet::new();
    for (index, feature) in proposal.features.iter().enumerate() {
        if feature.feature_catalog_id <= 0 {
            issues.push(ValidationIssue::new(
                "invalid_feature_catalog_id",
                format!("features[{index}] catalog id must be positive"),
            ));
        }
        if !feature_ids.insert(feature.feature_catalog_id) {
            issues.push(ValidationIssue::new(
                "duplicate_reference_feature",
                format!(
                    "feature {} appears more than once",
                    feature.feature_catalog_id
                ),
            ));
        }
        if feature.value.trim().is_empty() {
            issues.push(ValidationIssue::new(
                "empty_reference_feature_value",
                format!("features[{index}] has an empty value"),
            ));
        }
        require_evidence(
            &feature.evidence_id,
            EvidenceClaimKind::MaterialFeature,
            &evidence_by_id,
            &format!("features[{index}]"),
            &mut issues,
        );
    }

    validate_suite_memberships(&proposal.suite_memberships, &evidence_by_id, &mut issues);

    let mut price_years = BTreeSet::new();
    for (index, price) in proposal.reference_prices.iter().enumerate() {
        if !price_years.insert(price.model_year) {
            issues.push(ValidationIssue::new(
                "duplicate_reference_price_year",
                format!("reference price year {} appears twice", price.model_year),
            ));
        }
        if !(MIN_AIRCRAFT_MODEL_YEAR..=MAX_AIRCRAFT_MODEL_YEAR).contains(&price.model_year) {
            issues.push(ValidationIssue::new(
                "invalid_reference_price_model_year",
                format!("reference_prices[{index}] has an invalid aircraft model year"),
            ));
        }
        // Model year and dollar year are deliberately independent.  A 2020
        // model-year price list may have become effective in 2019; pretending
        // those are 2020 dollars would introduce an inflation error.
        if !(MIN_AIRCRAFT_MODEL_YEAR..=MAX_AIRCRAFT_MODEL_YEAR)
            .contains(&price.nominal_usd.reference_year)
        {
            issues.push(ValidationIssue::new(
                "invalid_reference_price_dollar_year",
                format!("reference_prices[{index}] has an invalid nominal-dollar year"),
            ));
        }
        if price.nominal_usd.amount_cents <= 0 {
            issues.push(ValidationIssue::new(
                "invalid_reference_price",
                format!("reference_prices[{index}] amount must be positive"),
            ));
        }
        if price.basis != ReferencePriceBasis::FullStandardConfiguration {
            issues.push(ValidationIssue::new(
                "unsupported_reference_price_basis",
                "first implementation requires a full-standard-configuration price",
            ));
        }
        require_evidence(
            &price.evidence_id,
            EvidenceClaimKind::ReferencePrice,
            &evidence_by_id,
            &format!("reference_prices[{index}]"),
            &mut issues,
        );
    }
    if proposal.profile_version_id <= 0 {
        issues.push(ValidationIssue::new(
            "invalid_profile_version_id",
            "configuration profile version id must be positive",
        ));
    }

    let mut value_keys = BTreeSet::new();
    let mut value_reference_year = None;
    for (index, value) in proposal.component_values.iter().enumerate() {
        value
            .component
            .validate(&format!("component_values[{index}]"), &mut issues);
        if !value_keys.insert(value.component) {
            issues.push(ValidationIssue::new(
                "duplicate_component_value",
                format!(
                    "component value {}:{} appears twice",
                    value.component.kind.token(),
                    value.component.catalog_id
                ),
            ));
        }
        if value.installed_contribution.amount_cents < 0
            || !(MIN_AIRCRAFT_MODEL_YEAR..=MAX_AIRCRAFT_MODEL_YEAR)
                .contains(&value.installed_contribution.reference_year)
        {
            issues.push(ValidationIssue::new(
                "invalid_component_value",
                format!("component_values[{index}] has an invalid amount or year"),
            ));
        }
        if value_reference_year
            .replace(value.installed_contribution.reference_year)
            .is_some_and(|year| year != value.installed_contribution.reference_year)
        {
            issues.push(ValidationIssue::new(
                "mixed_component_value_reference_years",
                "one approved configuration cannot mix component-value reference years",
            ));
        }
        require_evidence(
            &value.evidence_id,
            EvidenceClaimKind::InstalledMarketContribution,
            &evidence_by_id,
            &format!("component_values[{index}]"),
            &mut issues,
        );
    }

    validation_result(issues)
}

fn validate_evidence<'a>(
    evidence: &'a [EvidenceClaimProposal],
    issues: &mut Vec<ValidationIssue>,
) -> BTreeMap<&'a str, &'a EvidenceClaimProposal> {
    let mut evidence_by_id = BTreeMap::new();
    for (index, claim) in evidence.iter().enumerate() {
        if let Err(errors) = validate_evidence_claim_proposal(claim) {
            issues.extend(errors.0);
        }
        let id = claim.evidence_id.trim();
        if id.is_empty() {
            issues.push(ValidationIssue::new(
                "empty_configuration_evidence_id",
                format!("evidence[{index}] id is empty"),
            ));
        } else if evidence_by_id.insert(id, claim).is_some() {
            issues.push(ValidationIssue::new(
                "duplicate_configuration_evidence_id",
                format!("evidence id {id} appears twice"),
            ));
        }
        if claim.source_kind == EvidenceSourceKind::MarketplaceListing {
            issues.push(ValidationIssue::new(
                "listing_used_as_reference_evidence",
                format!("evidence[{index}] is a marketplace listing"),
            ));
        }
    }
    evidence_by_id
}

fn require_evidence(
    evidence_id: &str,
    claim_kind: EvidenceClaimKind,
    evidence_by_id: &BTreeMap<&str, &EvidenceClaimProposal>,
    path: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(evidence) = evidence_by_id.get(evidence_id.trim()) else {
        issues.push(ValidationIssue::new(
            "unknown_evidence_reference",
            format!("{path} references unknown evidence {evidence_id}"),
        ));
        return;
    };
    if !evidence.supports.contains(&claim_kind) {
        issues.push(ValidationIssue::new(
            "evidence_does_not_support_claim",
            format!("{path} evidence does not support {claim_kind:?}"),
        ));
    }
    if matches!(
        claim_kind,
        EvidenceClaimKind::FactoryConfiguration | EvidenceClaimKind::ReferencePrice
    ) && !evidence.source_kind.is_primary()
    {
        issues.push(ValidationIssue::new(
            "nonprimary_reference_evidence",
            format!("{path} requires primary reference evidence"),
        ));
    }
}

fn validate_suite_memberships(
    memberships: &[SuiteMembershipProposal],
    evidence_by_id: &BTreeMap<&str, &EvidenceClaimProposal>,
    issues: &mut Vec<ValidationIssue>,
) {
    let mut pairs = BTreeSet::new();
    let suite_ids = memberships
        .iter()
        .map(|membership| membership.suite)
        .collect::<BTreeSet<_>>();
    let mut owner_by_component = BTreeMap::<CatalogComponentKey, CatalogComponentKey>::new();
    for (index, membership) in memberships.iter().enumerate() {
        membership
            .suite
            .validate(&format!("suite_memberships[{index}].suite"), issues);
        membership
            .component
            .validate(&format!("suite_memberships[{index}].component"), issues);
        if membership.suite == membership.component {
            issues.push(ValidationIssue::new(
                "suite_contains_itself",
                format!("suite_memberships[{index}] contains itself"),
            ));
        }
        if membership.quantity == 0 {
            issues.push(ValidationIssue::new(
                "zero_suite_component_quantity",
                format!("suite_memberships[{index}] quantity must be positive"),
            ));
        }
        if !pairs.insert((membership.suite, membership.component)) {
            issues.push(ValidationIssue::new(
                "duplicate_suite_membership",
                format!("suite_memberships[{index}] duplicates an earlier row"),
            ));
        }
        if let Some(previous_suite) =
            owner_by_component.insert(membership.component, membership.suite)
        {
            if previous_suite != membership.suite {
                issues.push(ValidationIssue::new(
                    "component_in_multiple_suites",
                    format!(
                        "component {}:{} belongs to more than one suite",
                        membership.component.kind.token(),
                        membership.component.catalog_id
                    ),
                ));
            }
        }
        require_evidence(
            &membership.evidence_id,
            EvidenceClaimKind::FactoryConfiguration,
            evidence_by_id,
            &format!("suite_memberships[{index}]"),
            issues,
        );
    }
    if memberships
        .iter()
        .any(|membership| suite_ids.contains(&membership.component))
    {
        issues.push(ValidationIssue::new(
            "nested_suites_not_supported",
            "first implementation does not support nested suite valuation",
        ));
    }
}

fn validation_result(issues: Vec<ValidationIssue>) -> Result<(), ValidationErrors> {
    if issues.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::from_unsorted(issues))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ApprovedReferenceComponent {
    quantity: u32,
    inclusion: FactoryInclusion,
    evidence_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovedReferenceFeature {
    pub feature_catalog_id: i64,
    pub value: String,
    pub unit: Option<String>,
    pub evidence_id: String,
}

/// An approved configuration is immutable.  A corrected component set is a new
/// version, never an in-place refresh of this value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApprovedReferenceConfiguration {
    configuration_version_id: i64,
    reference_catalog_snapshot_id: u64,
    supersedes_configuration_version_id: Option<i64>,
    profile: ApprovedReferenceProfile,
    components: BTreeMap<CatalogComponentKey, ApprovedReferenceComponent>,
    features: BTreeMap<i64, ApprovedReferenceFeature>,
    suite_memberships: Vec<SuiteMembershipProposal>,
    reference_prices: BTreeMap<i32, ReferencePriceProposal>,
    component_values: BTreeMap<CatalogComponentKey, ComponentValueProposal>,
    evidence_ids: Vec<String>,
    canonical_configuration_key: String,
}

impl ApprovedReferenceConfiguration {
    pub fn approve(
        proposal: ReferenceConfigurationProposal,
        profile: ApprovedReferenceProfile,
    ) -> Result<Self, ValidationErrors> {
        validate_reference_configuration_proposal(&proposal)?;
        let mut linkage_issues = Vec::new();
        if proposal.profile_version_id != profile.profile_version_id() {
            linkage_issues.push(ValidationIssue::new(
                "configuration_profile_version_mismatch",
                format!(
                    "proposal profile {} does not match approved profile {}",
                    proposal.profile_version_id,
                    profile.profile_version_id()
                ),
            ));
        }
        for model_year in
            profile.applicability().first_model_year..=profile.applicability().last_model_year
        {
            if !proposal
                .reference_prices
                .iter()
                .any(|price| price.model_year == model_year)
            {
                linkage_issues.push(ValidationIssue::new(
                    "missing_exact_year_reference_price",
                    format!("profile has no full-configuration price for model year {model_year}"),
                ));
            }
        }
        validation_result(linkage_issues)?;
        let components = proposal
            .components
            .iter()
            .map(|component| {
                (
                    component.component,
                    ApprovedReferenceComponent {
                        quantity: component.quantity,
                        inclusion: component.inclusion,
                        evidence_id: component.evidence_id.trim().to_string(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let features = proposal
            .features
            .iter()
            .map(|feature| {
                (
                    feature.feature_catalog_id,
                    ApprovedReferenceFeature {
                        feature_catalog_id: feature.feature_catalog_id,
                        value: feature.value.trim().to_string(),
                        unit: feature
                            .unit
                            .as_deref()
                            .map(str::trim)
                            .filter(|unit| !unit.is_empty())
                            .map(str::to_string),
                        evidence_id: feature.evidence_id.trim().to_string(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut suite_memberships = proposal.suite_memberships.clone();
        suite_memberships.sort();
        let reference_prices = proposal
            .reference_prices
            .iter()
            .map(|price| (price.model_year, price.clone()))
            .collect::<BTreeMap<_, _>>();
        let component_values = proposal
            .component_values
            .iter()
            .map(|value| (value.component, value.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut evidence_ids = proposal
            .evidence
            .iter()
            .map(|evidence| evidence.evidence_id.trim().to_string())
            .collect::<Vec<_>>();
        evidence_ids.sort();
        evidence_ids.dedup();
        let canonical_configuration_key = canonical_configuration_key(
            proposal.configuration_version_id,
            proposal.reference_catalog_snapshot_id,
            profile.canonical_profile_key(),
            &components,
            &features,
            &suite_memberships,
            &reference_prices,
            &component_values,
            &evidence_ids,
        );
        Ok(Self {
            configuration_version_id: proposal.configuration_version_id,
            reference_catalog_snapshot_id: proposal.reference_catalog_snapshot_id,
            supersedes_configuration_version_id: proposal.supersedes_configuration_version_id,
            profile,
            components,
            features,
            suite_memberships,
            reference_prices,
            component_values,
            evidence_ids,
            canonical_configuration_key,
        })
    }

    pub fn configuration_version_id(&self) -> i64 {
        self.configuration_version_id
    }

    pub fn reference_catalog_snapshot_id(&self) -> u64 {
        self.reference_catalog_snapshot_id
    }

    pub fn supersedes_configuration_version_id(&self) -> Option<i64> {
        self.supersedes_configuration_version_id
    }

    pub fn profile(&self) -> &ApprovedReferenceProfile {
        &self.profile
    }

    pub fn components(
        &self,
    ) -> impl Iterator<Item = (CatalogComponentKey, u32, FactoryInclusion, &str)> {
        self.components.iter().map(|(component, approved)| {
            (
                *component,
                approved.quantity,
                approved.inclusion,
                approved.evidence_id.as_str(),
            )
        })
    }

    pub fn features(&self) -> impl Iterator<Item = &ApprovedReferenceFeature> {
        self.features.values()
    }

    pub fn suite_memberships(&self) -> &[SuiteMembershipProposal] {
        &self.suite_memberships
    }

    pub fn component_values(&self) -> impl Iterator<Item = &ComponentValueProposal> {
        self.component_values.values()
    }

    pub fn evidence_ids(&self) -> &[String] {
        &self.evidence_ids
    }

    pub fn canonical_configuration_key(&self) -> &str {
        &self.canonical_configuration_key
    }

    pub fn reference_price(&self, model_year: i32) -> Option<&ReferencePriceProposal> {
        self.reference_prices.get(&model_year)
    }
}

pub fn validate_approved_configuration_successor(
    current: &ApprovedReferenceConfiguration,
    successor: &ApprovedReferenceConfiguration,
) -> Result<(), ValidationErrors> {
    let mut issues = Vec::new();
    if successor.supersedes_configuration_version_id() != Some(current.configuration_version_id()) {
        issues.push(ValidationIssue::new(
            "configuration_successor_does_not_supersede_current",
            "successor must name the current configuration version",
        ));
    }
    if successor.configuration_version_id() == current.configuration_version_id() {
        issues.push(ValidationIssue::new(
            "configuration_successor_reuses_version_id",
            "successor must have a new immutable version id",
        ));
    }
    if successor.reference_catalog_snapshot_id() <= current.reference_catalog_snapshot_id() {
        issues.push(ValidationIssue::new(
            "configuration_successor_snapshot_not_newer",
            "successor must belong to a newer reference catalog snapshot",
        ));
    }
    if successor.profile().hierarchy() != current.profile().hierarchy() {
        issues.push(ValidationIssue::new(
            "configuration_successor_changes_aircraft_identity",
            "a configuration successor cannot move to a different maker/model/variant/generation/tier",
        ));
    }
    validation_result(issues)
}

/// Validate the configurations selected for one immutable active snapshot.
/// Historical successors should be validated separately and must not be mixed
/// into this active set.
pub fn validate_active_reference_configuration_set(
    configurations: &[ApprovedReferenceConfiguration],
) -> Result<(), ValidationErrors> {
    let mut issues = Vec::new();
    let mut configuration_ids = BTreeSet::new();
    let mut profile_ids = BTreeSet::new();
    let snapshot_id = configurations
        .first()
        .map(ApprovedReferenceConfiguration::reference_catalog_snapshot_id);
    for configuration in configurations {
        if !configuration_ids.insert(configuration.configuration_version_id()) {
            issues.push(ValidationIssue::new(
                "duplicate_active_configuration_version_id",
                format!(
                    "configuration version {} appears more than once",
                    configuration.configuration_version_id()
                ),
            ));
        }
        if !profile_ids.insert(configuration.profile().profile_version_id()) {
            issues.push(ValidationIssue::new(
                "multiple_active_configurations_for_profile",
                format!(
                    "profile version {} has more than one active configuration",
                    configuration.profile().profile_version_id()
                ),
            ));
        }
        if snapshot_id != Some(configuration.reference_catalog_snapshot_id()) {
            issues.push(ValidationIssue::new(
                "mixed_active_reference_snapshots",
                "active configurations must all belong to one frozen reference snapshot",
            ));
        }
    }
    validation_result(issues)
}

fn canonical_configuration_key(
    configuration_version_id: i64,
    snapshot_id: u64,
    profile_key: &str,
    components: &BTreeMap<CatalogComponentKey, ApprovedReferenceComponent>,
    features: &BTreeMap<i64, ApprovedReferenceFeature>,
    suites: &[SuiteMembershipProposal],
    prices: &BTreeMap<i32, ReferencePriceProposal>,
    component_values: &BTreeMap<CatalogComponentKey, ComponentValueProposal>,
    evidence_ids: &[String],
) -> String {
    let components = components
        .iter()
        .map(|(key, value)| {
            format!(
                "{}:{}:{}:{:?}:{}",
                key.kind.token(),
                key.catalog_id,
                value.quantity,
                value.inclusion,
                value.evidence_id
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let features = features
        .values()
        .map(|feature| {
            format!(
                "{}:{}:{}:{}",
                feature.feature_catalog_id,
                feature.value,
                feature.unit.as_deref().unwrap_or(""),
                feature.evidence_id,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let suites = suites
        .iter()
        .map(|membership| {
            format!(
                "{}:{}>{}:{}:{}:{}",
                membership.suite.kind.token(),
                membership.suite.catalog_id,
                membership.component.kind.token(),
                membership.component.catalog_id,
                membership.quantity,
                membership.evidence_id
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let prices = prices
        .values()
        .map(|price| {
            format!(
                "{}:{}:{}:{:?}:{}",
                price.model_year,
                price.nominal_usd.amount_cents,
                price.nominal_usd.reference_year,
                price.basis,
                price.evidence_id
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let values = component_values
        .values()
        .map(|value| {
            format!(
                "{}:{}:{}:{}:{}",
                value.component.kind.token(),
                value.component.catalog_id,
                value.installed_contribution.amount_cents,
                value.installed_contribution.reference_year,
                value.evidence_id
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let evidence = evidence_ids.join(",");
    format!(
        "configuration={configuration_version_id}|snapshot={snapshot_id}|{profile_key}|components={components}|features={features}|suites={suites}|prices={prices}|values={values}|evidence={evidence}"
    )
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingEvidenceConfidence {
    VeryHigh,
    High,
    Medium,
    Low,
}

impl ListingEvidenceConfidence {
    fn may_change_configuration(self) -> bool {
        matches!(self, Self::VeryHigh | Self::High)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingComponentAction {
    /// The quantity is the observed installed total, not an amount to add.
    Installed {
        component: CatalogComponentKey,
        observed_total_quantity: u32,
    },
    Replaces {
        removed_component: CatalogComponentKey,
        installed_component: CatalogComponentKey,
        installed_total_quantity: u32,
    },
    Removes {
        component: CatalogComponentKey,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListingComponentObservation {
    pub action: ListingComponentAction,
    pub confidence: ListingEvidenceConfidence,
    pub evidence_text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IgnoredListingObservation {
    pub observation_index: usize,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentDeltaKind {
    Added,
    Removed,
    QuantityChanged,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedComponentDelta {
    pub component: CatalogComponentKey,
    pub quantity_change: i64,
    pub kind: ComponentDeltaKind,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListingOnlyConfigurationFeatures {
    pub reference_configuration_version_id: i64,
    pub reference_catalog_snapshot_id: u64,
    pub generation_id: Option<i64>,
    pub tier_id: Option<i64>,
    pub standard_configuration_key: String,
    pub resolved_configuration_key: String,
    pub delta_tokens: Vec<String>,
    pub complete_approved_delta: Option<NominalUsd>,
    pub has_unvalued_deltas: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceValuationTerms {
    /// This amount already includes every standard component.  Consumers must
    /// not add the standard component set to it.
    pub full_standard_configuration_price: NominalUsd,
    /// Only the listing's difference from the approved standard configuration.
    /// It can have a different nominal reference year, so this type deliberately
    /// exposes no `total()` method.
    pub listing_configuration_delta: NominalUsd,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolvedValuationConfiguration {
    reference_configuration_version_id: i64,
    reference_catalog_snapshot_id: u64,
    reference_configuration_key: String,
    model_year: i32,
    standard_components: BTreeMap<CatalogComponentKey, u32>,
    effective_components: BTreeMap<CatalogComponentKey, u32>,
    deltas: Vec<ResolvedComponentDelta>,
    ignored_observations: Vec<IgnoredListingObservation>,
    complete_approved_delta: Option<NominalUsd>,
    unvalued_delta_components: Vec<CatalogComponentKey>,
    standard_configuration_key: String,
    resolved_configuration_key: String,
    reference_price: ReferencePriceProposal,
    generation_id: Option<i64>,
    tier_id: Option<i64>,
}

impl ResolvedValuationConfiguration {
    pub fn reference_configuration_version_id(&self) -> i64 {
        self.reference_configuration_version_id
    }

    pub fn reference_catalog_snapshot_id(&self) -> u64 {
        self.reference_catalog_snapshot_id
    }

    pub fn reference_configuration_key(&self) -> &str {
        &self.reference_configuration_key
    }

    pub fn model_year(&self) -> i32 {
        self.model_year
    }

    pub fn standard_components(&self) -> &BTreeMap<CatalogComponentKey, u32> {
        &self.standard_components
    }

    pub fn effective_components(&self) -> &BTreeMap<CatalogComponentKey, u32> {
        &self.effective_components
    }

    pub fn deltas(&self) -> &[ResolvedComponentDelta] {
        &self.deltas
    }

    pub fn ignored_observations(&self) -> &[IgnoredListingObservation] {
        &self.ignored_observations
    }

    pub fn unvalued_delta_components(&self) -> &[CatalogComponentKey] {
        &self.unvalued_delta_components
    }

    /// Features for structural/DNN training or inference.  The reference price
    /// is intentionally absent so listing price remains the sole target basis.
    pub fn listing_only_features(&self) -> ListingOnlyConfigurationFeatures {
        ListingOnlyConfigurationFeatures {
            reference_configuration_version_id: self.reference_configuration_version_id,
            reference_catalog_snapshot_id: self.reference_catalog_snapshot_id,
            generation_id: self.generation_id,
            tier_id: self.tier_id,
            standard_configuration_key: self.standard_configuration_key.clone(),
            resolved_configuration_key: self.resolved_configuration_key.clone(),
            delta_tokens: self
                .deltas
                .iter()
                .map(|delta| {
                    format!(
                        "delta:{}:{:+}",
                        delta.component.feature_token(),
                        delta.quantity_change
                    )
                })
                .collect(),
            complete_approved_delta: self.complete_approved_delta,
            has_unvalued_deltas: !self.unvalued_delta_components.is_empty(),
        }
    }

    /// Return the two disjoint terms for reference valuation.  Standard
    /// components are already present in the first term and never re-added.
    pub fn reference_valuation_terms(
        &self,
    ) -> Result<ReferenceValuationTerms, ConfigurationResolutionError> {
        let Some(delta) = self.complete_approved_delta else {
            return Err(ConfigurationResolutionError::UnvaluedConfigurationDelta(
                self.unvalued_delta_components.clone(),
            ));
        };
        Ok(ReferenceValuationTerms {
            full_standard_configuration_price: self.reference_price.nominal_usd,
            listing_configuration_delta: delta,
        })
    }

    /// Monetary adjustment for moving an already age/hour-normalized comparable
    /// from its listing delta to the target listing delta.  It intentionally
    /// does not invent a premium between different factory tiers.
    pub fn comparable_listing_delta_adjustment_to(
        &self,
        target: &Self,
    ) -> Result<NominalUsd, ConfigurationResolutionError> {
        if self.reference_configuration_version_id != target.reference_configuration_version_id
            || self.reference_catalog_snapshot_id != target.reference_catalog_snapshot_id
            || self.reference_configuration_key != target.reference_configuration_key
            || self.standard_configuration_key != target.standard_configuration_key
        {
            return Err(
                ConfigurationResolutionError::IncompatibleReferenceConfigurations {
                    comparable_configuration_version_id: self.reference_configuration_version_id,
                    target_configuration_version_id: target.reference_configuration_version_id,
                },
            );
        }
        let comparable_delta = self.complete_approved_delta.ok_or_else(|| {
            ConfigurationResolutionError::UnvaluedConfigurationDelta(
                self.unvalued_delta_components.clone(),
            )
        })?;
        let target_delta = target.complete_approved_delta.ok_or_else(|| {
            ConfigurationResolutionError::UnvaluedConfigurationDelta(
                target.unvalued_delta_components.clone(),
            )
        })?;
        target_delta.checked_difference(comparable_delta).ok_or(
            ConfigurationResolutionError::IncompatibleValueReferenceYears {
                left: comparable_delta.reference_year,
                right: target_delta.reference_year,
            },
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ConfigurationResolutionError {
    ModelYearOutsideProfile(i32),
    MissingReferencePrice(i32),
    InvalidListingObservation {
        observation_index: usize,
        reason: String,
    },
    ConflictingListingActions(String),
    PartialSuiteMutation(CatalogComponentKey),
    MonetaryOverflow,
    UnvaluedConfigurationDelta(Vec<CatalogComponentKey>),
    IncompatibleValueReferenceYears {
        left: i32,
        right: i32,
    },
    IncompatibleReferenceConfigurations {
        comparable_configuration_version_id: i64,
        target_configuration_version_id: i64,
    },
}

impl fmt::Display for ConfigurationResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModelYearOutsideProfile(year) => {
                write!(formatter, "model year {year} is outside the reference profile")
            }
            Self::MissingReferencePrice(year) => {
                write!(formatter, "reference profile has no exact price for {year}")
            }
            Self::InvalidListingObservation {
                observation_index,
                reason,
            } => write!(
                formatter,
                "listing observation {observation_index} is invalid: {reason}"
            ),
            Self::ConflictingListingActions(reason) => {
                write!(formatter, "listing actions conflict: {reason}")
            }
            Self::PartialSuiteMutation(component) => write!(
                formatter,
                "component {}:{} is bundled in an installed suite and cannot be removed independently",
                component.kind.token(),
                component.catalog_id
            ),
            Self::MonetaryOverflow => formatter.write_str("configuration value overflow"),
            Self::UnvaluedConfigurationDelta(components) => write!(
                formatter,
                "configuration delta has {} component(s) without approved values",
                components.len()
            ),
            Self::IncompatibleValueReferenceYears { left, right } => write!(
                formatter,
                "component value reference years differ: {left} versus {right}"
            ),
            Self::IncompatibleReferenceConfigurations {
                comparable_configuration_version_id,
                target_configuration_version_id,
            } => write!(
                formatter,
                "listing deltas use incompatible reference configurations {comparable_configuration_version_id} and {target_configuration_version_id}"
            ),
        }
    }
}

impl Error for ConfigurationResolutionError {}

pub fn resolve_valuation_configuration(
    reference: &ApprovedReferenceConfiguration,
    model_year: i32,
    observations: &[ListingComponentObservation],
) -> Result<ResolvedValuationConfiguration, ConfigurationResolutionError> {
    if !(reference.profile().applicability().first_model_year
        ..=reference.profile().applicability().last_model_year)
        .contains(&model_year)
    {
        return Err(ConfigurationResolutionError::ModelYearOutsideProfile(
            model_year,
        ));
    }
    let reference_price = reference.reference_price(model_year).cloned().ok_or(
        ConfigurationResolutionError::MissingReferencePrice(model_year),
    )?;

    let standard_raw = reference
        .components
        .iter()
        .filter(|(_, component)| component.inclusion.is_standard_configuration())
        .map(|(key, component)| (*key, component.quantity))
        .collect::<BTreeMap<_, _>>();
    let standard_components =
        canonicalize_suite_quantities(&standard_raw, &reference.suite_memberships);
    let mut effective_raw = standard_raw.clone();
    let mut ignored_observations = Vec::new();
    let mut removals = BTreeSet::new();
    let mut installations = BTreeMap::<CatalogComponentKey, u32>::new();

    for (index, observation) in observations.iter().enumerate() {
        if !observation.confidence.may_change_configuration() {
            ignored_observations.push(IgnoredListingObservation {
                observation_index: index,
                reason: "only high/very-high listing evidence may change the factory baseline"
                    .to_string(),
            });
            continue;
        }
        if observation.evidence_text.trim().len() < 4 {
            return Err(ConfigurationResolutionError::InvalidListingObservation {
                observation_index: index,
                reason: "high-confidence observation has no usable source excerpt".to_string(),
            });
        }
        match observation.action {
            ListingComponentAction::Installed {
                component,
                observed_total_quantity,
            } => {
                validate_observation_component(index, component, observed_total_quantity)?;
                record_installation(&mut installations, component, observed_total_quantity)?;
            }
            ListingComponentAction::Replaces {
                removed_component,
                installed_component,
                installed_total_quantity,
            } => {
                validate_observation_component(
                    index,
                    installed_component,
                    installed_total_quantity,
                )?;
                if removed_component.catalog_id <= 0 || removed_component == installed_component {
                    return Err(ConfigurationResolutionError::InvalidListingObservation {
                        observation_index: index,
                        reason: "replacement requires two different positive component identities"
                            .to_string(),
                    });
                }
                if !removals.insert(removed_component) {
                    return Err(ConfigurationResolutionError::ConflictingListingActions(
                        format!(
                            "component {} is removed or replaced more than once",
                            removed_component.feature_token()
                        ),
                    ));
                }
                record_installation(
                    &mut installations,
                    installed_component,
                    installed_total_quantity,
                )?;
            }
            ListingComponentAction::Removes { component } => {
                if component.catalog_id <= 0 {
                    return Err(ConfigurationResolutionError::InvalidListingObservation {
                        observation_index: index,
                        reason: "removed component identity must be positive".to_string(),
                    });
                }
                if !removals.insert(component) {
                    return Err(ConfigurationResolutionError::ConflictingListingActions(
                        format!(
                            "component {} is removed or replaced more than once",
                            component.feature_token()
                        ),
                    ));
                }
            }
        }
    }

    if let Some(conflict) = removals
        .iter()
        .find(|component| installations.contains_key(component))
    {
        return Err(ConfigurationResolutionError::ConflictingListingActions(
            format!(
                "component {} is both installed and removed",
                conflict.feature_token()
            ),
        ));
    }

    for component in &removals {
        if is_component_bundled_by_present_suite(
            *component,
            &effective_raw,
            &reference.suite_memberships,
        ) {
            return Err(ConfigurationResolutionError::PartialSuiteMutation(
                *component,
            ));
        }
        remove_component_and_its_bundle(
            *component,
            &mut effective_raw,
            &reference.suite_memberships,
        );
    }
    for (component, quantity) in installations {
        effective_raw
            .entry(component)
            .and_modify(|current| *current = (*current).max(quantity))
            .or_insert(quantity);
    }

    let effective_components =
        canonicalize_suite_quantities(&effective_raw, &reference.suite_memberships);
    let deltas = component_deltas(&standard_components, &effective_components);
    let (complete_approved_delta, unvalued_delta_components) = monetary_delta(
        &deltas,
        &reference.component_values,
        reference_price.nominal_usd.reference_year,
    )?;
    let standard_configuration_key = component_set_key(&standard_components);
    let effective_key = component_set_key(&effective_components);
    let resolved_configuration_key = format!(
        "{}|standard={standard_configuration_key}|effective={effective_key}",
        reference.canonical_configuration_key()
    );
    Ok(ResolvedValuationConfiguration {
        reference_configuration_version_id: reference.configuration_version_id(),
        reference_catalog_snapshot_id: reference.reference_catalog_snapshot_id(),
        reference_configuration_key: reference.canonical_configuration_key().to_string(),
        model_year,
        standard_components,
        effective_components,
        deltas,
        ignored_observations,
        complete_approved_delta,
        unvalued_delta_components,
        standard_configuration_key,
        resolved_configuration_key,
        reference_price,
        generation_id: reference.profile().hierarchy().generation_id,
        tier_id: reference.profile().hierarchy().tier_id,
    })
}

fn validate_observation_component(
    index: usize,
    component: CatalogComponentKey,
    quantity: u32,
) -> Result<(), ConfigurationResolutionError> {
    if component.catalog_id <= 0 || quantity == 0 {
        return Err(ConfigurationResolutionError::InvalidListingObservation {
            observation_index: index,
            reason: "installed component identity and quantity must be positive".to_string(),
        });
    }
    Ok(())
}

fn record_installation(
    installations: &mut BTreeMap<CatalogComponentKey, u32>,
    component: CatalogComponentKey,
    observed_total_quantity: u32,
) -> Result<(), ConfigurationResolutionError> {
    if let Some(previous_quantity) = installations.insert(component, observed_total_quantity) {
        if previous_quantity != observed_total_quantity {
            return Err(ConfigurationResolutionError::ConflictingListingActions(
                format!(
                    "component {} has conflicting observed totals {previous_quantity} and {observed_total_quantity}",
                    component.feature_token()
                ),
            ));
        }
    }
    Ok(())
}

fn canonicalize_suite_quantities(
    raw: &BTreeMap<CatalogComponentKey, u32>,
    memberships: &[SuiteMembershipProposal],
) -> BTreeMap<CatalogComponentKey, u32> {
    let mut quantities = raw.clone();
    for membership in memberships {
        let Some(suite_quantity) = raw.get(&membership.suite).copied() else {
            continue;
        };
        let bundled = suite_quantity.saturating_mul(membership.quantity);
        if let Some(component_quantity) = quantities.get_mut(&membership.component) {
            *component_quantity = component_quantity.saturating_sub(bundled);
        }
    }
    quantities.retain(|_, quantity| *quantity > 0);
    quantities
}

fn is_component_bundled_by_present_suite(
    component: CatalogComponentKey,
    raw: &BTreeMap<CatalogComponentKey, u32>,
    memberships: &[SuiteMembershipProposal],
) -> bool {
    memberships
        .iter()
        .any(|membership| membership.component == component && raw.contains_key(&membership.suite))
}

fn remove_component_and_its_bundle(
    component: CatalogComponentKey,
    raw: &mut BTreeMap<CatalogComponentKey, u32>,
    memberships: &[SuiteMembershipProposal],
) {
    let removed_suite_quantity = raw.remove(&component).unwrap_or_default();
    if removed_suite_quantity == 0 {
        return;
    }
    for membership in memberships
        .iter()
        .filter(|membership| membership.suite == component)
    {
        let bundled = removed_suite_quantity.saturating_mul(membership.quantity);
        if let Some(component_quantity) = raw.get_mut(&membership.component) {
            *component_quantity = component_quantity.saturating_sub(bundled);
        }
    }
    raw.retain(|_, quantity| *quantity > 0);
}

fn component_deltas(
    standard: &BTreeMap<CatalogComponentKey, u32>,
    effective: &BTreeMap<CatalogComponentKey, u32>,
) -> Vec<ResolvedComponentDelta> {
    let keys = standard
        .keys()
        .chain(effective.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter_map(|component| {
            let before = i64::from(standard.get(&component).copied().unwrap_or_default());
            let after = i64::from(effective.get(&component).copied().unwrap_or_default());
            let quantity_change = after - before;
            (quantity_change != 0).then(|| ResolvedComponentDelta {
                component,
                quantity_change,
                kind: if before == 0 {
                    ComponentDeltaKind::Added
                } else if after == 0 {
                    ComponentDeltaKind::Removed
                } else {
                    ComponentDeltaKind::QuantityChanged
                },
            })
        })
        .collect()
}

fn monetary_delta(
    deltas: &[ResolvedComponentDelta],
    values: &BTreeMap<CatalogComponentKey, ComponentValueProposal>,
    zero_delta_reference_year: i32,
) -> Result<(Option<NominalUsd>, Vec<CatalogComponentKey>), ConfigurationResolutionError> {
    let mut reference_year = None;
    let mut amount = 0_i64;
    let mut unvalued = Vec::new();
    for delta in deltas {
        let Some(value) = values.get(&delta.component) else {
            unvalued.push(delta.component);
            continue;
        };
        if let Some(year) = reference_year {
            if year != value.installed_contribution.reference_year {
                return Err(
                    ConfigurationResolutionError::IncompatibleValueReferenceYears {
                        left: year,
                        right: value.installed_contribution.reference_year,
                    },
                );
            }
        } else {
            reference_year = Some(value.installed_contribution.reference_year);
        }
        let component_delta = value
            .installed_contribution
            .amount_cents
            .checked_mul(delta.quantity_change)
            .ok_or(ConfigurationResolutionError::MonetaryOverflow)?;
        amount = amount
            .checked_add(component_delta)
            .ok_or(ConfigurationResolutionError::MonetaryOverflow)?;
    }
    unvalued.sort();
    unvalued.dedup();
    if !unvalued.is_empty() {
        return Ok((None, unvalued));
    }
    let reference_year = reference_year
        .or_else(|| {
            values
                .values()
                .next()
                .map(|value| value.installed_contribution.reference_year)
        })
        .or((deltas.is_empty()).then_some(zero_delta_reference_year));
    Ok((
        reference_year.map(|reference_year| NominalUsd {
            amount_cents: amount,
            reference_year,
        }),
        Vec::new(),
    ))
}

fn component_set_key(components: &BTreeMap<CatalogComponentKey, u32>) -> String {
    components
        .iter()
        .map(|(component, quantity)| {
            format!(
                "{}:{}:{}",
                component.kind.token(),
                component.catalog_id,
                quantity
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::catalog::{
        AircraftApplicability, AircraftHierarchy, EvidenceClaimKind, ReferenceProfileProposal,
    };

    const SUITE: CatalogComponentKey = CatalogComponentKey {
        kind: AircraftComponentKind::Avionics,
        catalog_id: 100,
    };
    const DISPLAY: CatalogComponentKey = CatalogComponentKey {
        kind: AircraftComponentKind::Avionics,
        catalog_id: 101,
    };
    const TRANSPONDER: CatalogComponentKey = CatalogComponentKey {
        kind: AircraftComponentKind::Avionics,
        catalog_id: 102,
    };
    const UPGRADE: CatalogComponentKey = CatalogComponentKey {
        kind: AircraftComponentKind::Avionics,
        catalog_id: 200,
    };

    fn evidence(id: &str, claims: &[EvidenceClaimKind]) -> EvidenceClaimProposal {
        EvidenceClaimProposal {
            evidence_id: id.to_string(),
            source_url: format!("https://manufacturer.example/{id}"),
            source_title: "Official configuration reference".to_string(),
            evidence_excerpt:
                "The official configuration and exact model-year price are documented here."
                    .to_string(),
            source_kind: EvidenceSourceKind::Manufacturer,
            supports: claims.iter().copied().collect(),
        }
    }

    fn profile(
        variant: i64,
        generation: Option<i64>,
        tier: Option<i64>,
    ) -> ApprovedReferenceProfile {
        ApprovedReferenceProfile::approve(ReferenceProfileProposal {
            profile_version_id: 1,
            catalog_revision: 1,
            supersedes_profile_version_id: None,
            hierarchy: AircraftHierarchy {
                manufacturer_id: 1,
                model_family_id: 22,
                certified_variant_id: variant,
                generation_id: generation,
                tier_id: tier,
            },
            applicability: AircraftApplicability::exact_model_year(2020),
            evidence: vec![evidence(
                "profile",
                &[
                    EvidenceClaimKind::HierarchyIdentity,
                    EvidenceClaimKind::ProductionApplicability,
                    EvidenceClaimKind::FactoryConfiguration,
                ],
            )],
        })
        .unwrap()
    }

    fn approved_configuration(
        variant: i64,
        generation: Option<i64>,
        tier: Option<i64>,
    ) -> ApprovedReferenceConfiguration {
        let approved_profile = profile(variant, generation, tier);
        let configuration_evidence = evidence(
            "configuration",
            &[
                EvidenceClaimKind::FactoryConfiguration,
                EvidenceClaimKind::ProductionApplicability,
                EvidenceClaimKind::ReferencePrice,
                EvidenceClaimKind::ComponentIdentity,
                EvidenceClaimKind::InstalledMarketContribution,
                EvidenceClaimKind::MaterialFeature,
            ],
        );
        let proposal = ReferenceConfigurationProposal {
            configuration_version_id: 10,
            reference_catalog_snapshot_id: 7,
            supersedes_configuration_version_id: None,
            profile_version_id: approved_profile.profile_version_id(),
            components: vec![
                ReferenceComponentProposal {
                    component: SUITE,
                    quantity: 1,
                    inclusion: FactoryInclusion::IncludedInTier,
                    evidence_id: "configuration".to_string(),
                },
                ReferenceComponentProposal {
                    component: DISPLAY,
                    quantity: 2,
                    inclusion: FactoryInclusion::IncludedInTier,
                    evidence_id: "configuration".to_string(),
                },
                ReferenceComponentProposal {
                    component: TRANSPONDER,
                    quantity: 1,
                    inclusion: FactoryInclusion::Standard,
                    evidence_id: "configuration".to_string(),
                },
            ],
            features: vec![ReferenceFeatureProposal {
                feature_catalog_id: 1,
                value: "standard".to_string(),
                unit: None,
                evidence_id: "configuration".to_string(),
            }],
            suite_memberships: vec![SuiteMembershipProposal {
                suite: SUITE,
                component: DISPLAY,
                quantity: 2,
                evidence_id: "configuration".to_string(),
            }],
            reference_prices: vec![ReferencePriceProposal {
                model_year: 2020,
                nominal_usd: NominalUsd {
                    amount_cents: 80_000_000,
                    reference_year: 2020,
                },
                basis: ReferencePriceBasis::FullStandardConfiguration,
                evidence_id: "configuration".to_string(),
            }],
            component_values: vec![
                ComponentValueProposal {
                    component: SUITE,
                    installed_contribution: NominalUsd {
                        amount_cents: 3_000_000,
                        reference_year: 2026,
                    },
                    evidence_id: "configuration".to_string(),
                },
                ComponentValueProposal {
                    component: TRANSPONDER,
                    installed_contribution: NominalUsd {
                        amount_cents: 500_000,
                        reference_year: 2026,
                    },
                    evidence_id: "configuration".to_string(),
                },
                ComponentValueProposal {
                    component: UPGRADE,
                    installed_contribution: NominalUsd {
                        amount_cents: 1_250_000,
                        reference_year: 2026,
                    },
                    evidence_id: "configuration".to_string(),
                },
            ],
            evidence: vec![configuration_evidence],
        };
        ApprovedReferenceConfiguration::approve(proposal, approved_profile).unwrap()
    }

    fn minimal_configuration_proposal(profile_version_id: i64) -> ReferenceConfigurationProposal {
        ReferenceConfigurationProposal {
            configuration_version_id: 20,
            reference_catalog_snapshot_id: 20,
            supersedes_configuration_version_id: None,
            profile_version_id,
            components: vec![],
            features: vec![],
            suite_memberships: vec![],
            reference_prices: vec![ReferencePriceProposal {
                model_year: 2020,
                nominal_usd: NominalUsd {
                    amount_cents: 50_000_000,
                    reference_year: 2020,
                },
                basis: ReferencePriceBasis::FullStandardConfiguration,
                evidence_id: "minimal-reference".to_string(),
            }],
            component_values: vec![],
            evidence: vec![evidence(
                "minimal-reference",
                &[
                    EvidenceClaimKind::FactoryConfiguration,
                    EvidenceClaimKind::ProductionApplicability,
                    EvidenceClaimKind::ReferencePrice,
                ],
            )],
        }
    }

    fn observation(action: ListingComponentAction) -> ListingComponentObservation {
        ListingComponentObservation {
            action,
            confidence: ListingEvidenceConfidence::High,
            evidence_text: "Listing explicitly states this installed configuration.".to_string(),
        }
    }

    #[test]
    fn suite_and_constituent_defaults_are_not_counted_twice() {
        let reference = approved_configuration(22, Some(6), Some(100));
        let resolved = resolve_valuation_configuration(&reference, 2020, &[]).unwrap();
        assert_eq!(resolved.standard_components().get(&SUITE), Some(&1));
        assert!(!resolved.standard_components().contains_key(&DISPLAY));
        assert!(resolved.deltas().is_empty());
        let terms = resolved.reference_valuation_terms().unwrap();
        assert_eq!(
            terms.full_standard_configuration_price.amount_cents,
            80_000_000
        );
        assert_eq!(terms.listing_configuration_delta.amount_cents, 0);
    }

    #[test]
    fn repeating_standard_equipment_in_listing_produces_zero_delta() {
        let reference = approved_configuration(22, Some(6), Some(100));
        let resolved = resolve_valuation_configuration(
            &reference,
            2020,
            &[
                observation(ListingComponentAction::Installed {
                    component: SUITE,
                    observed_total_quantity: 1,
                }),
                observation(ListingComponentAction::Installed {
                    component: DISPLAY,
                    observed_total_quantity: 2,
                }),
                observation(ListingComponentAction::Installed {
                    component: TRANSPONDER,
                    observed_total_quantity: 1,
                }),
            ],
        )
        .unwrap();
        assert!(resolved.deltas().is_empty());
        assert_eq!(
            resolved
                .listing_only_features()
                .complete_approved_delta
                .unwrap()
                .amount_cents,
            0
        );
    }

    #[test]
    fn replacement_is_new_minus_old_exactly_once() {
        let reference = approved_configuration(22, Some(6), Some(100));
        let resolved = resolve_valuation_configuration(
            &reference,
            2020,
            &[observation(ListingComponentAction::Replaces {
                removed_component: TRANSPONDER,
                installed_component: UPGRADE,
                installed_total_quantity: 1,
            })],
        )
        .unwrap();
        assert_eq!(resolved.deltas().len(), 2);
        let delta = resolved
            .listing_only_features()
            .complete_approved_delta
            .unwrap();
        assert_eq!(delta.amount_cents, 750_000);
        let terms = resolved.reference_valuation_terms().unwrap();
        assert_eq!(
            terms.full_standard_configuration_price.amount_cents,
            80_000_000
        );
        assert_eq!(terms.listing_configuration_delta.amount_cents, 750_000);
    }

    #[test]
    fn low_confidence_listing_delta_cannot_change_factory_baseline() {
        let reference = approved_configuration(22, Some(6), Some(100));
        let resolved = resolve_valuation_configuration(
            &reference,
            2020,
            &[ListingComponentObservation {
                action: ListingComponentAction::Installed {
                    component: UPGRADE,
                    observed_total_quantity: 1,
                },
                confidence: ListingEvidenceConfidence::Medium,
                evidence_text: "Possibly upgraded".to_string(),
            }],
        )
        .unwrap();
        assert!(resolved.deltas().is_empty());
        assert_eq!(resolved.ignored_observations().len(), 1);
    }

    #[test]
    fn partial_mutation_of_integrated_suite_fails_closed() {
        let reference = approved_configuration(22, Some(6), Some(100));
        let error = resolve_valuation_configuration(
            &reference,
            2020,
            &[observation(ListingComponentAction::Removes {
                component: DISPLAY,
            })],
        )
        .unwrap_err();
        assert_eq!(
            error,
            ConfigurationResolutionError::PartialSuiteMutation(DISPLAY)
        );
    }

    #[test]
    fn removing_a_whole_suite_removes_its_bundled_defaults_once() {
        let reference = approved_configuration(22, Some(6), Some(100));
        let resolved = resolve_valuation_configuration(
            &reference,
            2020,
            &[observation(ListingComponentAction::Removes {
                component: SUITE,
            })],
        )
        .unwrap();
        assert!(!resolved.effective_components().contains_key(&SUITE));
        assert!(!resolved.effective_components().contains_key(&DISPLAY));
        assert_eq!(
            resolved
                .listing_only_features()
                .complete_approved_delta
                .unwrap()
                .amount_cents,
            -3_000_000
        );
    }

    #[test]
    fn comparable_adjustment_uses_only_listing_deltas() {
        let reference = approved_configuration(22, Some(6), Some(100));
        let standard = resolve_valuation_configuration(&reference, 2020, &[]).unwrap();
        let upgraded = resolve_valuation_configuration(
            &reference,
            2020,
            &[observation(ListingComponentAction::Replaces {
                removed_component: TRANSPONDER,
                installed_component: UPGRADE,
                installed_total_quantity: 1,
            })],
        )
        .unwrap();
        assert_eq!(
            standard
                .comparable_listing_delta_adjustment_to(&upgraded)
                .unwrap()
                .amount_cents,
            750_000
        );
        assert_eq!(
            upgraded
                .comparable_listing_delta_adjustment_to(&standard)
                .unwrap()
                .amount_cents,
            -750_000
        );
    }

    #[test]
    fn observation_order_cannot_change_the_resolved_configuration() {
        let reference = approved_configuration(22, Some(6), Some(100));
        let remove = observation(ListingComponentAction::Removes {
            component: TRANSPONDER,
        });
        let install = observation(ListingComponentAction::Installed {
            component: UPGRADE,
            observed_total_quantity: 1,
        });
        let forward =
            resolve_valuation_configuration(&reference, 2020, &[remove.clone(), install.clone()])
                .unwrap();
        let reverse =
            resolve_valuation_configuration(&reference, 2020, &[install, remove]).unwrap();

        assert_eq!(
            forward.effective_components(),
            reverse.effective_components()
        );
        assert_eq!(forward.deltas(), reverse.deltas());
        assert_eq!(
            forward.listing_only_features(),
            reverse.listing_only_features()
        );
        assert_eq!(
            forward
                .reference_valuation_terms()
                .unwrap()
                .listing_configuration_delta
                .amount_cents,
            750_000
        );
    }

    #[test]
    fn conflicting_listing_actions_fail_closed() {
        let reference = approved_configuration(22, Some(6), Some(100));
        let error = resolve_valuation_configuration(
            &reference,
            2020,
            &[
                observation(ListingComponentAction::Installed {
                    component: TRANSPONDER,
                    observed_total_quantity: 1,
                }),
                observation(ListingComponentAction::Removes {
                    component: TRANSPONDER,
                }),
            ],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ConfigurationResolutionError::ConflictingListingActions(_)
        ));

        let inconsistent_totals = resolve_valuation_configuration(
            &reference,
            2020,
            &[
                observation(ListingComponentAction::Installed {
                    component: UPGRADE,
                    observed_total_quantity: 1,
                }),
                observation(ListingComponentAction::Installed {
                    component: UPGRADE,
                    observed_total_quantity: 2,
                }),
            ],
        )
        .unwrap_err();
        assert!(matches!(
            inconsistent_totals,
            ConfigurationResolutionError::ConflictingListingActions(_)
        ));
    }

    #[test]
    fn listing_only_projection_does_not_expose_reference_price() {
        let reference = approved_configuration(22, Some(6), Some(100));
        let resolved = resolve_valuation_configuration(&reference, 2020, &[]).unwrap();
        let features = resolved.listing_only_features();
        assert_eq!(features.generation_id, Some(6));
        assert_eq!(features.tier_id, Some(100));
        assert!(features.delta_tokens.is_empty());
    }

    #[test]
    fn missing_component_value_blocks_monetary_reference_adjustment() {
        let approved = approved_configuration(22, Some(6), Some(100));
        let approved_profile = approved.profile().clone();
        let mut proposal = ReferenceConfigurationProposal {
            configuration_version_id: 11,
            reference_catalog_snapshot_id: 8,
            supersedes_configuration_version_id: None,
            profile_version_id: approved_profile.profile_version_id(),
            components: vec![ReferenceComponentProposal {
                component: TRANSPONDER,
                quantity: 1,
                inclusion: FactoryInclusion::Standard,
                evidence_id: "configuration".to_string(),
            }],
            features: vec![],
            suite_memberships: vec![],
            reference_prices: vec![approved.reference_price(2020).unwrap().clone()],
            component_values: vec![],
            evidence: vec![evidence(
                "configuration",
                &[
                    EvidenceClaimKind::FactoryConfiguration,
                    EvidenceClaimKind::ProductionApplicability,
                    EvidenceClaimKind::ReferencePrice,
                ],
            )],
        };
        // The proposal remains valid without guessed component contributions.
        let reference =
            ApprovedReferenceConfiguration::approve(proposal.clone(), approved_profile.clone())
                .unwrap();
        let unchanged = resolve_valuation_configuration(&reference, 2020, &[]).unwrap();
        let unchanged_terms = unchanged.reference_valuation_terms().unwrap();
        assert_eq!(unchanged_terms.listing_configuration_delta.amount_cents, 0);
        assert_eq!(
            unchanged_terms.listing_configuration_delta.reference_year,
            2020
        );

        let resolved = resolve_valuation_configuration(
            &reference,
            2020,
            &[observation(ListingComponentAction::Removes {
                component: TRANSPONDER,
            })],
        )
        .unwrap();
        assert!(resolved
            .listing_only_features()
            .complete_approved_delta
            .is_none());
        assert!(resolved.reference_valuation_terms().is_err());

        proposal.component_values.push(ComponentValueProposal {
            component: TRANSPONDER,
            installed_contribution: NominalUsd {
                amount_cents: 500_000,
                reference_year: 2026,
            },
            evidence_id: "configuration".to_string(),
        });
        // The evidence must explicitly support installed market contribution,
        // not merely component identity or factory inclusion.
        assert!(ApprovedReferenceConfiguration::approve(proposal, approved_profile).is_err());
    }

    #[test]
    fn model_year_must_be_inside_the_exact_reference_profile() {
        let reference = approved_configuration(22, Some(6), Some(100));
        assert_eq!(
            resolve_valuation_configuration(&reference, 2019, &[]).unwrap_err(),
            ConfigurationResolutionError::ModelYearOutsideProfile(2019)
        );
    }

    #[test]
    fn every_profile_year_requires_its_own_full_configuration_price() {
        let approved_profile = ApprovedReferenceProfile::approve(ReferenceProfileProposal {
            profile_version_id: 30,
            catalog_revision: 30,
            supersedes_profile_version_id: None,
            hierarchy: AircraftHierarchy {
                manufacturer_id: 1,
                model_family_id: 22,
                certified_variant_id: 22,
                generation_id: Some(6),
                tier_id: Some(100),
            },
            applicability: AircraftApplicability {
                first_model_year: 2020,
                last_model_year: 2021,
                serial_constraints: vec![],
                markets: BTreeSet::new(),
            },
            evidence: vec![evidence(
                "two-year-profile",
                &[
                    EvidenceClaimKind::HierarchyIdentity,
                    EvidenceClaimKind::ProductionApplicability,
                    EvidenceClaimKind::FactoryConfiguration,
                ],
            )],
        })
        .unwrap();
        let proposal = minimal_configuration_proposal(approved_profile.profile_version_id());
        let error =
            ApprovedReferenceConfiguration::approve(proposal, approved_profile).unwrap_err();
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "missing_exact_year_reference_price"));
    }

    #[test]
    fn configuration_must_link_the_exact_approved_profile_version() {
        let approved_profile = profile(22, Some(6), Some(100));
        let mut proposal = minimal_configuration_proposal(approved_profile.profile_version_id());
        proposal.profile_version_id += 1;
        let error =
            ApprovedReferenceConfiguration::approve(proposal, approved_profile).unwrap_err();
        assert!(error
            .0
            .iter()
            .any(|issue| issue.code == "configuration_profile_version_mismatch"));
    }

    #[test]
    fn base_only_prices_and_marketplace_reference_evidence_are_rejected() {
        let approved_profile = profile(22, Some(6), Some(100));
        let mut base_only = minimal_configuration_proposal(approved_profile.profile_version_id());
        base_only.reference_prices[0].basis = ReferencePriceBasis::BaseAircraftOnly;
        let base_error = validate_reference_configuration_proposal(&base_only).unwrap_err();
        assert!(base_error
            .0
            .iter()
            .any(|issue| issue.code == "unsupported_reference_price_basis"));

        let mut listing_backed =
            minimal_configuration_proposal(approved_profile.profile_version_id());
        listing_backed.evidence[0].source_kind = EvidenceSourceKind::MarketplaceListing;
        let listing_error = validate_reference_configuration_proposal(&listing_backed).unwrap_err();
        assert!(listing_error
            .0
            .iter()
            .any(|issue| issue.code == "listing_used_as_reference_evidence"));
    }

    #[test]
    fn aircraft_model_year_is_not_conflated_with_nominal_dollar_year() {
        let approved_profile = profile(22, Some(6), Some(100));
        let mut proposal = minimal_configuration_proposal(approved_profile.profile_version_id());
        proposal.reference_prices[0].nominal_usd.reference_year = 2019;

        let reference =
            ApprovedReferenceConfiguration::approve(proposal, approved_profile).unwrap();
        assert_eq!(
            reference
                .reference_price(2020)
                .unwrap()
                .nominal_usd
                .reference_year,
            2019
        );
    }

    #[test]
    fn approved_configurations_are_replaced_only_by_new_snapshot_successors() {
        let current = approved_configuration(22, Some(6), Some(100));
        let approved_profile = current.profile().clone();
        let mut proposal = minimal_configuration_proposal(approved_profile.profile_version_id());
        proposal.configuration_version_id = 11;
        proposal.reference_catalog_snapshot_id = 8;
        proposal.supersedes_configuration_version_id = Some(current.configuration_version_id());
        let successor =
            ApprovedReferenceConfiguration::approve(proposal.clone(), approved_profile.clone())
                .unwrap();
        validate_approved_configuration_successor(&current, &successor).unwrap();

        proposal.configuration_version_id = 12;
        proposal.reference_catalog_snapshot_id = 9;
        proposal.supersedes_configuration_version_id = None;
        let unrelated =
            ApprovedReferenceConfiguration::approve(proposal, approved_profile).unwrap();
        assert!(validate_approved_configuration_successor(&current, &unrelated).is_err());

        let active_set_error =
            validate_active_reference_configuration_set(&[current.clone(), current]).unwrap_err();
        assert!(active_set_error
            .0
            .iter()
            .any(|issue| issue.code == "duplicate_active_configuration_version_id"));
    }

    #[test]
    fn g6_base_and_gts_are_distinct_immutable_configuration_keys() {
        let base = approved_configuration(22, Some(6), None);
        let gts = approved_configuration(22, Some(6), Some(100));
        assert_ne!(
            base.canonical_configuration_key(),
            gts.canonical_configuration_key()
        );
        assert_ne!(base.profile().hierarchy(), gts.profile().hierarchy());

        let resolved_base = resolve_valuation_configuration(&base, 2020, &[]).unwrap();
        let resolved_gts = resolve_valuation_configuration(&gts, 2020, &[]).unwrap();
        assert!(matches!(
            resolved_base.comparable_listing_delta_adjustment_to(&resolved_gts),
            Err(ConfigurationResolutionError::IncompatibleReferenceConfigurations { .. })
        ));
    }

    #[test]
    fn certified_turbo_variants_remain_separate_in_reference_values() {
        let sr22 = approved_configuration(22, Some(6), Some(100));
        let sr22t = approved_configuration(220, Some(6), Some(100));
        assert_ne!(sr22.profile().hierarchy(), sr22t.profile().hierarchy());
        assert_ne!(
            sr22.canonical_configuration_key(),
            sr22t.canonical_configuration_key()
        );
    }
}
