//! Atomic persistence for independently reviewable aircraft hierarchies.
//!
//! Gemini can research and propose a hierarchy, but only this deterministic
//! boundary may approve catalog rows. One invocation handles one exact listing
//! observation and its current FAA grounding. Catalog approvals, any required
//! FAA legal-make alias, any retained-label-to-family alias, the FAA
//! designation binding, the immutable listing assignment, and the valuation
//! compatibility projection share one database transaction.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use url::Url;

use super::{
    CurationConfidence, EntityResolutionAction, FaaMakeRelationshipAction,
    FamilyLabelRelationshipAction, ReviewableAircraftHierarchy, VerificationVerdict,
};
use crate::aircraft::catalog::{
    normalize_aircraft_designator_retrieval_key, normalize_aircraft_retrieval_text,
    validate_aircraft_hierarchy_proposal, AircraftHierarchy, CatalogEntityProposal,
    EvidenceClaimKind, EvidenceClaimProposal, EvidenceSourceKind,
};
use crate::aircraft::faa::{
    normalize_n_number, normalize_serial_key, AircraftGrounding, SerialMatch,
};
use crate::aircraft::identity::{
    persist_assignment_postgres_in_transaction, persist_assignment_sqlite_in_transaction,
    CanonicalAircraftIdentityAssignment, FaaIdentityEvidence, IdentityAssignmentError,
    PromotionCandidate,
};
use crate::aircraft::observations::{
    retained_source_identity_evidence_matches, AircraftIdentityObservation,
};
use crate::db::{AppDb, DatabaseBackend};

const PERSISTENCE_VERSION: &str = "aircraft-hierarchy-persistence-v8";
const SERVER_FAA_REGISTRY_EVIDENCE_PREFIX: &str = "server_faa_registry.";
const SERVER_FAA_DRS_EVIDENCE_PREFIX: &str = "server_faa_drs.";

#[derive(Clone, Copy)]
pub struct PersistReviewableAircraftHierarchy<'a> {
    pub listing_id: i64,
    pub observation: &'a AircraftIdentityObservation,
    pub expected_catalog_revision: &'a str,
    pub reviewable: &'a ReviewableAircraftHierarchy,
    pub grounding: &'a AircraftGrounding,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PersistedAircraftHierarchy {
    pub hierarchy: AircraftHierarchy,
    pub assignment: CanonicalAircraftIdentityAssignment,
    pub catalog_writes: usize,
    pub idempotent_replay: bool,
    pub approval_fingerprint: String,
}

#[derive(Debug)]
pub enum AircraftHierarchyPersistenceError {
    Invalid(String),
    StaleCatalog {
        expected: String,
        actual: String,
    },
    OptionalSelectionInvalidated {
        dimension: &'static str,
        reason: String,
    },
    Collision(String),
    Faa(String),
    Database(String),
    Assignment(String),
}

impl fmt::Display for AircraftHierarchyPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message)
            | Self::Collision(message)
            | Self::Faa(message)
            | Self::Database(message)
            | Self::Assignment(message) => formatter.write_str(message),
            Self::StaleCatalog { expected, actual } => write!(
                formatter,
                "aircraft catalog changed after adjudication (expected {expected}, current {actual})"
            ),
            Self::OptionalSelectionInvalidated { dimension, reason } => write!(
                formatter,
                "aircraft {dimension} no-supported selection is no longer valid: {reason}"
            ),
        }
    }
}

impl std::error::Error for AircraftHierarchyPersistenceError {}

impl From<sqlx::Error> for AircraftHierarchyPersistenceError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

impl From<IdentityAssignmentError> for AircraftHierarchyPersistenceError {
    fn from(error: IdentityAssignmentError) -> Self {
        Self::Assignment(error.to_string())
    }
}

#[derive(Clone, Debug)]
struct PreparedApproval {
    approval_fingerprint: String,
    payload_json: String,
    validation_json: String,
    claims_by_evidence_id: BTreeMap<String, EvidenceClaimProposal>,
    source_content_sha256_by_evidence_id: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SafeSerialScope {
    prefix: String,
    digits_width: i64,
    first_number: i64,
    last_number: Option<i64>,
}

#[derive(Clone, Debug)]
struct PreparedTcdsMakeLineage {
    tcds_number: String,
    document_guid: String,
    pdf_sha256: String,
    former_holder_name: String,
    current_holder_name: String,
    manufacturer_name: Option<String>,
    selection_basis: &'static str,
    serial_scope_kind: &'static str,
    serial_scope: SafeSerialScope,
    faa_make_evidence_id: String,
    model_identity_evidence_id: String,
    serial_applicability_evidence_id: String,
    holder_transfer_evidence_id: String,
    manufacturer_range_evidence_id: Option<String>,
}

#[derive(Debug, FromRow)]
struct ExistingTcdsMakeLineageRow {
    id: i64,
    aircraft_make_id: i64,
    aircraft_designation_id: i64,
    tcds_number: String,
    tcds_document_guid: String,
    tcds_pdf_sha256: String,
    tcds_former_holder_name: String,
    tcds_current_holder_name: String,
    tcds_manufacturer_name: Option<String>,
    tcds_selection_basis: String,
    serial_scope_kind: String,
    serial_prefix: String,
    serial_digits_width: i64,
    first_serial_number: i64,
    last_serial_number: Option<i64>,
}

fn split_safe_serial_key(value: &str) -> Option<(String, i64, i64)> {
    let value = normalize_serial_key(value)?;
    let digit_offset = value
        .char_indices()
        .find_map(|(offset, character)| character.is_ascii_digit().then_some(offset))?;
    let (prefix, digits) = value.split_at(digit_offset);
    if prefix.len() > 16
        || !prefix.bytes().all(|byte| byte.is_ascii_uppercase())
        || digits.is_empty()
        || digits.len() > 18
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let number = digits.parse::<i64>().ok()?;
    Some((
        prefix.to_string(),
        i64::try_from(digits.len()).ok()?,
        number,
    ))
}

fn safe_serial_scope(
    first: &str,
    last: Option<&str>,
    current: &str,
) -> Result<SafeSerialScope, AircraftHierarchyPersistenceError> {
    let (prefix, digits_width, first_number) = split_safe_serial_key(first).ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "FAA TCDS serial interval is not a safe prefix-plus-numeric range".to_string(),
        )
    })?;
    let last_number = last
        .map(|value| {
            split_safe_serial_key(value).ok_or_else(|| {
                AircraftHierarchyPersistenceError::Faa(
                    "FAA TCDS serial interval end is not a safe prefix-plus-numeric key"
                        .to_string(),
                )
            })
        })
        .transpose()?
        .map(|(last_prefix, last_width, number)| {
            if last_prefix != prefix || last_width != digits_width {
                return Err(AircraftHierarchyPersistenceError::Faa(
                    "FAA TCDS serial interval changes prefix or numeric width".to_string(),
                ));
            }
            Ok(number)
        })
        .transpose()?;
    if last_number.is_some_and(|last| last < first_number) {
        return Err(AircraftHierarchyPersistenceError::Faa(
            "FAA TCDS serial interval is reversed".to_string(),
        ));
    }
    let (current_prefix, current_width, current_number) = split_safe_serial_key(current)
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Faa(
                "FAA manufacturer serial is not a safe prefix-plus-numeric key".to_string(),
            )
        })?;
    if current_prefix != prefix
        || current_width != digits_width
        || current_number < first_number
        || last_number.is_some_and(|last| current_number > last)
    {
        return Err(AircraftHierarchyPersistenceError::Faa(
            "FAA manufacturer serial is outside the selected TCDS interval".to_string(),
        ));
    }
    Ok(SafeSerialScope {
        prefix,
        digits_width,
        first_number,
        last_number,
    })
}

fn prepare_tcds_make_lineage(
    reviewable: &ReviewableAircraftHierarchy,
    grounding: &AircraftGrounding,
) -> Result<Option<PreparedTcdsMakeLineage>, AircraftHierarchyPersistenceError> {
    if reviewable.adjudication.faa_make_relationship.action
        != FaaMakeRelationshipAction::MatchTcdsMakeLineage
    {
        return Ok(None);
    }
    let identity = reviewable
        .server_faa_evidence
        .tcds_identity_binding
        .as_ref()
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Faa(
                "FAA TCDS make-lineage action has no exact identity binding".to_string(),
            )
        })?;
    let evidence = reviewable
        .server_faa_evidence
        .tcds_make_lineage_evidence()
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Faa(
                "FAA TCDS make-lineage action has no exact lineage evidence".to_string(),
            )
        })?;
    let holder = evidence.holder_transfer.as_ref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "FAA TCDS make-lineage action has no holder-transfer evidence".to_string(),
        )
    })?;
    let reference = grounding.aircraft.as_ref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "FAA grounding has no aircraft reference for TCDS lineage".to_string(),
        )
    })?;
    let selection_basis = reviewable
        .server_faa_evidence
        .tcds_selection_basis
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Faa(
                "FAA TCDS make-lineage action has no exact selection basis".to_string(),
            )
        })?;
    let registry_tcds = reference
        .type_certificate_data_sheet
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let selection_matches_registry = match selection_basis {
        super::TcdsSelectionBasis::RegistryReference => {
            registry_tcds == Some(identity.tcds_number.as_str())
        }
        super::TcdsSelectionBasis::DrsUniqueCurrentExactModel
        | super::TcdsSelectionBasis::OperatorValidatedExactModelSerial => registry_tcds.is_none(),
    };
    if !selection_matches_registry
        || evidence.document_guid != identity.document_guid
        || evidence.tcds_number != identity.tcds_number
        || evidence.source_url != identity.source_url
        || evidence.pdf_sha256 != identity.pdf_sha256
        || evidence.exact_faa_model != identity.exact_faa_model
        || evidence.faa_serial_key != identity.faa_serial_key
        || reference.model_name.as_deref() != Some(identity.exact_faa_model.as_str())
        || grounding.manufacturer_serial_key.as_deref() != Some(identity.faa_serial_key.as_str())
    {
        return Err(AircraftHierarchyPersistenceError::Faa(
            "FAA registry and TCDS make-lineage document/model/serial provenance differ"
                .to_string(),
        ));
    }
    let (serial_scope_kind, manufacturer_name, first, last) =
        if let Some(manufacturer) = evidence.manufacturer_serial_eligibility.as_ref() {
            if manufacturer.model != identity.exact_faa_model {
                return Err(AircraftHierarchyPersistenceError::Faa(
                    "TCDS manufacturer range belongs to a different model".to_string(),
                ));
            }
            (
                "manufacturer",
                Some(manufacturer.manufacturer_name.clone()),
                manufacturer.first_serial_key.as_str(),
                manufacturer.last_serial_key.as_deref(),
            )
        } else {
            (
                "tcds_model",
                None,
                identity.serial_eligibility.first_serial_key.as_str(),
                identity.serial_eligibility.last_serial_key.as_deref(),
            )
        };
    let serial_scope = safe_serial_scope(first, last, &identity.faa_serial_key)?;
    let claim_ids = reviewable
        .server_faa_evidence
        .tcds_make_lineage_claim_ids()
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Faa(
                "FAA TCDS make-lineage action has no deterministic claim IDs".to_string(),
            )
        })?;
    Ok(Some(PreparedTcdsMakeLineage {
        tcds_number: identity.tcds_number.clone(),
        document_guid: identity.document_guid.clone(),
        pdf_sha256: identity.pdf_sha256.clone(),
        former_holder_name: holder.former_holder_name.clone(),
        current_holder_name: holder.current_holder_name.clone(),
        manufacturer_name,
        selection_basis: selection_basis.as_str(),
        serial_scope_kind,
        serial_scope,
        faa_make_evidence_id: reviewable.server_faa_evidence.make_claim_id().to_string(),
        model_identity_evidence_id: claim_ids.faa_model_heading,
        serial_applicability_evidence_id: claim_ids.serial_eligibility,
        holder_transfer_evidence_id: claim_ids.holder_transfer,
        manufacturer_range_evidence_id: claim_ids.manufacturer_serial_eligibility,
    }))
}

#[derive(Clone, Debug, FromRow)]
struct ObservationRow {
    id: i64,
    aircraft_sale_listing_id: Option<i64>,
    source_url: Option<String>,
    observed_make: Option<String>,
    observed_family: Option<String>,
    observed_designation: Option<String>,
    model_year: Option<i64>,
    serial_number: Option<String>,
    registration_number: Option<String>,
    exact_source_evidence: String,
    observation_sha256: String,
}

#[derive(Clone, Debug, FromRow)]
struct ExistingDecisionRow {
    id: i64,
    entity_kind: String,
    decision_action: String,
    decision_status: String,
    selected_entity_id: Option<i64>,
    decision_payload_json: String,
    deterministic_validation_json: String,
}

#[derive(Clone, Debug)]
struct ResolvedHierarchy {
    hierarchy: AircraftHierarchy,
    make_name: String,
    family_name: String,
    official_designation: String,
    designation_approval_decision_id: i64,
    catalog_writes: usize,
    idempotent_replay: bool,
    assignment: Option<CanonicalAircraftIdentityAssignment>,
}

impl ResolvedHierarchy {
    fn promotion_candidate(&self) -> PromotionCandidate {
        PromotionCandidate {
            aircraft_make_id: self.hierarchy.manufacturer_id,
            make_name: self.make_name.clone(),
            aircraft_model_family_id: self.hierarchy.model_family_id,
            family_name: self.family_name.clone(),
            aircraft_designation_id: self.hierarchy.certified_variant_id,
            official_designation: self.official_designation.clone(),
            identity_decision_id: self.designation_approval_decision_id,
            aircraft_generation_id: self.hierarchy.generation_id,
            aircraft_factory_package_id: self.hierarchy.tier_id,
        }
    }
}

#[derive(Clone, Debug, FromRow)]
struct CurrentAssignmentKey {
    assignment_id: i64,
    aircraft_make_id: i64,
    aircraft_model_family_id: i64,
    aircraft_designation_id: i64,
    aircraft_generation_id: Option<i64>,
    aircraft_factory_package_id: Option<i64>,
    faa_registry_snapshot_id: i64,
    faa_n_number: String,
    faa_source_record_sha256: String,
}

#[derive(Debug, FromRow)]
struct PersistedAssignmentRow {
    assignment_id: i64,
    aircraft_sale_listing_id: i64,
    supersedes_assignment_id: Option<i64>,
    aircraft_make_id: i64,
    make_name: String,
    aircraft_model_family_id: i64,
    family_name: String,
    aircraft_designation_id: i64,
    official_designation: String,
    aircraft_generation_id: Option<i64>,
    aircraft_factory_package_id: Option<i64>,
    identity_decision_id: i64,
    identity_evidence_claim_id: i64,
    faa_registry_snapshot_id: i64,
    faa_n_number: String,
    faa_aircraft_code: String,
    faa_source_record_sha256: String,
    created_at: String,
}

impl PersistedAssignmentRow {
    fn into_public(self) -> CanonicalAircraftIdentityAssignment {
        CanonicalAircraftIdentityAssignment {
            assignment_id: self.assignment_id,
            aircraft_sale_listing_id: self.aircraft_sale_listing_id,
            supersedes_assignment_id: self.supersedes_assignment_id,
            aircraft_make_id: self.aircraft_make_id,
            make_name: self.make_name,
            aircraft_model_family_id: self.aircraft_model_family_id,
            family_name: self.family_name,
            aircraft_designation_id: self.aircraft_designation_id,
            official_designation: self.official_designation,
            aircraft_generation_id: self.aircraft_generation_id,
            aircraft_factory_package_id: self.aircraft_factory_package_id,
            identity_decision_id: self.identity_decision_id,
            identity_evidence_claim_id: self.identity_evidence_claim_id,
            faa_registry_snapshot_id: self.faa_registry_snapshot_id,
            faa_n_number: self.faa_n_number,
            faa_aircraft_code: self.faa_aircraft_code,
            faa_source_record_sha256: self.faa_source_record_sha256,
            created_at: self.created_at,
        }
    }
}

#[derive(Debug, FromRow)]
struct CurrentListingSourceRow {
    listing_source_url: Option<String>,
    model_year: i64,
    serial_number: Option<String>,
    registration_number: Option<String>,
    submission_id: Option<i64>,
    submission_source_url: Option<String>,
    rendered_html_sha256: Option<String>,
    rendered_html: Option<String>,
    extracted_listing_json: Option<String>,
}

#[derive(Default, Deserialize)]
struct CurrentLiteralAircraftFields {
    manufacturer: Option<String>,
    model: Option<String>,
    variant: Option<String>,
    model_year: Option<i64>,
    serial_number: Option<String>,
    registration_number: Option<String>,
}

#[derive(Debug, FromRow)]
struct CurrentFaaGroundingRow {
    snapshot_id: i64,
    evidence_source_id: i64,
    snapshot_date: String,
    source_url: String,
    archive_sha256: String,
    source_manifest_sha256: String,
    target_set_sha256: String,
    lookup_status: String,
    n_number: Option<String>,
    manufacturer_serial_raw: Option<String>,
    manufacturer_serial_key: Option<String>,
    aircraft_code: Option<String>,
    engine_code: Option<String>,
    year_manufactured: Option<i64>,
    source_record_sha256: Option<String>,
    reference_aircraft_code: Option<String>,
    aircraft_manufacturer_name: Option<String>,
    aircraft_model_name: Option<String>,
    aircraft_type_code: Option<String>,
    aircraft_engine_type_code: Option<String>,
    category_code: Option<String>,
    certification_indicator_code: Option<String>,
    engine_count: Option<i64>,
    seat_count: Option<i64>,
    weight_class_code: Option<String>,
    cruise_speed_mph: Option<i64>,
    type_certificate_data_sheet: Option<String>,
    type_certificate_holder: Option<String>,
    reference_engine_code: Option<String>,
    engine_manufacturer_name: Option<String>,
    engine_model_name: Option<String>,
    engine_type_code: Option<String>,
    horsepower: Option<i64>,
    thrust_pounds: Option<i64>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
struct CatalogBaseFingerprintRow {
    entity_kind: String,
    entity_id: i64,
    parent_id: Option<i64>,
    display_name: String,
    authoritative_designator: Option<String>,
    normalized_name: String,
}

#[derive(Clone, Debug, FromRow, Serialize)]
struct CatalogLookupFingerprintRow {
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

#[derive(Clone, Debug, FromRow, Serialize)]
struct CatalogGenerationDesignationFingerprintRow {
    aircraft_generation_id: i64,
    aircraft_designation_id: i64,
}

#[derive(Clone, Debug, FromRow, Serialize)]
struct CatalogPackageApplicabilityFingerprintRow {
    applicability_id: i64,
    aircraft_factory_package_id: i64,
    package_kind: String,
    aircraft_designation_id: i64,
    aircraft_generation_id: Option<i64>,
    valid_from_model_year: Option<i64>,
    valid_to_model_year: Option<i64>,
}

/// Approve and assign one independently reviewable hierarchy.
///
/// The supplied observation must be the exact retained-source observation used
/// by the curation case. The supplied grounding is reloaded from the listing
/// before any write. Replaying the same verified approval fingerprint is a
/// no-op for catalog rows and reuses an already-current exact assignment.
pub async fn persist_reviewable_aircraft_hierarchy(
    db: &AppDb,
    input: PersistReviewableAircraftHierarchy<'_>,
) -> Result<PersistedAircraftHierarchy, AircraftHierarchyPersistenceError> {
    let prepared = prepare_request(&input)?;

    let resolved = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            // Acquire SQLite's writer reservation before transaction-local
            // source, FAA, and catalog revalidation.
            sqlx::query("UPDATE aircraft_markets SET name = name WHERE code = 'GLOBAL'")
                .execute(&mut *transaction)
                .await?;
            revalidate_observation_sqlite(&mut transaction, &input).await?;
            revalidate_faa_grounding_sqlite(&mut transaction, &input).await?;
            let current_revision = catalog_revision_sqlite(&mut transaction).await?;
            let observation_id =
                stage_observation_sqlite(&mut transaction, input.observation).await?;
            let result = persist_sqlite(
                &mut transaction,
                db,
                &input,
                &prepared,
                observation_id,
                &current_revision,
            )
            .await?;
            revalidate_observation_sqlite(&mut transaction, &input).await?;
            revalidate_faa_grounding_sqlite(&mut transaction, &input).await?;
            transaction.commit().await?;
            result
        }
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool.begin().await?;
            sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
                .execute(&mut *transaction)
                .await?;
            sqlx::query(
                r#"
                LOCK TABLE
                  aircraft_makes, aircraft_make_aliases,
                  aircraft_model_families, aircraft_family_aliases,
                  aircraft_designations, aircraft_designation_aliases,
                  aircraft_designation_identifiers, aircraft_generations,
                  aircraft_generation_designations, aircraft_factory_packages,
                  aircraft_package_applicability,
                  aircraft_sale_listings, plugin_submissions,
                  aircraft_manufacturers, aircraft_models, aircraft_model_variants,
                  faa_registry_snapshots, faa_registry_coverage,
                  faa_registry_aircraft, faa_registry_aircraft_references,
                  faa_registry_engine_references
                IN SHARE ROW EXCLUSIVE MODE
                "#,
            )
            .execute(&mut *transaction)
            .await?;
            revalidate_observation_postgres(&mut transaction, &input).await?;
            revalidate_faa_grounding_postgres(&mut transaction, &input).await?;
            let current_revision = catalog_revision_postgres(&mut transaction).await?;
            let observation_id =
                stage_observation_postgres(&mut transaction, input.observation).await?;
            let result = persist_postgres(
                &mut transaction,
                db,
                &input,
                &prepared,
                observation_id,
                &current_revision,
            )
            .await?;
            revalidate_observation_postgres(&mut transaction, &input).await?;
            revalidate_faa_grounding_postgres(&mut transaction, &input).await?;
            transaction.commit().await?;
            result
        }
    };

    let assignment = resolved.assignment.ok_or_else(|| {
        AircraftHierarchyPersistenceError::Assignment(
            "persistence transaction returned no exact listing assignment".to_string(),
        )
    })?;
    Ok(PersistedAircraftHierarchy {
        hierarchy: resolved.hierarchy,
        assignment,
        catalog_writes: resolved.catalog_writes,
        idempotent_replay: resolved.idempotent_replay,
        approval_fingerprint: prepared.approval_fingerprint,
    })
}

const CURRENT_LISTING_SOURCE_SQLITE: &str = r#"
    SELECT listing.source_url AS listing_source_url,
           listing.model_year, listing.serial_number, listing.registration_number,
           submission.id AS submission_id,
           submission.source_url AS submission_source_url,
           submission.rendered_html_sha256, submission.rendered_html,
           submission.extracted_listing_json
    FROM aircraft_sale_listings listing
    LEFT JOIN plugin_submissions submission
      ON submission.id = (
        SELECT candidate.id
        FROM plugin_submissions candidate
        WHERE candidate.canonical_listing_id = listing.id
           OR (
             candidate.canonical_listing_id IS NULL
             AND listing.source_url IS NOT NULL
             AND candidate.source_url = listing.source_url
           )
        ORDER BY
          CASE WHEN candidate.canonical_listing_id IS NOT NULL THEN 0 ELSE 1 END,
          candidate.submitted_at DESC,
          candidate.id DESC
        LIMIT 1
      )
    WHERE listing.id = ?
"#;

const CURRENT_LISTING_SOURCE_POSTGRES: &str = r#"
    SELECT listing.source_url AS listing_source_url,
           listing.model_year, listing.serial_number, listing.registration_number,
           submission.id AS submission_id,
           submission.source_url AS submission_source_url,
           submission.rendered_html_sha256, submission.rendered_html,
           submission.extracted_listing_json
    FROM aircraft_sale_listings listing
    LEFT JOIN plugin_submissions submission
      ON submission.id = (
        SELECT candidate.id
        FROM plugin_submissions candidate
        WHERE candidate.canonical_listing_id = listing.id
           OR (
             candidate.canonical_listing_id IS NULL
             AND listing.source_url IS NOT NULL
             AND candidate.source_url = listing.source_url
           )
        ORDER BY
          CASE WHEN candidate.canonical_listing_id IS NOT NULL THEN 0 ELSE 1 END,
          candidate.submitted_at DESC,
          candidate.id DESC
        LIMIT 1
      )
    WHERE listing.id = $1
    FOR SHARE OF listing
"#;

async fn revalidate_observation_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &PersistReviewableAircraftHierarchy<'_>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let row = sqlx::query_as::<_, CurrentListingSourceRow>(CURRENT_LISTING_SOURCE_SQLITE)
        .bind(input.listing_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(
                "listing disappeared before hierarchy persistence".to_string(),
            )
        })?;
    validate_current_listing_source(&row, input.observation)
}

async fn revalidate_observation_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &PersistReviewableAircraftHierarchy<'_>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let row = sqlx::query_as::<_, CurrentListingSourceRow>(CURRENT_LISTING_SOURCE_POSTGRES)
        .bind(input.listing_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(
                "listing disappeared before hierarchy persistence".to_string(),
            )
        })?;
    validate_current_listing_source(&row, input.observation)
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn retained_identifier_is_absent_or_exact(
    retained_value: Option<&str>,
    admission_value: Option<&str>,
) -> bool {
    trimmed(retained_value)
        .is_none_or(|retained_value| trimmed(admission_value) == Some(retained_value))
}

fn validate_current_listing_source(
    row: &CurrentListingSourceRow,
    expected: &AircraftIdentityObservation,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let rendered_html = row.rendered_html.as_deref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Invalid(
            "selected listing submission no longer retains rendered HTML".to_string(),
        )
    })?;
    let rendered_sha = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
    let literal = row
        .extracted_listing_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<CurrentLiteralAircraftFields>(value).ok())
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(
                "selected listing submission no longer has its literal aircraft extraction"
                    .to_string(),
            )
        })?;
    let selected_source_url = row
        .submission_source_url
        .as_deref()
        .or(row.listing_source_url.as_deref());
    let exact_excerpt = expected.source_excerpt.as_deref().unwrap_or_default();
    let exact = expected.source_kind == "retained_submission"
        && expected.source_excerpt_is_exact
        && !exact_excerpt.trim().is_empty()
        && retained_source_identity_evidence_matches(rendered_html, expected)
        && row.submission_id == expected.submission_id
        && selected_source_url == expected.source_url.as_deref()
        && row.rendered_html_sha256.as_deref() == expected.rendered_html_sha256.as_deref()
        && row.rendered_html_sha256.as_deref() == Some(rendered_sha.as_str())
        && row.model_year == expected.model_year
        && trimmed(row.serial_number.as_deref()) == trimmed(expected.serial_number.as_deref())
        && trimmed(row.registration_number.as_deref())
            == trimmed(expected.registration_number.as_deref())
        && trimmed(literal.manufacturer.as_deref()) == Some(expected.manufacturer.trim())
        && trimmed(literal.model.as_deref()) == Some(expected.model.trim())
        && trimmed(literal.variant.as_deref()) == Some(expected.variant.trim())
        && literal.model_year == Some(expected.model_year)
        // Retained extraction identifiers are advisory: the immutable current
        // listing values drive FAA admission and are revalidated separately
        // before and after persistence. An omitted retained identifier is
        // therefore safe, but a nonempty conflict remains a hard failure.
        && retained_identifier_is_absent_or_exact(
            literal.serial_number.as_deref(),
            expected.serial_number.as_deref(),
        )
        && retained_identifier_is_absent_or_exact(
            literal.registration_number.as_deref(),
            expected.registration_number.as_deref(),
        );
    if !exact {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "listing source, selected retained submission, rendered-content digest, or extracted identity changed after hierarchy validation"
                .to_string(),
        ));
    }
    Ok(())
}

const CURRENT_FAA_GROUNDING_SQLITE: &str = r#"
    WITH latest_release AS (
      SELECT snapshot_date, archive_sha256
      FROM faa_registry_snapshots
      ORDER BY snapshot_date DESC, id DESC
      LIMIT 1
    ),
    covering_snapshot AS (
      SELECT snapshot.id
      FROM faa_registry_snapshots snapshot
      JOIN faa_registry_coverage coverage
        ON coverage.snapshot_id = snapshot.id AND coverage.n_number = ?
      JOIN latest_release latest
        ON latest.snapshot_date = snapshot.snapshot_date
       AND latest.archive_sha256 = snapshot.archive_sha256
      ORDER BY (
        SELECT count(*) FROM faa_registry_coverage target
        WHERE target.snapshot_id = snapshot.id
      ) DESC, snapshot.id DESC
      LIMIT 1
    )
    SELECT snapshot.id AS snapshot_id, snapshot.evidence_source_id,
           snapshot.snapshot_date, snapshot.source_url, snapshot.archive_sha256,
           snapshot.source_manifest_sha256, snapshot.target_set_sha256,
           coverage.lookup_status,
           registry.n_number, registry.manufacturer_serial_raw,
           registry.manufacturer_serial_key, registry.aircraft_code,
           registry.engine_code, registry.year_manufactured,
           registry.source_record_sha256,
           aircraft.aircraft_code AS reference_aircraft_code,
           aircraft.manufacturer_name AS aircraft_manufacturer_name,
           aircraft.model_name AS aircraft_model_name,
           aircraft.aircraft_type_code,
           aircraft.engine_type_code AS aircraft_engine_type_code,
           aircraft.category_code, aircraft.certification_indicator_code,
           aircraft.engine_count, aircraft.seat_count, aircraft.weight_class_code,
           aircraft.cruise_speed_mph, aircraft.type_certificate_data_sheet,
           aircraft.type_certificate_holder,
           engine.engine_code AS reference_engine_code,
           engine.manufacturer_name AS engine_manufacturer_name,
           engine.model_name AS engine_model_name, engine.engine_type_code,
           engine.horsepower, engine.thrust_pounds
    FROM covering_snapshot covering
    JOIN faa_registry_snapshots snapshot ON snapshot.id = covering.id
    JOIN faa_registry_coverage coverage
      ON coverage.snapshot_id = snapshot.id AND coverage.n_number = ?
    LEFT JOIN faa_registry_aircraft registry
      ON registry.snapshot_id = snapshot.id AND registry.n_number = ?
    LEFT JOIN faa_registry_aircraft_references aircraft
      ON aircraft.snapshot_id = registry.snapshot_id
     AND aircraft.aircraft_code = registry.aircraft_code
    LEFT JOIN faa_registry_engine_references engine
      ON engine.snapshot_id = registry.snapshot_id
     AND engine.engine_code = registry.engine_code
"#;

const CURRENT_FAA_GROUNDING_POSTGRES: &str = r#"
    WITH latest_release AS (
      SELECT snapshot_date, archive_sha256
      FROM faa_registry_snapshots
      ORDER BY snapshot_date DESC, id DESC
      LIMIT 1
    ),
    covering_snapshot AS (
      SELECT snapshot.id
      FROM faa_registry_snapshots snapshot
      JOIN faa_registry_coverage coverage
        ON coverage.snapshot_id = snapshot.id AND coverage.n_number = $1
      JOIN latest_release latest
        ON latest.snapshot_date = snapshot.snapshot_date
       AND latest.archive_sha256 = snapshot.archive_sha256
      ORDER BY (
        SELECT count(*) FROM faa_registry_coverage target
        WHERE target.snapshot_id = snapshot.id
      ) DESC, snapshot.id DESC
      LIMIT 1
    )
    SELECT snapshot.id AS snapshot_id, snapshot.evidence_source_id,
           snapshot.snapshot_date, snapshot.source_url, snapshot.archive_sha256,
           snapshot.source_manifest_sha256, snapshot.target_set_sha256,
           coverage.lookup_status,
           registry.n_number, registry.manufacturer_serial_raw,
           registry.manufacturer_serial_key, registry.aircraft_code,
           registry.engine_code, registry.year_manufactured,
           registry.source_record_sha256,
           aircraft.aircraft_code AS reference_aircraft_code,
           aircraft.manufacturer_name AS aircraft_manufacturer_name,
           aircraft.model_name AS aircraft_model_name,
           aircraft.aircraft_type_code,
           aircraft.engine_type_code AS aircraft_engine_type_code,
           aircraft.category_code, aircraft.certification_indicator_code,
           aircraft.engine_count, aircraft.seat_count, aircraft.weight_class_code,
           aircraft.cruise_speed_mph, aircraft.type_certificate_data_sheet,
           aircraft.type_certificate_holder,
           engine.engine_code AS reference_engine_code,
           engine.manufacturer_name AS engine_manufacturer_name,
           engine.model_name AS engine_model_name, engine.engine_type_code,
           engine.horsepower, engine.thrust_pounds
    FROM covering_snapshot covering
    JOIN faa_registry_snapshots snapshot ON snapshot.id = covering.id
    JOIN faa_registry_coverage coverage
      ON coverage.snapshot_id = snapshot.id AND coverage.n_number = $2
    LEFT JOIN faa_registry_aircraft registry
      ON registry.snapshot_id = snapshot.id AND registry.n_number = $3
    LEFT JOIN faa_registry_aircraft_references aircraft
      ON aircraft.snapshot_id = registry.snapshot_id
     AND aircraft.aircraft_code = registry.aircraft_code
    LEFT JOIN faa_registry_engine_references engine
      ON engine.snapshot_id = registry.snapshot_id
     AND engine.engine_code = registry.engine_code
    FOR SHARE OF snapshot, coverage
"#;

async fn revalidate_faa_grounding_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &PersistReviewableAircraftHierarchy<'_>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let n_number = normalize_n_number(
        input
            .observation
            .registration_number
            .as_deref()
            .unwrap_or_default(),
    )
    .ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "validated observation no longer has a valid N-number".to_string(),
        )
    })?;
    let row = sqlx::query_as::<_, CurrentFaaGroundingRow>(CURRENT_FAA_GROUNDING_SQLITE)
        .bind(&n_number)
        .bind(&n_number)
        .bind(&n_number)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Faa(
                "newest FAA release no longer covers the validated N-number".to_string(),
            )
        })?;
    validate_current_faa_row(
        &row,
        input.grounding,
        input.observation.serial_number.as_deref(),
    )
}

async fn revalidate_faa_grounding_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &PersistReviewableAircraftHierarchy<'_>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let n_number = normalize_n_number(
        input
            .observation
            .registration_number
            .as_deref()
            .unwrap_or_default(),
    )
    .ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "validated observation no longer has a valid N-number".to_string(),
        )
    })?;
    let row = sqlx::query_as::<_, CurrentFaaGroundingRow>(CURRENT_FAA_GROUNDING_POSTGRES)
        .bind(&n_number)
        .bind(&n_number)
        .bind(&n_number)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Faa(
                "newest FAA release no longer covers the validated N-number".to_string(),
            )
        })?;
    validate_current_faa_row(
        &row,
        input.grounding,
        input.observation.serial_number.as_deref(),
    )
}

fn checked_u16(value: Option<i64>) -> Option<Option<u16>> {
    value.map(u16::try_from).transpose().ok()
}

fn checked_u32(value: Option<i64>) -> Option<Option<u32>> {
    value.map(u32::try_from).transpose().ok()
}

fn current_serial_match(observed: Option<&str>, registry: Option<&str>) -> SerialMatch {
    let observed = trimmed(observed);
    let registry = trimmed(registry);
    let Some(observed) = observed else {
        return SerialMatch::NotProvided;
    };
    let Some(registry) = registry else {
        return SerialMatch::RegistryUnavailable;
    };
    if observed == registry {
        return SerialMatch::RawExact;
    }
    if normalize_serial_key(observed)
        .is_some_and(|left| normalize_serial_key(registry).is_some_and(|right| left == right))
    {
        SerialMatch::NormalizedOnly
    } else {
        SerialMatch::Conflict
    }
}

fn validate_current_faa_row(
    row: &CurrentFaaGroundingRow,
    expected: &AircraftGrounding,
    observed_serial: Option<&str>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let aircraft = expected.aircraft.as_ref();
    let engine = expected.engine.as_ref();
    let exact = row.snapshot_id == expected.snapshot.id
        && row.evidence_source_id == expected.snapshot.evidence_source_id
        && row.snapshot_date == expected.snapshot.snapshot_date
        && row.source_url == expected.snapshot.source_url
        && row.archive_sha256 == expected.snapshot.archive_sha256
        && row.source_manifest_sha256 == expected.snapshot.source_manifest_sha256
        && row.target_set_sha256 == expected.snapshot.target_set_sha256
        && row.lookup_status == "matched"
        && row.n_number.as_deref() == Some(expected.n_number.as_str())
        && row.manufacturer_serial_raw == expected.manufacturer_serial_raw
        && row.manufacturer_serial_key == expected.manufacturer_serial_key
        && row.aircraft_code.as_deref() == Some(expected.aircraft_code.as_str())
        && row.engine_code == expected.engine_code
        && checked_u16(row.year_manufactured) == Some(expected.year_manufactured)
        && row.source_record_sha256.as_deref() == Some(expected.source_record_sha256.as_str())
        && row.reference_aircraft_code.as_deref()
            == aircraft.map(|value| value.aircraft_code.as_str())
        && row.aircraft_manufacturer_name
            == aircraft.and_then(|value| value.manufacturer_name.clone())
        && row.aircraft_model_name == aircraft.and_then(|value| value.model_name.clone())
        && row.aircraft_type_code == aircraft.and_then(|value| value.aircraft_type_code.clone())
        && row.aircraft_engine_type_code
            == aircraft.and_then(|value| value.engine_type_code.clone())
        && row.category_code == aircraft.and_then(|value| value.category_code.clone())
        && row.certification_indicator_code
            == aircraft.and_then(|value| value.certification_indicator_code.clone())
        && checked_u16(row.engine_count) == Some(aircraft.and_then(|value| value.engine_count))
        && checked_u16(row.seat_count) == Some(aircraft.and_then(|value| value.seat_count))
        && row.weight_class_code == aircraft.and_then(|value| value.weight_class_code.clone())
        && checked_u16(row.cruise_speed_mph)
            == Some(aircraft.and_then(|value| value.cruise_speed_mph))
        && row.type_certificate_data_sheet
            == aircraft.and_then(|value| value.type_certificate_data_sheet.clone())
        && row.type_certificate_holder
            == aircraft.and_then(|value| value.type_certificate_holder.clone())
        && row.reference_engine_code.as_deref() == engine.map(|value| value.engine_code.as_str())
        && row.engine_manufacturer_name == engine.and_then(|value| value.manufacturer_name.clone())
        && row.engine_model_name == engine.and_then(|value| value.model_name.clone())
        && row.engine_type_code == engine.and_then(|value| value.engine_type_code.clone())
        && checked_u32(row.horsepower) == Some(engine.and_then(|value| value.horsepower))
        && checked_u32(row.thrust_pounds) == Some(engine.and_then(|value| value.thrust_pounds))
        && current_serial_match(observed_serial, row.manufacturer_serial_raw.as_deref())
            == expected.serial_match;
    if !exact {
        return Err(AircraftHierarchyPersistenceError::Faa(
            "listing or newest FAA projection changed after hierarchy validation".to_string(),
        ));
    }
    Ok(())
}

fn prepare_request(
    input: &PersistReviewableAircraftHierarchy<'_>,
) -> Result<PreparedApproval, AircraftHierarchyPersistenceError> {
    if input.listing_id <= 0 || input.observation.listing_id != input.listing_id {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "persistence requires one positive listing id and its exact observation".to_string(),
        ));
    }
    if input.expected_catalog_revision.trim().is_empty() {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "expected catalog revision is required".to_string(),
        ));
    }
    let _exact_excerpt = input
        .observation
        .source_excerpt
        .as_deref()
        .filter(|excerpt| input.observation.source_excerpt_is_exact && !excerpt.trim().is_empty())
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(
                "catalog approval requires an exact retained-source observation".to_string(),
            )
        })?;
    if input.reviewable.adjudication.confidence != CurationConfidence::VeryHigh
        || input.reviewable.verification.verdict != VerificationVerdict::Confirm
        || input.reviewable.verification.confidence != CurationConfidence::VeryHigh
        || !input
            .reviewable
            .adjudication
            .unresolved_questions
            .is_empty()
        || !input.reviewable.verification.errors.is_empty()
    {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "only independently confirmed, very-high-confidence, fully resolved hierarchies may be persisted"
                .to_string(),
        ));
    }
    validate_aircraft_hierarchy_proposal(&input.reviewable.proposal).map_err(|errors| {
        AircraftHierarchyPersistenceError::Invalid(format!(
            "reviewable hierarchy no longer validates: {errors}"
        ))
    })?;
    input
        .reviewable
        .require_server_faa_observation_binding(
            input.listing_id,
            &input.observation.observation_sha256,
            input.observation.model_year,
            input.grounding,
        )
        .map_err(AircraftHierarchyPersistenceError::Faa)?;
    validate_decision_projection(input.reviewable)?;
    validate_family_label_projection(input)?;
    input
        .reviewable
        .require_tcds_family_relationship_binding(
            input.listing_id,
            input.observation,
            input.grounding,
        )
        .map_err(AircraftHierarchyPersistenceError::Faa)?;
    validate_faa_projection(input.reviewable, input.grounding)?;

    let all_claims_by_evidence_id = input
        .reviewable
        .proposal
        .evidence
        .iter()
        .map(|claim| (claim.evidence_id.trim().to_string(), claim.clone()))
        .collect::<BTreeMap<_, _>>();
    let used_evidence_ids = decision_evidence_ids(input.reviewable);
    for evidence_id in &used_evidence_ids {
        if !all_claims_by_evidence_id.contains_key(*evidence_id) {
            return Err(AircraftHierarchyPersistenceError::Invalid(format!(
                "decision references evidence id {evidence_id} absent from the verified proposal"
            )));
        }
    }
    let claims_by_evidence_id = all_claims_by_evidence_id
        .into_iter()
        .filter(|(evidence_id, _)| used_evidence_ids.contains(evidence_id.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut source_content_sha256_by_evidence_id = BTreeMap::new();
    for (evidence_id, claim) in &claims_by_evidence_id {
        if input
            .reviewable
            .is_exact_server_faa_claim(evidence_id, claim)
        {
            if is_server_faa_drs_evidence_id(evidence_id) {
                let content_sha256 = input
                    .reviewable
                    .server_evidence_source_content_sha256(evidence_id)
                    .filter(|digest| is_lower_hex_sha256(digest))
                    .ok_or_else(|| {
                        AircraftHierarchyPersistenceError::Faa(format!(
                            "exact server FAA DRS evidence {evidence_id} has no valid PDF digest"
                        ))
                    })?;
                source_content_sha256_by_evidence_id
                    .insert(evidence_id.clone(), content_sha256.to_string());
            }
            continue;
        }
        if is_reserved_server_faa_evidence_id(evidence_id) {
            return Err(AircraftHierarchyPersistenceError::Faa(format!(
                "reserved server FAA evidence {evidence_id} is not an exact claim for this case"
            )));
        }
        let content_sha256 = input
            .reviewable
            .require_direct_source_claim_proof(evidence_id, claim)
            .map_err(AircraftHierarchyPersistenceError::Invalid)?;
        source_content_sha256_by_evidence_id
            .insert(evidence_id.clone(), content_sha256.to_string());
    }

    let used_evidence = claims_by_evidence_id
        .values()
        .map(|claim| {
            json!({
                "evidence_id": claim.evidence_id,
                "source_url": claim.source_url,
                "evidence_excerpt": claim.evidence_excerpt,
                "source_kind": claim.source_kind,
                "supports": claim.supports,
            })
        })
        .collect::<Vec<_>>();
    let verified_used_evidence_ids = input
        .reviewable
        .verification
        .verified_evidence_ids
        .iter()
        .filter(|evidence_id| used_evidence_ids.contains(evidence_id.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    let fingerprint_payload = json!({
        "version": PERSISTENCE_VERSION,
        "expected_catalog_revision": input.expected_catalog_revision,
        "proposal": {
            "manufacturer": input.reviewable.proposal.manufacturer,
            "model_family": input.reviewable.proposal.model_family,
            "certified_variant": input.reviewable.proposal.certified_variant,
            "generation": input.reviewable.proposal.generation,
            "tier": input.reviewable.proposal.tier,
            "used_evidence": used_evidence,
        },
        "adjudication": input.reviewable.adjudication,
        "verification": {
            "verdict": input.reviewable.verification.verdict,
            "confidence": input.reviewable.verification.confidence,
            "verified_used_evidence_ids": verified_used_evidence_ids,
            "differentiation_checks": input.reviewable.verification.differentiation_checks,
            "errors": input.reviewable.verification.errors,
            "rationale": input.reviewable.verification.rationale,
        },
        "direct_source_proofs": input.reviewable.direct_source_proofs,
        "used_source_content_sha256": source_content_sha256_by_evidence_id,
    });
    let canonical_payload = serde_json::to_vec(&fingerprint_payload).map_err(|error| {
        AircraftHierarchyPersistenceError::Invalid(format!(
            "could not serialize hierarchy approval: {error}"
        ))
    })?;
    let approval_fingerprint = format!("sha256:{:x}", Sha256::digest(&canonical_payload));
    let payload_json = serde_json::to_string(&fingerprint_payload).map_err(|error| {
        AircraftHierarchyPersistenceError::Invalid(format!(
            "could not serialize hierarchy approval: {error}"
        ))
    })?;
    let validation_json = serde_json::to_string(&json!({
        "passed": true,
        "persistence_version": PERSISTENCE_VERSION,
        "approval_fingerprint": approval_fingerprint,
        "expected_catalog_revision": input.expected_catalog_revision,
        "case_validation": "independently_reviewable_hierarchy",
    }))
    .expect("deterministic validation payload serializes");

    Ok(PreparedApproval {
        approval_fingerprint,
        payload_json,
        validation_json,
        claims_by_evidence_id,
        source_content_sha256_by_evidence_id,
    })
}

fn validate_decision_projection(
    reviewable: &ReviewableAircraftHierarchy,
) -> Result<(), AircraftHierarchyPersistenceError> {
    validate_entity_projection(
        "make",
        &reviewable.adjudication.make,
        Some(&reviewable.proposal.manufacturer),
        false,
    )?;
    validate_entity_projection(
        "family",
        &reviewable.adjudication.family,
        Some(&reviewable.proposal.model_family),
        false,
    )?;
    validate_entity_projection(
        "designation",
        &reviewable.adjudication.designation,
        Some(&reviewable.proposal.certified_variant),
        false,
    )?;
    validate_entity_projection(
        "generation",
        &reviewable.adjudication.generation,
        reviewable.proposal.generation.as_ref(),
        true,
    )?;
    validate_entity_projection(
        "package",
        &reviewable.adjudication.package,
        reviewable.proposal.tier.as_ref(),
        true,
    )
}

fn validate_family_label_projection(
    input: &PersistReviewableAircraftHierarchy<'_>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relationship = &input.reviewable.adjudication.family_label_relationship;
    let observed_family = input.observation.model.trim();
    let canonical_family = input.reviewable.proposal.model_family.display_name.trim();
    if relationship.observed_family_label.trim() != observed_family {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "family-label relationship does not preserve the exact retained model/family label"
                .to_string(),
        ));
    }
    if relationship.canonical_family_name.trim() != canonical_family {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "family-label relationship does not target the validated canonical family".to_string(),
        ));
    }
    match relationship.action {
        FamilyLabelRelationshipAction::ExactCanonicalLabel => {
            if observed_family != canonical_family
                || relationship.existing_alias_id.is_some()
                || relationship.valid_from_model_year.is_some()
                || relationship.valid_to_model_year.is_some()
                || !relationship.evidence_ids.is_empty()
                || !relationship.applicability_evidence_ids.is_empty()
            {
                return Err(AircraftHierarchyPersistenceError::Invalid(
                    "exact family-label relationship must have equal labels and no alias fields or evidence"
                        .to_string(),
                ));
            }
        }
        FamilyLabelRelationshipAction::MatchManufacturerSeriesFamily => {
            if observed_family == canonical_family
                || relationship.existing_alias_id.is_some()
                || relationship.valid_from_model_year.is_some()
                || relationship.valid_to_model_year.is_some()
                || relationship.evidence_ids.is_empty()
                || !relationship.applicability_evidence_ids.is_empty()
            {
                return Err(AircraftHierarchyPersistenceError::Invalid(
                    "manufacturer series/family matching requires different retained/canonical labels, relationship evidence, and no alias id, model-year bounds, or alias-applicability evidence"
                        .to_string(),
                ));
            }
        }
        FamilyLabelRelationshipAction::MatchApprovedAlias => {
            if observed_family == canonical_family || relationship.existing_alias_id.is_none() {
                return Err(AircraftHierarchyPersistenceError::Invalid(
                    "matched family-label alias requires different labels and an exact alias id"
                        .to_string(),
                ));
            }
        }
        FamilyLabelRelationshipAction::ProposeAlias => {
            if observed_family == canonical_family
                || relationship.existing_alias_id.is_some()
                || relationship.evidence_ids.is_empty()
                || relationship.applicability_evidence_ids.is_empty()
            {
                return Err(AircraftHierarchyPersistenceError::Invalid(
                    "new family-label alias requires different labels, separate identity and applicability evidence, and no existing id"
                        .to_string(),
                ));
            }
            let has_primary_claim = |evidence_ids: &[String], required_kind: EvidenceClaimKind| {
                evidence_ids.iter().any(|evidence_id| {
                    !is_reserved_server_faa_evidence_id(evidence_id)
                        && input.reviewable.proposal.evidence.iter().any(|claim| {
                            claim.evidence_id == *evidence_id
                                && matches!(
                                    claim.source_kind,
                                    EvidenceSourceKind::Manufacturer
                                        | EvidenceSourceKind::ManufacturerServicePublication
                                )
                                && claim.supports.contains(&required_kind)
                        })
                })
            };
            if !has_primary_claim(
                &relationship.evidence_ids,
                EvidenceClaimKind::HierarchyIdentity,
            ) || !has_primary_claim(
                &relationship.applicability_evidence_ids,
                EvidenceClaimKind::ProductionApplicability,
            ) {
                return Err(AircraftHierarchyPersistenceError::Invalid(
                    "new family-label alias lacks classified manufacturer-primary identity or production-applicability evidence"
                        .to_string(),
                ));
            }
        }
        FamilyLabelRelationshipAction::MatchFaaTypeCertificateFamily => {
            if observed_family == canonical_family
                || relationship.existing_alias_id.is_some()
                || relationship.valid_from_model_year.is_some()
                || relationship.valid_to_model_year.is_some()
                || relationship.evidence_ids.is_empty()
                || !relationship.applicability_evidence_ids.is_empty()
            {
                return Err(AircraftHierarchyPersistenceError::Invalid(
                    "FAA type-certificate family matching requires different retained/canonical labels, exact server evidence, and no alias id, model-year bounds, or alias-applicability evidence"
                        .to_string(),
                ));
            }
        }
        FamilyLabelRelationshipAction::Unresolved => {
            return Err(AircraftHierarchyPersistenceError::Invalid(
                "retained family label relationship is unresolved".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_entity_projection(
    label: &str,
    decision: &super::CatalogEntityDecision,
    proposal: Option<&CatalogEntityProposal>,
    optional: bool,
) -> Result<(), AircraftHierarchyPersistenceError> {
    match (decision.action, proposal) {
        (EntityResolutionAction::MatchExisting, Some(proposal)) => {
            if decision.existing_catalog_id != proposal.existing_catalog_id
                || decision.existing_catalog_id.is_none()
                || decision.display_name.as_deref().map(str::trim)
                    != Some(proposal.display_name.trim())
                || decision.authoritative_designator.as_deref().map(str::trim)
                    != proposal.authoritative_designator.as_deref().map(str::trim)
            {
                return Err(AircraftHierarchyPersistenceError::Invalid(format!(
                    "{label} MatchExisting decision no longer exactly equals its validated proposal"
                )));
            }
        }
        (EntityResolutionAction::ProposeNew, Some(proposal)) => {
            if decision.existing_catalog_id.is_some()
                || proposal.existing_catalog_id.is_some()
                || decision.display_name.as_deref().map(str::trim)
                    != Some(proposal.display_name.trim())
                || decision.authoritative_designator.as_deref().map(str::trim)
                    != proposal.authoritative_designator.as_deref().map(str::trim)
            {
                return Err(AircraftHierarchyPersistenceError::Invalid(format!(
                    "{label} ProposeNew decision no longer exactly equals its validated proposal"
                )));
            }
        }
        (EntityResolutionAction::NoSupportedSelection, None) if optional => {
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
                return Err(AircraftHierarchyPersistenceError::Invalid(format!(
                    "{label} NoSupportedSelection decision carries entity fields or evidence"
                )));
            }
        }
        _ => {
            return Err(AircraftHierarchyPersistenceError::Invalid(format!(
                "{label} decision/proposal state is not persistable"
            )));
        }
    }
    Ok(())
}

fn validate_faa_projection(
    reviewable: &ReviewableAircraftHierarchy,
    grounding: &AircraftGrounding,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let reference = grounding.aircraft.as_ref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "FAA grounding has no aircraft reference identity".to_string(),
        )
    })?;
    let faa_make = reference
        .manufacturer_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Faa(
                "FAA grounding has no manufacturer identity".to_string(),
            )
        })?;
    let faa_model = reference
        .model_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Faa(
                "FAA grounding has no model designation".to_string(),
            )
        })?;
    let relationship = &reviewable.adjudication.faa_make_relationship;
    if relationship.faa_manufacturer_name.trim() != faa_make
        || relationship.canonical_make_name.trim()
            != reviewable.proposal.manufacturer.display_name.trim()
    {
        return Err(AircraftHierarchyPersistenceError::Faa(
            "validated FAA make relationship does not equal the current FAA/canonical labels"
                .to_string(),
        ));
    }
    if reviewable
        .proposal
        .certified_variant
        .authoritative_designator
        .as_deref()
        .map(str::trim)
        != Some(faa_model)
    {
        return Err(AircraftHierarchyPersistenceError::Faa(
            "validated designation does not exactly identify the current FAA model".to_string(),
        ));
    }
    match relationship.action {
        FaaMakeRelationshipAction::ExactCanonicalLabel
            if faa_make == reviewable.proposal.manufacturer.display_name.trim() => {}
        FaaMakeRelationshipAction::ExactCanonicalLabel => {
            return Err(AircraftHierarchyPersistenceError::Faa(
                "exact FAA make action was selected for different make labels".to_string(),
            ));
        }
        FaaMakeRelationshipAction::MatchApprovedAlias | FaaMakeRelationshipAction::ProposeAlias => {
        }
        FaaMakeRelationshipAction::MatchTcdsMakeLineage => {
            if prepare_tcds_make_lineage(reviewable, grounding)?.is_none() {
                return Err(AircraftHierarchyPersistenceError::Faa(
                    "FAA TCDS make-lineage action has no exact prepared binding".to_string(),
                ));
            }
        }
        FaaMakeRelationshipAction::Unresolved => {
            return Err(AircraftHierarchyPersistenceError::Faa(
                "FAA legal make relationship is unresolved".to_string(),
            ));
        }
    }
    Ok(())
}

fn decision_evidence_ids(reviewable: &ReviewableAircraftHierarchy) -> BTreeSet<&str> {
    [
        &reviewable.adjudication.make,
        &reviewable.adjudication.family,
        &reviewable.adjudication.designation,
        &reviewable.adjudication.generation,
        &reviewable.adjudication.package,
    ]
    .into_iter()
    .flat_map(|decision| decision.evidence_ids.iter().map(String::as_str))
    .chain(
        reviewable
            .adjudication
            .faa_make_relationship
            .evidence_ids
            .iter()
            .map(String::as_str),
    )
    .chain(
        reviewable
            .adjudication
            .faa_make_relationship
            .applicability_evidence_ids
            .iter()
            .map(String::as_str),
    )
    .chain(
        reviewable
            .adjudication
            .family_label_relationship
            .evidence_ids
            .iter()
            .map(String::as_str),
    )
    .chain(
        reviewable
            .adjudication
            .family_label_relationship
            .applicability_evidence_ids
            .iter()
            .map(String::as_str),
    )
    .collect()
}

fn observation_legacy_hint(observation: &AircraftIdentityObservation) -> String {
    serde_json::to_string(&json!({
        "source_kind": observation.source_kind,
        "submission_id": observation.submission_id,
        "rendered_html_sha256": observation.rendered_html_sha256,
        "cluster_key": observation.cluster_key,
        "requires_human_review": observation.requires_human_review,
        "review_reasons": observation.review_reasons,
        "literal_fields": {
            "manufacturer": observation.manufacturer,
            "model": observation.model,
            "variant": observation.variant,
            "model_year": observation.model_year,
            "serial_number": observation.serial_number,
            "registration_number": observation.registration_number,
        }
    }))
    .expect("observation persistence payload serializes")
}

async fn stage_observation_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    observation: &AircraftIdentityObservation,
) -> Result<i64, AircraftHierarchyPersistenceError> {
    let exact_source_evidence = observation.source_excerpt.as_deref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Invalid(
            "exact observation excerpt disappeared before persistence".to_string(),
        )
    })?;
    let legacy_hint_json = observation_legacy_hint(observation);
    sqlx::query(
        r#"
        INSERT INTO aircraft_identity_observations (
          aircraft_sale_listing_id, source_url, observed_make, observed_family,
          observed_designation, observed_generation, observed_package,
          model_year, serial_number, registration_number, market_code,
          exact_source_evidence, observation_sha256, legacy_hint_json
        ) VALUES (?, ?, ?, ?, ?, NULL, NULL, ?, ?, ?, 'US', ?, ?, ?)
        ON CONFLICT (observation_sha256) DO NOTHING
        "#,
    )
    .bind(observation.listing_id)
    .bind(observation.source_url.as_deref())
    .bind(&observation.manufacturer)
    .bind(&observation.model)
    .bind(&observation.variant)
    .bind(observation.model_year)
    .bind(observation.serial_number.as_deref())
    .bind(observation.registration_number.as_deref())
    .bind(exact_source_evidence)
    .bind(&observation.observation_sha256)
    .bind(&legacy_hint_json)
    .execute(&mut **transaction)
    .await?;
    let row = sqlx::query_as::<_, ObservationRow>(
        r#"
        SELECT id, aircraft_sale_listing_id, source_url, observed_make,
               observed_family, observed_designation, model_year, serial_number,
               registration_number, exact_source_evidence, observation_sha256
        FROM aircraft_identity_observations
        WHERE observation_sha256 = ?
        "#,
    )
    .bind(&observation.observation_sha256)
    .fetch_one(&mut **transaction)
    .await?;
    validate_staged_observation(&row, observation)?;
    Ok(row.id)
}

async fn stage_observation_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    observation: &AircraftIdentityObservation,
) -> Result<i64, AircraftHierarchyPersistenceError> {
    let exact_source_evidence = observation.source_excerpt.as_deref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Invalid(
            "exact observation excerpt disappeared before persistence".to_string(),
        )
    })?;
    let legacy_hint_json = observation_legacy_hint(observation);
    sqlx::query(
        r#"
        INSERT INTO aircraft_identity_observations (
          aircraft_sale_listing_id, source_url, observed_make, observed_family,
          observed_designation, observed_generation, observed_package,
          model_year, serial_number, registration_number, market_code,
          exact_source_evidence, observation_sha256, legacy_hint_json
        ) VALUES ($1, $2, $3, $4, $5, NULL, NULL, $6, $7, $8, 'US', $9, $10, $11)
        ON CONFLICT (observation_sha256) DO NOTHING
        "#,
    )
    .bind(observation.listing_id)
    .bind(observation.source_url.as_deref())
    .bind(&observation.manufacturer)
    .bind(&observation.model)
    .bind(&observation.variant)
    .bind(observation.model_year)
    .bind(observation.serial_number.as_deref())
    .bind(observation.registration_number.as_deref())
    .bind(exact_source_evidence)
    .bind(&observation.observation_sha256)
    .bind(&legacy_hint_json)
    .execute(&mut **transaction)
    .await?;
    let row = sqlx::query_as::<_, ObservationRow>(
        r#"
        SELECT id, aircraft_sale_listing_id, source_url, observed_make,
               observed_family, observed_designation, model_year, serial_number,
               registration_number, exact_source_evidence, observation_sha256
        FROM aircraft_identity_observations
        WHERE observation_sha256 = $1
        "#,
    )
    .bind(&observation.observation_sha256)
    .fetch_one(&mut **transaction)
    .await?;
    validate_staged_observation(&row, observation)?;
    Ok(row.id)
}

fn validate_staged_observation(
    row: &ObservationRow,
    expected: &AircraftIdentityObservation,
) -> Result<(), AircraftHierarchyPersistenceError> {
    if row.aircraft_sale_listing_id != Some(expected.listing_id)
        || row.source_url.as_deref() != expected.source_url.as_deref()
        || row.observed_make.as_deref() != Some(expected.manufacturer.as_str())
        || row.observed_family.as_deref() != Some(expected.model.as_str())
        || row.observed_designation.as_deref() != Some(expected.variant.as_str())
        || row.model_year != Some(expected.model_year)
        || row.serial_number.as_deref() != expected.serial_number.as_deref()
        || row.registration_number.as_deref() != expected.registration_number.as_deref()
        || row.exact_source_evidence != expected.source_excerpt.as_deref().unwrap_or_default()
        || row.observation_sha256 != expected.observation_sha256
    {
        return Err(AircraftHierarchyPersistenceError::Collision(
            "observation fingerprint collision: stored provenance differs from the validated case"
                .to_string(),
        ));
    }
    Ok(())
}

fn decision_fingerprint(prepared: &PreparedApproval, suffix: &str) -> String {
    format!(
        "{PERSISTENCE_VERSION}:{}:{suffix}",
        prepared.approval_fingerprint
    )
}

fn source_tier(kind: EvidenceSourceKind) -> &'static str {
    match kind {
        EvidenceSourceKind::Manufacturer
        | EvidenceSourceKind::ApprovedFlightManual
        | EvidenceSourceKind::ManufacturerServicePublication => "manufacturer_primary",
        EvidenceSourceKind::Regulator | EvidenceSourceKind::TypeCertificate => "regulator_primary",
        EvidenceSourceKind::RecognizedSecondary => "recognized_secondary",
        EvidenceSourceKind::MarketplaceListing => "marketplace_observation",
    }
}

fn claim_kind(claim: &EvidenceClaimProposal) -> &'static str {
    if claim
        .supports
        .contains(&EvidenceClaimKind::HierarchyIdentity)
    {
        "identity"
    } else if claim
        .supports
        .contains(&EvidenceClaimKind::ProductionApplicability)
        || claim
            .supports
            .contains(&EvidenceClaimKind::SerialApplicability)
        || claim
            .supports
            .contains(&EvidenceClaimKind::MarketApplicability)
    {
        "applicability"
    } else {
        "other"
    }
}

fn evidence_role(claim: &EvidenceClaimProposal) -> &'static str {
    if claim
        .supports
        .contains(&EvidenceClaimKind::ProductionApplicability)
        || claim
            .supports
            .contains(&EvidenceClaimKind::SerialApplicability)
        || claim
            .supports
            .contains(&EvidenceClaimKind::MarketApplicability)
    {
        "applicability"
    } else {
        "identity"
    }
}

fn evidence_source_domain(source_url: &str) -> Result<String, AircraftHierarchyPersistenceError> {
    Url::parse(source_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .filter(|host| !host.trim().is_empty())
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(format!(
                "verified evidence URL has no valid host: {source_url}"
            ))
        })
}

fn is_server_faa_registry_evidence_id(evidence_id: &str) -> bool {
    evidence_id.starts_with(SERVER_FAA_REGISTRY_EVIDENCE_PREFIX)
}

fn is_server_faa_drs_evidence_id(evidence_id: &str) -> bool {
    evidence_id.starts_with(SERVER_FAA_DRS_EVIDENCE_PREFIX)
}

fn is_reserved_server_faa_evidence_id(evidence_id: &str) -> bool {
    is_server_faa_registry_evidence_id(evidence_id) || is_server_faa_drs_evidence_id(evidence_id)
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_official_faa_drs_download_url(source_url: &str) -> bool {
    let Ok(url) = Url::parse(source_url) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("drs.faa.gov")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(document_guid) = url
        .path()
        .strip_prefix("/api/drs/data-pull/download/")
        .filter(|value| !value.is_empty() && !value.contains('/'))
    else {
        return false;
    };
    document_guid.len() == 36
        && document_guid
            .bytes()
            .enumerate()
            .all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            })
}

fn claim_object_json(claim: &EvidenceClaimProposal) -> String {
    serde_json::to_string(&json!({
        "evidence_id": claim.evidence_id,
        "supports": claim.supports,
    }))
    .expect("evidence claim object serializes")
}

const CATALOG_BASE_SQLITE: &str = r#"
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

const CATALOG_BASE_POSTGRES: &str = r#"
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
    SELECT 'generation'::TEXT, id, aircraft_model_family_id, name, NULL::TEXT,
           normalized_name
    FROM aircraft_generations
    UNION ALL
    SELECT 'package'::TEXT, id, aircraft_model_family_id, name, NULL::TEXT,
           normalized_name
    FROM aircraft_factory_packages
    ORDER BY entity_kind, entity_id
"#;

const CATALOG_LOOKUP_SQLITE: &str = r#"
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
           identifier_value, normalized_identifier_value, NULL, NULL, NULL
    FROM aircraft_designation_identifiers
    ORDER BY entity_kind, entity_id, lookup_kind, normalized_value, lookup_id
"#;

const CATALOG_LOOKUP_POSTGRES: &str = r#"
    SELECT 'make'::TEXT AS entity_kind, alias.aircraft_make_id AS entity_id,
           'alias'::TEXT AS lookup_kind, alias.id AS lookup_id,
           alias.alias AS display_value,
           alias.normalized_alias AS normalized_value,
           alias.valid_from_model_year, alias.valid_to_model_year,
           market.code AS market_code
    FROM aircraft_make_aliases alias
    LEFT JOIN aircraft_markets market ON market.id = alias.aircraft_market_id
    UNION ALL
    SELECT 'family'::TEXT, alias.aircraft_model_family_id, 'alias'::TEXT, alias.id,
           alias.alias, alias.normalized_alias,
           alias.valid_from_model_year, alias.valid_to_model_year,
           market.code
    FROM aircraft_family_aliases alias
    LEFT JOIN aircraft_markets market ON market.id = alias.aircraft_market_id
    UNION ALL
    SELECT 'designation'::TEXT, alias.aircraft_designation_id, 'alias'::TEXT, alias.id,
           alias.alias, alias.normalized_alias,
           alias.valid_from_model_year, alias.valid_to_model_year,
           market.code
    FROM aircraft_designation_aliases alias
    LEFT JOIN aircraft_markets market ON market.id = alias.aircraft_market_id
    UNION ALL
    SELECT 'designation'::TEXT, aircraft_designation_id, 'identifier'::TEXT, id,
           identifier_value, normalized_identifier_value,
           NULL::BIGINT, NULL::BIGINT, NULL::TEXT
    FROM aircraft_designation_identifiers
    ORDER BY entity_kind, entity_id, lookup_kind, normalized_value, lookup_id
"#;

const CATALOG_GENERATION_DESIGNATIONS: &str = r#"
    SELECT aircraft_generation_id, aircraft_designation_id
    FROM aircraft_generation_designations
    ORDER BY aircraft_generation_id, aircraft_designation_id
"#;

const CATALOG_PACKAGE_APPLICABILITY: &str = r#"
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

fn hash_catalog_revision(
    base: &[CatalogBaseFingerprintRow],
    lookup: &[CatalogLookupFingerprintRow],
    generation_designations: &[CatalogGenerationDesignationFingerprintRow],
    package_applicability: &[CatalogPackageApplicabilityFingerprintRow],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        serde_json::to_vec(&(base, lookup, generation_designations, package_applicability))
            .expect("catalog rows serialize"),
    );
    format!("sha256:{:x}", hasher.finalize())
}

async fn catalog_revision_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<String, sqlx::Error> {
    let base = sqlx::query_as::<_, CatalogBaseFingerprintRow>(CATALOG_BASE_SQLITE)
        .fetch_all(&mut **transaction)
        .await?;
    let lookup = sqlx::query_as::<_, CatalogLookupFingerprintRow>(CATALOG_LOOKUP_SQLITE)
        .fetch_all(&mut **transaction)
        .await?;
    let generation_designations = sqlx::query_as::<_, CatalogGenerationDesignationFingerprintRow>(
        CATALOG_GENERATION_DESIGNATIONS,
    )
    .fetch_all(&mut **transaction)
    .await?;
    let package_applicability = sqlx::query_as::<_, CatalogPackageApplicabilityFingerprintRow>(
        CATALOG_PACKAGE_APPLICABILITY,
    )
    .fetch_all(&mut **transaction)
    .await?;
    Ok(hash_catalog_revision(
        &base,
        &lookup,
        &generation_designations,
        &package_applicability,
    ))
}

async fn catalog_revision_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<String, sqlx::Error> {
    let base = sqlx::query_as::<_, CatalogBaseFingerprintRow>(CATALOG_BASE_POSTGRES)
        .fetch_all(&mut **transaction)
        .await?;
    let lookup = sqlx::query_as::<_, CatalogLookupFingerprintRow>(CATALOG_LOOKUP_POSTGRES)
        .fetch_all(&mut **transaction)
        .await?;
    let generation_designations = sqlx::query_as::<_, CatalogGenerationDesignationFingerprintRow>(
        CATALOG_GENERATION_DESIGNATIONS,
    )
    .fetch_all(&mut **transaction)
    .await?;
    let package_applicability = sqlx::query_as::<_, CatalogPackageApplicabilityFingerprintRow>(
        CATALOG_PACKAGE_APPLICABILITY,
    )
    .fetch_all(&mut **transaction)
    .await?;
    Ok(hash_catalog_revision(
        &base,
        &lookup,
        &generation_designations,
        &package_applicability,
    ))
}

async fn persist_claims_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    prepared: &PreparedApproval,
    grounding: &AircraftGrounding,
) -> Result<BTreeMap<String, i64>, AircraftHierarchyPersistenceError> {
    let mut ids = BTreeMap::new();
    for (evidence_id, claim) in &prepared.claims_by_evidence_id {
        let source_id = if is_server_faa_registry_evidence_id(evidence_id) {
            if claim.source_kind != EvidenceSourceKind::Regulator
                || claim.source_url != grounding.snapshot.source_url
            {
                return Err(AircraftHierarchyPersistenceError::Faa(format!(
                    "server FAA evidence {evidence_id} is not bound to the current imported snapshot"
                )));
            }
            grounding.snapshot.evidence_source_id
        } else if is_server_faa_drs_evidence_id(evidence_id) {
            if claim.source_kind != EvidenceSourceKind::TypeCertificate
                || !is_official_faa_drs_download_url(&claim.source_url)
            {
                return Err(AircraftHierarchyPersistenceError::Faa(format!(
                    "server FAA DRS evidence {evidence_id} is not bound to an official type-certificate download"
                )));
            }
            let content_sha256 = prepared
                .source_content_sha256_by_evidence_id
                .get(evidence_id)
                .filter(|digest| is_lower_hex_sha256(digest))
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Faa(format!(
                        "server FAA DRS evidence {evidence_id} has no prepared PDF digest"
                    ))
                })?;
            find_or_insert_web_source_sqlite(transaction, claim, content_sha256).await?
        } else {
            let content_sha256 = prepared
                .source_content_sha256_by_evidence_id
                .get(evidence_id)
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Invalid(format!(
                        "used web evidence {evidence_id} has no prepared content digest"
                    ))
                })?;
            find_or_insert_web_source_sqlite(transaction, claim, content_sha256).await?
        };
        let object = claim_object_json(claim);
        let kind = claim_kind(claim);
        let predicate = "supports verified aircraft hierarchy decision";
        let existing = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT id
            FROM curation_evidence_claims
            WHERE evidence_source_id = ? AND claim_kind = ?
              AND subject_text = 'aircraft_model_hierarchy'
              AND predicate_text = ? AND object_text = ?
              AND quoted_evidence = ? AND validation_status = 'validated'
            ORDER BY id LIMIT 1
            "#,
        )
        .bind(source_id)
        .bind(kind)
        .bind(predicate)
        .bind(&object)
        .bind(&claim.evidence_excerpt)
        .fetch_optional(&mut **transaction)
        .await?;
        let claim_id = if let Some(id) = existing {
            id
        } else {
            sqlx::query_scalar::<_, i64>(
                r#"
                INSERT INTO curation_evidence_claims (
                  evidence_source_id, claim_kind, subject_text, predicate_text,
                  object_text, quoted_evidence, validation_status, validated_at
                ) VALUES (
                  ?, ?, 'aircraft_model_hierarchy', ?, ?, ?,
                  'validated', CURRENT_TIMESTAMP
                ) RETURNING id
                "#,
            )
            .bind(source_id)
            .bind(kind)
            .bind(predicate)
            .bind(&object)
            .bind(&claim.evidence_excerpt)
            .fetch_one(&mut **transaction)
            .await?
        };
        ids.insert(evidence_id.clone(), claim_id);
    }
    Ok(ids)
}

async fn persist_claims_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    prepared: &PreparedApproval,
    grounding: &AircraftGrounding,
) -> Result<BTreeMap<String, i64>, AircraftHierarchyPersistenceError> {
    let mut ids = BTreeMap::new();
    for (evidence_id, claim) in &prepared.claims_by_evidence_id {
        let source_id = if is_server_faa_registry_evidence_id(evidence_id) {
            if claim.source_kind != EvidenceSourceKind::Regulator
                || claim.source_url != grounding.snapshot.source_url
            {
                return Err(AircraftHierarchyPersistenceError::Faa(format!(
                    "server FAA evidence {evidence_id} is not bound to the current imported snapshot"
                )));
            }
            grounding.snapshot.evidence_source_id
        } else if is_server_faa_drs_evidence_id(evidence_id) {
            if claim.source_kind != EvidenceSourceKind::TypeCertificate
                || !is_official_faa_drs_download_url(&claim.source_url)
            {
                return Err(AircraftHierarchyPersistenceError::Faa(format!(
                    "server FAA DRS evidence {evidence_id} is not bound to an official type-certificate download"
                )));
            }
            let content_sha256 = prepared
                .source_content_sha256_by_evidence_id
                .get(evidence_id)
                .filter(|digest| is_lower_hex_sha256(digest))
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Faa(format!(
                        "server FAA DRS evidence {evidence_id} has no prepared PDF digest"
                    ))
                })?;
            find_or_insert_web_source_postgres(transaction, claim, content_sha256).await?
        } else {
            let content_sha256 = prepared
                .source_content_sha256_by_evidence_id
                .get(evidence_id)
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Invalid(format!(
                        "used web evidence {evidence_id} has no prepared content digest"
                    ))
                })?;
            find_or_insert_web_source_postgres(transaction, claim, content_sha256).await?
        };
        let object = claim_object_json(claim);
        let kind = claim_kind(claim);
        let predicate = "supports verified aircraft hierarchy decision";
        let existing = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT id
            FROM curation_evidence_claims
            WHERE evidence_source_id = $1 AND claim_kind = $2
              AND subject_text = 'aircraft_model_hierarchy'
              AND predicate_text = $3 AND object_text = $4
              AND quoted_evidence = $5 AND validation_status = 'validated'
            ORDER BY id LIMIT 1
            "#,
        )
        .bind(source_id)
        .bind(kind)
        .bind(predicate)
        .bind(&object)
        .bind(&claim.evidence_excerpt)
        .fetch_optional(&mut **transaction)
        .await?;
        let claim_id = if let Some(id) = existing {
            id
        } else {
            sqlx::query_scalar::<_, i64>(
                r#"
                INSERT INTO curation_evidence_claims (
                  evidence_source_id, claim_kind, subject_text, predicate_text,
                  object_text, quoted_evidence, validation_status, validated_at
                ) VALUES (
                  $1, $2, 'aircraft_model_hierarchy', $3, $4, $5,
                  'validated', CURRENT_TIMESTAMP
                ) RETURNING id
                "#,
            )
            .bind(source_id)
            .bind(kind)
            .bind(predicate)
            .bind(&object)
            .bind(&claim.evidence_excerpt)
            .fetch_one(&mut **transaction)
            .await?
        };
        ids.insert(evidence_id.clone(), claim_id);
    }
    Ok(ids)
}

async fn find_or_insert_web_source_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    claim: &EvidenceClaimProposal,
    content_sha256: &str,
) -> Result<i64, AircraftHierarchyPersistenceError> {
    let tier = source_tier(claim.source_kind);
    let domain = evidence_source_domain(&claim.source_url)?;
    if let Some(id) = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM curation_evidence_sources
        WHERE source_url = ? AND resolved_url = ?
          AND source_domain = ? AND source_tier = ? AND content_sha256 = ?
        ORDER BY id LIMIT 1
        "#,
    )
    .bind(&claim.source_url)
    .bind(&claim.source_url)
    .bind(&domain)
    .bind(tier)
    .bind(content_sha256)
    .fetch_optional(&mut **transaction)
    .await?
    {
        return Ok(id);
    }
    if sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM curation_evidence_sources
        WHERE source_url = ? AND content_sha256 = ?
        ORDER BY id LIMIT 1
        "#,
    )
    .bind(&claim.source_url)
    .bind(content_sha256)
    .fetch_optional(&mut **transaction)
    .await?
    .is_some()
    {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "verified web source {} with content digest {} already exists under different tier, domain, or resolved-URL metadata",
            claim.source_url, content_sha256
        )));
    }
    Ok(sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO curation_evidence_sources (
          source_url, resolved_url, source_title, publisher, source_domain,
          source_tier, content_sha256, retrieved_at
        ) VALUES (?, ?, ?, NULL, ?, ?, ?, CURRENT_TIMESTAMP)
        RETURNING id
        "#,
    )
    .bind(&claim.source_url)
    .bind(&claim.source_url)
    .bind(&claim.source_title)
    .bind(&domain)
    .bind(tier)
    .bind(content_sha256)
    .fetch_one(&mut **transaction)
    .await?)
}

async fn find_or_insert_web_source_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    claim: &EvidenceClaimProposal,
    content_sha256: &str,
) -> Result<i64, AircraftHierarchyPersistenceError> {
    let tier = source_tier(claim.source_kind);
    let domain = evidence_source_domain(&claim.source_url)?;
    if let Some(id) = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM curation_evidence_sources
        WHERE source_url = $1 AND resolved_url = $2
          AND source_domain = $3 AND source_tier = $4 AND content_sha256 = $5
        ORDER BY id LIMIT 1
        "#,
    )
    .bind(&claim.source_url)
    .bind(&claim.source_url)
    .bind(&domain)
    .bind(tier)
    .bind(content_sha256)
    .fetch_optional(&mut **transaction)
    .await?
    {
        return Ok(id);
    }
    if sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM curation_evidence_sources
        WHERE source_url = $1 AND content_sha256 = $2
        ORDER BY id LIMIT 1
        "#,
    )
    .bind(&claim.source_url)
    .bind(content_sha256)
    .fetch_optional(&mut **transaction)
    .await?
    .is_some()
    {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "verified web source {} with content digest {} already exists under different tier, domain, or resolved-URL metadata",
            claim.source_url, content_sha256
        )));
    }
    Ok(sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO curation_evidence_sources (
          source_url, resolved_url, source_title, publisher, source_domain,
          source_tier, content_sha256, retrieved_at
        ) VALUES ($1, $2, $3, NULL, $4, $5, $6, CURRENT_TIMESTAMP)
        RETURNING id
        "#,
    )
    .bind(&claim.source_url)
    .bind(&claim.source_url)
    .bind(&claim.source_title)
    .bind(&domain)
    .bind(tier)
    .bind(content_sha256)
    .fetch_one(&mut **transaction)
    .await?)
}

struct DecisionSpec<'a> {
    suffix: &'a str,
    resolution_scope: &'a str,
    entity_kind: &'a str,
    action: &'a str,
    status: &'a str,
    selected_entity_id: Option<i64>,
    rationale: &'a str,
    evidence_ids: Vec<&'a str>,
}

async fn ensure_decision_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    observation_id: i64,
    expected_catalog_revision: &str,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    spec: DecisionSpec<'_>,
) -> Result<(i64, bool), AircraftHierarchyPersistenceError> {
    let job_fingerprint = decision_fingerprint(prepared, spec.suffix);
    let inserted = sqlx::query(
        r#"
        INSERT INTO aircraft_identity_resolution_cases (
          observation_id, resolution_scope, job_fingerprint,
          catalog_revision, case_status
        ) VALUES (?, ?, ?, ?, 'resolved')
        ON CONFLICT (job_fingerprint) DO NOTHING
        "#,
    )
    .bind(observation_id)
    .bind(spec.resolution_scope)
    .bind(&job_fingerprint)
    .bind(expected_catalog_revision)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
        == 1;
    let (case_id, stored_revision, case_status) =
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, catalog_revision, case_status FROM aircraft_identity_resolution_cases WHERE job_fingerprint = ?",
        )
        .bind(&job_fingerprint)
        .fetch_one(&mut **transaction)
        .await?;
    if stored_revision != expected_catalog_revision || case_status != "resolved" {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "approval fingerprint collision for {} resolution case",
            spec.suffix
        )));
    }
    if let Some(existing) = sqlx::query_as::<_, ExistingDecisionRow>(
        r#"
        SELECT id, entity_kind, decision_action, decision_status, selected_entity_id,
               decision_payload_json, deterministic_validation_json
        FROM aircraft_identity_decisions
        WHERE resolution_case_id = ?
        ORDER BY id LIMIT 1
        "#,
    )
    .bind(case_id)
    .fetch_optional(&mut **transaction)
    .await?
    {
        validate_existing_decision(&existing, prepared, &spec)?;
        return Ok((existing.id, false));
    }
    if !inserted {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "resolved {} case has no immutable decision",
            spec.suffix
        )));
    }
    let decision_id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO aircraft_identity_decisions (
          resolution_case_id, entity_kind, decision_action, decision_status,
          selected_entity_id, decision_payload_json,
          deterministic_validation_json, deterministic_validation_passed,
          rationale, decided_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, CURRENT_TIMESTAMP)
        RETURNING id
        "#,
    )
    .bind(case_id)
    .bind(spec.entity_kind)
    .bind(spec.action)
    .bind(spec.status)
    .bind(spec.selected_entity_id)
    .bind(&prepared.payload_json)
    .bind(&prepared.validation_json)
    .bind(spec.rationale)
    .fetch_one(&mut **transaction)
    .await?;
    attach_decision_claims_sqlite(
        transaction,
        decision_id,
        prepared,
        claim_ids,
        &spec.evidence_ids,
    )
    .await?;
    Ok((decision_id, true))
}

async fn ensure_decision_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    observation_id: i64,
    expected_catalog_revision: &str,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    spec: DecisionSpec<'_>,
) -> Result<(i64, bool), AircraftHierarchyPersistenceError> {
    let job_fingerprint = decision_fingerprint(prepared, spec.suffix);
    let inserted = sqlx::query(
        r#"
        INSERT INTO aircraft_identity_resolution_cases (
          observation_id, resolution_scope, job_fingerprint,
          catalog_revision, case_status
        ) VALUES ($1, $2, $3, $4, 'resolved')
        ON CONFLICT (job_fingerprint) DO NOTHING
        "#,
    )
    .bind(observation_id)
    .bind(spec.resolution_scope)
    .bind(&job_fingerprint)
    .bind(expected_catalog_revision)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
        == 1;
    let (case_id, stored_revision, case_status) =
        sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, catalog_revision, case_status FROM aircraft_identity_resolution_cases WHERE job_fingerprint = $1",
        )
        .bind(&job_fingerprint)
        .fetch_one(&mut **transaction)
        .await?;
    if stored_revision != expected_catalog_revision || case_status != "resolved" {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "approval fingerprint collision for {} resolution case",
            spec.suffix
        )));
    }
    if let Some(existing) = sqlx::query_as::<_, ExistingDecisionRow>(
        r#"
        SELECT id, entity_kind, decision_action, decision_status, selected_entity_id,
               decision_payload_json, deterministic_validation_json
        FROM aircraft_identity_decisions
        WHERE resolution_case_id = $1
        ORDER BY id LIMIT 1
        "#,
    )
    .bind(case_id)
    .fetch_optional(&mut **transaction)
    .await?
    {
        validate_existing_decision(&existing, prepared, &spec)?;
        return Ok((existing.id, false));
    }
    if !inserted {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "resolved {} case has no immutable decision",
            spec.suffix
        )));
    }
    let decision_id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO aircraft_identity_decisions (
          resolution_case_id, entity_kind, decision_action, decision_status,
          selected_entity_id, decision_payload_json,
          deterministic_validation_json, deterministic_validation_passed,
          rationale, decided_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, CURRENT_TIMESTAMP)
        RETURNING id
        "#,
    )
    .bind(case_id)
    .bind(spec.entity_kind)
    .bind(spec.action)
    .bind(spec.status)
    .bind(spec.selected_entity_id)
    .bind(&prepared.payload_json)
    .bind(&prepared.validation_json)
    .bind(spec.rationale)
    .fetch_one(&mut **transaction)
    .await?;
    attach_decision_claims_postgres(
        transaction,
        decision_id,
        prepared,
        claim_ids,
        &spec.evidence_ids,
    )
    .await?;
    Ok((decision_id, true))
}

fn validate_existing_decision(
    existing: &ExistingDecisionRow,
    prepared: &PreparedApproval,
    spec: &DecisionSpec<'_>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    if existing.entity_kind != spec.entity_kind
        || existing.decision_action != spec.action
        || existing.decision_status != spec.status
        || existing.selected_entity_id != spec.selected_entity_id
        || existing.decision_payload_json != prepared.payload_json
        || existing.deterministic_validation_json != prepared.validation_json
    {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "approval fingerprint collision for {} decision",
            spec.suffix
        )));
    }
    Ok(())
}

async fn attach_decision_claims_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    decision_id: i64,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    evidence_ids: &[&str],
) -> Result<(), AircraftHierarchyPersistenceError> {
    for evidence_id in evidence_ids {
        let claim_id = *claim_ids.get(*evidence_id).ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(format!(
                "decision evidence {evidence_id} was not persisted"
            ))
        })?;
        let role = prepared
            .claims_by_evidence_id
            .get(*evidence_id)
            .map(evidence_role)
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Invalid(format!(
                    "decision evidence {evidence_id} is absent from the verified proposal"
                ))
            })?;
        sqlx::query(
            r#"
            INSERT INTO aircraft_identity_decision_claims (
              decision_id, evidence_claim_id, evidence_role
            ) VALUES (?, ?, ?)
            ON CONFLICT (decision_id, evidence_claim_id, evidence_role) DO NOTHING
            "#,
        )
        .bind(decision_id)
        .bind(claim_id)
        .bind(role)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn attach_decision_claims_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    decision_id: i64,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    evidence_ids: &[&str],
) -> Result<(), AircraftHierarchyPersistenceError> {
    for evidence_id in evidence_ids {
        let claim_id = *claim_ids.get(*evidence_id).ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(format!(
                "decision evidence {evidence_id} was not persisted"
            ))
        })?;
        let role = prepared
            .claims_by_evidence_id
            .get(*evidence_id)
            .map(evidence_role)
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Invalid(format!(
                    "decision evidence {evidence_id} is absent from the verified proposal"
                ))
            })?;
        sqlx::query(
            r#"
            INSERT INTO aircraft_identity_decision_claims (
              decision_id, evidence_claim_id, evidence_role
            ) VALUES ($1, $2, $3)
            ON CONFLICT (decision_id, evidence_claim_id, evidence_role) DO NOTHING
            "#,
        )
        .bind(decision_id)
        .bind(claim_id)
        .bind(role)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn attach_tcds_claims_to_designation_approval_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    designation_id: i64,
    approval_decision_id: i64,
    reviewable: &ReviewableAircraftHierarchy,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let evidence_ids = tcds_family_evidence_ids(reviewable);
    if evidence_ids.is_empty() {
        return Ok(());
    }
    let valid = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM aircraft_designations designation
          JOIN aircraft_identity_decisions decision
            ON decision.id = designation.approval_decision_id
          WHERE designation.id = ?
            AND designation.approval_decision_id = ?
            AND decision.entity_kind = 'designation'
            AND decision.decision_action = 'approve_new'
            AND decision.decision_status = 'approved'
            AND decision.deterministic_validation_passed = 1
        )
        "#,
    )
    .bind(designation_id)
    .bind(approval_decision_id)
    .fetch_one(&mut **transaction)
    .await?
        != 0;
    if !valid {
        return Err(AircraftHierarchyPersistenceError::Collision(
            "FAA TCDS evidence cannot be attached to a different or unapproved designation decision"
                .to_string(),
        ));
    }
    attach_decision_claims_sqlite(
        transaction,
        approval_decision_id,
        prepared,
        claim_ids,
        &evidence_ids,
    )
    .await
}

async fn attach_tcds_claims_to_designation_approval_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    designation_id: i64,
    approval_decision_id: i64,
    reviewable: &ReviewableAircraftHierarchy,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let evidence_ids = tcds_family_evidence_ids(reviewable);
    if evidence_ids.is_empty() {
        return Ok(());
    }
    let valid = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM aircraft_designations designation
          JOIN aircraft_identity_decisions decision
            ON decision.id = designation.approval_decision_id
          WHERE designation.id = $1
            AND designation.approval_decision_id = $2
            AND decision.entity_kind = 'designation'
            AND decision.decision_action = 'approve_new'
            AND decision.decision_status = 'approved'
            AND decision.deterministic_validation_passed = TRUE
        )
        "#,
    )
    .bind(designation_id)
    .bind(approval_decision_id)
    .fetch_one(&mut **transaction)
    .await?;
    if !valid {
        return Err(AircraftHierarchyPersistenceError::Collision(
            "FAA TCDS evidence cannot be attached to a different or unapproved designation decision"
                .to_string(),
        ));
    }
    attach_decision_claims_postgres(
        transaction,
        approval_decision_id,
        prepared,
        claim_ids,
        &evidence_ids,
    )
    .await
}

fn validate_existing_tcds_make_lineage(
    existing: &ExistingTcdsMakeLineageRow,
    prepared: &PreparedTcdsMakeLineage,
    make_id: i64,
    designation_id: i64,
) -> Result<(), AircraftHierarchyPersistenceError> {
    if existing.aircraft_make_id != make_id
        || existing.aircraft_designation_id != designation_id
        || existing.tcds_number != prepared.tcds_number
        || existing.tcds_document_guid != prepared.document_guid
        || existing.tcds_pdf_sha256 != prepared.pdf_sha256
        || existing.tcds_former_holder_name != prepared.former_holder_name
        || existing.tcds_current_holder_name != prepared.current_holder_name
        || existing.tcds_manufacturer_name != prepared.manufacturer_name
        || existing.tcds_selection_basis != prepared.selection_basis
        || existing.serial_scope_kind != prepared.serial_scope_kind
        || existing.serial_prefix != prepared.serial_scope.prefix
        || existing.serial_digits_width != prepared.serial_scope.digits_width
        || existing.first_serial_number != prepared.serial_scope.first_number
        || existing.last_serial_number != prepared.serial_scope.last_number
    {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "approved FAA/TCDS make-lineage binding {} conflicts with the exact current relationship",
            existing.id
        )));
    }
    Ok(())
}

fn prepared_lineage_claim_ids<'a>(lineage: &'a PreparedTcdsMakeLineage) -> Vec<&'a str> {
    [
        Some(lineage.faa_make_evidence_id.as_str()),
        Some(lineage.model_identity_evidence_id.as_str()),
        Some(lineage.serial_applicability_evidence_id.as_str()),
        Some(lineage.holder_transfer_evidence_id.as_str()),
        lineage.manufacturer_range_evidence_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

async fn persist_tcds_make_lineage_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    make_id: i64,
    make_decision_id: i64,
    designation_id: i64,
    replay: bool,
    catalog_writes: &mut usize,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let Some(lineage) = prepare_tcds_make_lineage(input.reviewable, input.grounding)? else {
        return Ok(());
    };
    let reference = input
        .grounding
        .aircraft
        .as_ref()
        .expect("prepared reference");
    let existing = sqlx::query_as::<_, ExistingTcdsMakeLineageRow>(
        r#"
        SELECT id, aircraft_make_id, aircraft_designation_id, tcds_number,
               tcds_document_guid, tcds_pdf_sha256,
               tcds_former_holder_name, tcds_current_holder_name,
               tcds_manufacturer_name, tcds_selection_basis,
               serial_scope_kind, serial_prefix,
               serial_digits_width, first_serial_number, last_serial_number
        FROM aircraft_tcds_make_lineage_bindings
        WHERE faa_snapshot_date = ?
          AND faa_archive_sha256 = ?
          AND faa_aircraft_code = ?
          AND faa_manufacturer_name = ?
          AND faa_model = ?
          AND serial_prefix = ?
          AND serial_digits_width = ?
          AND first_serial_number = ?
          AND coalesce(last_serial_number, -1) =
              coalesce(?, -1)
        ORDER BY id
        LIMIT 2
        "#,
    )
    .bind(&input.grounding.snapshot.snapshot_date)
    .bind(&input.grounding.snapshot.archive_sha256)
    .bind(&input.grounding.aircraft_code)
    .bind(reference.manufacturer_name.as_deref().unwrap_or_default())
    .bind(reference.model_name.as_deref().unwrap_or_default())
    .bind(&lineage.serial_scope.prefix)
    .bind(lineage.serial_scope.digits_width)
    .bind(lineage.serial_scope.first_number)
    .bind(lineage.serial_scope.last_number)
    .fetch_all(&mut **transaction)
    .await?;
    if existing.len() > 1 {
        return Err(AircraftHierarchyPersistenceError::Collision(
            "multiple FAA/TCDS make-lineage bindings cover one exact identity scope".to_string(),
        ));
    }
    if let Some(existing) = existing.first() {
        return validate_existing_tcds_make_lineage(existing, &lineage, make_id, designation_id);
    }
    if replay {
        return Err(AircraftHierarchyPersistenceError::Collision(
            "replayed FAA/TCDS make-lineage decision has no immutable binding".to_string(),
        ));
    }
    let evidence_ids = prepared_lineage_claim_ids(&lineage);
    attach_decision_claims_sqlite(
        transaction,
        make_decision_id,
        prepared,
        claim_ids,
        &evidence_ids,
    )
    .await?;
    let claim_id = |evidence_id: &str| {
        claim_ids.get(evidence_id).copied().ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(format!(
                "FAA/TCDS make-lineage evidence {evidence_id} was not persisted"
            ))
        })
    };
    sqlx::query(
        r#"
        INSERT INTO aircraft_tcds_make_lineage_bindings (
          faa_snapshot_date, faa_archive_sha256, faa_aircraft_code,
          representative_faa_registry_snapshot_id,
          representative_faa_n_number,
          representative_faa_source_record_sha256,
          representative_faa_manufacturer_serial_key,
          faa_manufacturer_name, faa_model,
          aircraft_make_id, aircraft_designation_id,
          tcds_number, tcds_document_guid, tcds_pdf_sha256,
          tcds_former_holder_name, tcds_current_holder_name,
          tcds_manufacturer_name, tcds_selection_basis, serial_scope_kind,
          serial_prefix, serial_digits_width,
          first_serial_number, last_serial_number,
          approval_decision_id, faa_make_evidence_claim_id,
          tcds_model_identity_evidence_claim_id,
          tcds_serial_applicability_evidence_claim_id,
          tcds_holder_transfer_evidence_claim_id,
          tcds_manufacturer_range_evidence_claim_id
        ) VALUES (
          ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
          ?, ?, ?, ?, ?, ?, ?, ?, ?
        )
        "#,
    )
    .bind(&input.grounding.snapshot.snapshot_date)
    .bind(&input.grounding.snapshot.archive_sha256)
    .bind(&input.grounding.aircraft_code)
    .bind(input.grounding.snapshot.id)
    .bind(&input.grounding.n_number)
    .bind(&input.grounding.source_record_sha256)
    .bind(
        input
            .grounding
            .manufacturer_serial_key
            .as_deref()
            .expect("prepared lineage has an exact FAA serial"),
    )
    .bind(reference.manufacturer_name.as_deref().unwrap_or_default())
    .bind(reference.model_name.as_deref().unwrap_or_default())
    .bind(make_id)
    .bind(designation_id)
    .bind(&lineage.tcds_number)
    .bind(&lineage.document_guid)
    .bind(&lineage.pdf_sha256)
    .bind(&lineage.former_holder_name)
    .bind(&lineage.current_holder_name)
    .bind(lineage.manufacturer_name.as_deref())
    .bind(lineage.selection_basis)
    .bind(lineage.serial_scope_kind)
    .bind(&lineage.serial_scope.prefix)
    .bind(lineage.serial_scope.digits_width)
    .bind(lineage.serial_scope.first_number)
    .bind(lineage.serial_scope.last_number)
    .bind(make_decision_id)
    .bind(claim_id(&lineage.faa_make_evidence_id)?)
    .bind(claim_id(&lineage.model_identity_evidence_id)?)
    .bind(claim_id(&lineage.serial_applicability_evidence_id)?)
    .bind(claim_id(&lineage.holder_transfer_evidence_id)?)
    .bind(
        lineage
            .manufacturer_range_evidence_id
            .as_deref()
            .map(claim_id)
            .transpose()?,
    )
    .execute(&mut **transaction)
    .await?;
    *catalog_writes += 1;
    Ok(())
}

async fn persist_tcds_make_lineage_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    make_id: i64,
    make_decision_id: i64,
    designation_id: i64,
    replay: bool,
    catalog_writes: &mut usize,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let Some(lineage) = prepare_tcds_make_lineage(input.reviewable, input.grounding)? else {
        return Ok(());
    };
    let reference = input
        .grounding
        .aircraft
        .as_ref()
        .expect("prepared reference");
    let existing = sqlx::query_as::<_, ExistingTcdsMakeLineageRow>(
        r#"
        SELECT id, aircraft_make_id, aircraft_designation_id, tcds_number,
               tcds_document_guid, tcds_pdf_sha256,
               tcds_former_holder_name, tcds_current_holder_name,
               tcds_manufacturer_name, tcds_selection_basis,
               serial_scope_kind, serial_prefix,
               serial_digits_width, first_serial_number, last_serial_number
        FROM aircraft_tcds_make_lineage_bindings
        WHERE faa_snapshot_date = $1
          AND faa_archive_sha256 = $2
          AND faa_aircraft_code = $3
          AND faa_manufacturer_name = $4
          AND faa_model = $5
          AND serial_prefix = $6
          AND serial_digits_width = $7
          AND first_serial_number = $8
          AND coalesce(last_serial_number, -1) =
              coalesce($9, -1)
        ORDER BY id
        LIMIT 2
        "#,
    )
    .bind(&input.grounding.snapshot.snapshot_date)
    .bind(&input.grounding.snapshot.archive_sha256)
    .bind(&input.grounding.aircraft_code)
    .bind(reference.manufacturer_name.as_deref().unwrap_or_default())
    .bind(reference.model_name.as_deref().unwrap_or_default())
    .bind(&lineage.serial_scope.prefix)
    .bind(lineage.serial_scope.digits_width)
    .bind(lineage.serial_scope.first_number)
    .bind(lineage.serial_scope.last_number)
    .fetch_all(&mut **transaction)
    .await?;
    if existing.len() > 1 {
        return Err(AircraftHierarchyPersistenceError::Collision(
            "multiple FAA/TCDS make-lineage bindings cover one exact identity scope".to_string(),
        ));
    }
    if let Some(existing) = existing.first() {
        return validate_existing_tcds_make_lineage(existing, &lineage, make_id, designation_id);
    }
    if replay {
        return Err(AircraftHierarchyPersistenceError::Collision(
            "replayed FAA/TCDS make-lineage decision has no immutable binding".to_string(),
        ));
    }
    let evidence_ids = prepared_lineage_claim_ids(&lineage);
    attach_decision_claims_postgres(
        transaction,
        make_decision_id,
        prepared,
        claim_ids,
        &evidence_ids,
    )
    .await?;
    let claim_id = |evidence_id: &str| {
        claim_ids.get(evidence_id).copied().ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(format!(
                "FAA/TCDS make-lineage evidence {evidence_id} was not persisted"
            ))
        })
    };
    sqlx::query(
        r#"
        INSERT INTO aircraft_tcds_make_lineage_bindings (
          faa_snapshot_date, faa_archive_sha256, faa_aircraft_code,
          representative_faa_registry_snapshot_id,
          representative_faa_n_number,
          representative_faa_source_record_sha256,
          representative_faa_manufacturer_serial_key,
          faa_manufacturer_name, faa_model,
          aircraft_make_id, aircraft_designation_id,
          tcds_number, tcds_document_guid, tcds_pdf_sha256,
          tcds_former_holder_name, tcds_current_holder_name,
          tcds_manufacturer_name, tcds_selection_basis, serial_scope_kind,
          serial_prefix, serial_digits_width,
          first_serial_number, last_serial_number,
          approval_decision_id, faa_make_evidence_claim_id,
          tcds_model_identity_evidence_claim_id,
          tcds_serial_applicability_evidence_claim_id,
          tcds_holder_transfer_evidence_claim_id,
          tcds_manufacturer_range_evidence_claim_id
        ) VALUES (
          $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
          $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24,
          $25, $26, $27, $28, $29
        )
        "#,
    )
    .bind(&input.grounding.snapshot.snapshot_date)
    .bind(&input.grounding.snapshot.archive_sha256)
    .bind(&input.grounding.aircraft_code)
    .bind(input.grounding.snapshot.id)
    .bind(&input.grounding.n_number)
    .bind(&input.grounding.source_record_sha256)
    .bind(
        input
            .grounding
            .manufacturer_serial_key
            .as_deref()
            .expect("prepared lineage has an exact FAA serial"),
    )
    .bind(reference.manufacturer_name.as_deref().unwrap_or_default())
    .bind(reference.model_name.as_deref().unwrap_or_default())
    .bind(make_id)
    .bind(designation_id)
    .bind(&lineage.tcds_number)
    .bind(&lineage.document_guid)
    .bind(&lineage.pdf_sha256)
    .bind(&lineage.former_holder_name)
    .bind(&lineage.current_holder_name)
    .bind(lineage.manufacturer_name.as_deref())
    .bind(lineage.selection_basis)
    .bind(lineage.serial_scope_kind)
    .bind(&lineage.serial_scope.prefix)
    .bind(lineage.serial_scope.digits_width)
    .bind(lineage.serial_scope.first_number)
    .bind(lineage.serial_scope.last_number)
    .bind(make_decision_id)
    .bind(claim_id(&lineage.faa_make_evidence_id)?)
    .bind(claim_id(&lineage.model_identity_evidence_id)?)
    .bind(claim_id(&lineage.serial_applicability_evidence_id)?)
    .bind(claim_id(&lineage.holder_transfer_evidence_id)?)
    .bind(
        lineage
            .manufacturer_range_evidence_id
            .as_deref()
            .map(claim_id)
            .transpose()?,
    )
    .execute(&mut **transaction)
    .await?;
    *catalog_writes += 1;
    Ok(())
}

async fn reject_tcds_holder_make_duplicate_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &PersistReviewableAircraftHierarchy<'_>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    if input.reviewable.adjudication.make.action != EntityResolutionAction::ProposeNew {
        return Ok(());
    }
    let Some(holder) = input
        .reviewable
        .server_faa_evidence
        .tcds_make_lineage_evidence()
        .and_then(|evidence| evidence.holder_transfer.as_ref())
    else {
        return Ok(());
    };
    let ids = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id
        FROM aircraft_makes
        WHERE lower(rtrim(trim(name), '.')) =
              lower(rtrim(trim(?), '.'))
           OR lower(rtrim(trim(name), '.')) =
              lower(rtrim(trim(?), '.'))
        ORDER BY id
        LIMIT 2
        "#,
    )
    .bind(&holder.former_holder_name)
    .bind(&holder.current_holder_name)
    .fetch_all(&mut **transaction)
    .await?;
    if !ids.is_empty() {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "FAA/TCDS holder lineage resolves to existing aircraft make id(s) {ids:?}; proposing a duplicate legal-make branch is forbidden"
        )));
    }
    Ok(())
}

async fn reject_tcds_holder_make_duplicate_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &PersistReviewableAircraftHierarchy<'_>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    if input.reviewable.adjudication.make.action != EntityResolutionAction::ProposeNew {
        return Ok(());
    }
    let Some(holder) = input
        .reviewable
        .server_faa_evidence
        .tcds_make_lineage_evidence()
        .and_then(|evidence| evidence.holder_transfer.as_ref())
    else {
        return Ok(());
    };
    let ids = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id
        FROM aircraft_makes
        WHERE lower(rtrim(btrim(name), '.')) =
              lower(rtrim(btrim($1), '.'))
           OR lower(rtrim(btrim(name), '.')) =
              lower(rtrim(btrim($2), '.'))
        ORDER BY id
        LIMIT 2
        "#,
    )
    .bind(&holder.former_holder_name)
    .bind(&holder.current_holder_name)
    .fetch_all(&mut **transaction)
    .await?;
    if !ids.is_empty() {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "FAA/TCDS holder lineage resolves to existing aircraft make id(s) {ids:?}; proposing a duplicate legal-make branch is forbidden"
        )));
    }
    Ok(())
}

async fn persist_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    _db: &AppDb,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    observation_id: i64,
    current_revision: &str,
) -> Result<ResolvedHierarchy, AircraftHierarchyPersistenceError> {
    let replay = approval_case_exists_sqlite(transaction, prepared).await?;
    if !replay && current_revision != input.expected_catalog_revision {
        return Err(AircraftHierarchyPersistenceError::StaleCatalog {
            expected: input.expected_catalog_revision.to_string(),
            actual: current_revision.to_string(),
        });
    }
    let claim_ids = if replay {
        BTreeMap::new()
    } else {
        persist_claims_sqlite(transaction, prepared, input.grounding).await?
    };
    let mut catalog_writes = 0;
    reject_tcds_holder_make_duplicate_sqlite(transaction, input).await?;
    let (make_id, make_name, make_decision_id) = resolve_make_sqlite(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        &mut catalog_writes,
    )
    .await?;
    resolve_faa_make_alias_sqlite(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        make_id,
        &mut catalog_writes,
    )
    .await?;
    let (family_id, family_name) = resolve_family_sqlite(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        make_id,
        &mut catalog_writes,
    )
    .await?;
    resolve_family_label_relationship_sqlite(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        make_id,
        family_id,
        &family_name,
        &mut catalog_writes,
    )
    .await?;
    let (designation_id, official_designation, designation_approval_decision_id) =
        resolve_designation_sqlite(
            transaction,
            observation_id,
            input,
            prepared,
            &claim_ids,
            family_id,
            &mut catalog_writes,
        )
        .await?;
    if !replay {
        attach_tcds_claims_to_designation_approval_sqlite(
            transaction,
            designation_id,
            designation_approval_decision_id,
            input.reviewable,
            prepared,
            &claim_ids,
        )
        .await?;
    }
    persist_tcds_make_lineage_sqlite(
        transaction,
        input,
        prepared,
        &claim_ids,
        make_id,
        make_decision_id,
        designation_id,
        replay,
        &mut catalog_writes,
    )
    .await?;
    let generation_id = resolve_generation_sqlite(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        family_id,
        designation_id,
        &mut catalog_writes,
    )
    .await?;
    let package_id = resolve_package_sqlite(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        family_id,
        designation_id,
        generation_id,
    )
    .await?;
    let mut resolved = ResolvedHierarchy {
        hierarchy: AircraftHierarchy {
            manufacturer_id: make_id,
            model_family_id: family_id,
            certified_variant_id: designation_id,
            generation_id,
            tier_id: package_id,
        },
        make_name,
        family_name,
        official_designation,
        designation_approval_decision_id,
        catalog_writes,
        idempotent_replay: replay,
        assignment: None,
    };
    let assignment_id = ensure_assignment_sqlite(transaction, input, &resolved).await?;
    resolved.assignment =
        Some(load_persisted_assignment_sqlite(transaction, input.listing_id, assignment_id).await?);
    Ok(resolved)
}

async fn approval_case_exists_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    prepared: &PreparedApproval,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS (SELECT 1 FROM aircraft_identity_resolution_cases WHERE job_fingerprint = ?)",
    )
    .bind(decision_fingerprint(prepared, "make"))
    .fetch_one(&mut **transaction)
    .await?
        != 0)
}

fn string_ids(values: &[String]) -> Vec<&str> {
    values.iter().map(String::as_str).collect()
}

async fn resolve_make_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    catalog_writes: &mut usize,
) -> Result<(i64, String, i64), AircraftHierarchyPersistenceError> {
    let proposal = &input.reviewable.proposal.manufacturer;
    let decision = &input.reviewable.adjudication.make;
    match decision.action {
        EntityResolutionAction::MatchExisting => {
            let id = proposal.existing_catalog_id.expect("projection validated");
            let row = sqlx::query_as::<_, (String, String)>(
                "SELECT name, normalized_name FROM aircraft_makes WHERE id = ?",
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft make id {id} no longer exists"
                ))
            })?;
            if row.0 != proposal.display_name
                || row.1 != normalize_aircraft_retrieval_text(&proposal.display_name)
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft make id {id} changed identity"
                )));
            }
            let (approval_decision_id, _) = ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "make",
                    resolution_scope: "make",
                    entity_kind: "make",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(id),
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            Ok((id, row.0, approval_decision_id))
        }
        EntityResolutionAction::ProposeNew => {
            let normalized = normalize_aircraft_retrieval_text(&proposal.display_name);
            let (approval_decision_id, created) = ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "make",
                    resolution_scope: "make",
                    entity_kind: "make",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<_, (i64, String, String)>(
                    "SELECT id, name, normalized_name FROM aircraft_makes WHERE approval_decision_id = ?",
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed make decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != proposal.display_name || row.2 != normalized {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed make decision points to a different canonical row".to_string(),
                    ));
                }
                return Ok((row.0, row.1, approval_decision_id));
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM aircraft_makes WHERE normalized_name = ?",
            )
            .bind(&normalized)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed make collides with existing catalog id {existing_id}"
                )));
            }
            let id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO aircraft_makes (name, normalized_name, approval_decision_id) VALUES (?, ?, ?) RETURNING id",
            )
            .bind(&proposal.display_name)
            .bind(&normalized)
            .bind(approval_decision_id)
            .fetch_one(&mut **transaction)
            .await?;
            *catalog_writes += 1;
            Ok((id, proposal.display_name.clone(), approval_decision_id))
        }
        _ => Err(AircraftHierarchyPersistenceError::Invalid(
            "make decision is not persistable".to_string(),
        )),
    }
}

async fn resolve_family_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    make_id: i64,
    catalog_writes: &mut usize,
) -> Result<(i64, String), AircraftHierarchyPersistenceError> {
    let proposal = &input.reviewable.proposal.model_family;
    let decision = &input.reviewable.adjudication.family;
    let normalized = normalize_aircraft_retrieval_text(&proposal.display_name);
    match decision.action {
        EntityResolutionAction::MatchExisting => {
            let id = proposal.existing_catalog_id.expect("projection validated");
            let row = sqlx::query_as::<_, (i64, String, String)>(
                "SELECT aircraft_make_id, name, normalized_name FROM aircraft_model_families WHERE id = ?",
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft family id {id} no longer exists"
                ))
            })?;
            if row.0 != make_id || row.1 != proposal.display_name || row.2 != normalized {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft family id {id} changed identity or parent"
                )));
            }
            ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family",
                    resolution_scope: "family",
                    entity_kind: "family",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(id),
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            Ok((id, row.1))
        }
        EntityResolutionAction::ProposeNew => {
            let (approval_decision_id, created) = ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family",
                    resolution_scope: "family",
                    entity_kind: "family",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<_, (i64, i64, String, String)>(
                    "SELECT id, aircraft_make_id, name, normalized_name FROM aircraft_model_families WHERE approval_decision_id = ?",
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed family decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != make_id || row.2 != proposal.display_name || row.3 != normalized {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed family decision points to a different canonical row".to_string(),
                    ));
                }
                return Ok((row.0, row.2));
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM aircraft_model_families WHERE aircraft_make_id = ? AND normalized_name = ?",
            )
            .bind(make_id)
            .bind(&normalized)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed family collides with existing catalog id {existing_id}"
                )));
            }
            let id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO aircraft_model_families (aircraft_make_id, name, normalized_name, approval_decision_id) VALUES (?, ?, ?, ?) RETURNING id",
            )
            .bind(make_id)
            .bind(&proposal.display_name)
            .bind(&normalized)
            .bind(approval_decision_id)
            .fetch_one(&mut **transaction)
            .await?;
            *catalog_writes += 1;
            Ok((id, proposal.display_name.clone()))
        }
        _ => Err(AircraftHierarchyPersistenceError::Invalid(
            "family decision is not persistable".to_string(),
        )),
    }
}

async fn resolve_designation_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    family_id: i64,
    catalog_writes: &mut usize,
) -> Result<(i64, String, i64), AircraftHierarchyPersistenceError> {
    let proposal = &input.reviewable.proposal.certified_variant;
    let decision = &input.reviewable.adjudication.designation;
    let official = proposal
        .authoritative_designator
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(
                "certified designation requires an authoritative designator".to_string(),
            )
        })?;
    let normalized = normalize_aircraft_designator_retrieval_key(official);
    match decision.action {
        EntityResolutionAction::MatchExisting => {
            let id = proposal.existing_catalog_id.expect("projection validated");
            let row = sqlx::query_as::<_, (i64, String, String, String, i64)>(
                r#"
                SELECT aircraft_model_family_id, official_designation,
                       normalized_official_designation, display_name,
                       approval_decision_id
                FROM aircraft_designations WHERE id = ?
                "#,
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft designation id {id} no longer exists"
                ))
            })?;
            if row.0 != family_id
                || row.1 != official
                || row.2 != normalized
                || row.3 != proposal.display_name
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft designation id {id} changed identity or parent"
                )));
            }
            ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "designation",
                    resolution_scope: "designation",
                    entity_kind: "designation",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(id),
                    rationale: &decision.rationale,
                    evidence_ids: designation_evidence_ids(input.reviewable),
                },
            )
            .await?;
            Ok((id, row.1, row.4))
        }
        EntityResolutionAction::ProposeNew => {
            let (approval_decision_id, created) = ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "designation",
                    resolution_scope: "designation",
                    entity_kind: "designation",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &decision.rationale,
                    evidence_ids: designation_evidence_ids(input.reviewable),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<_, (i64, i64, String, String, String)>(
                    r#"
                    SELECT id, aircraft_model_family_id, official_designation,
                           normalized_official_designation, display_name
                    FROM aircraft_designations WHERE approval_decision_id = ?
                    "#,
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed designation decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != family_id
                    || row.2 != official
                    || row.3 != normalized
                    || row.4 != proposal.display_name
                {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed designation decision points to a different canonical row"
                            .to_string(),
                    ));
                }
                return Ok((row.0, row.2, approval_decision_id));
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM aircraft_designations WHERE aircraft_model_family_id = ? AND normalized_official_designation = ?",
            )
            .bind(family_id)
            .bind(&normalized)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed designation collides with existing catalog id {existing_id}"
                )));
            }
            let id = sqlx::query_scalar::<_, i64>(
                r#"
                INSERT INTO aircraft_designations (
                  aircraft_model_family_id, official_designation,
                  normalized_official_designation, display_name,
                  approval_decision_id
                ) VALUES (?, ?, ?, ?, ?) RETURNING id
                "#,
            )
            .bind(family_id)
            .bind(official)
            .bind(&normalized)
            .bind(&proposal.display_name)
            .bind(approval_decision_id)
            .fetch_one(&mut **transaction)
            .await?;
            *catalog_writes += 1;
            Ok((id, official.to_string(), approval_decision_id))
        }
        _ => Err(AircraftHierarchyPersistenceError::Invalid(
            "designation decision is not persistable".to_string(),
        )),
    }
}

async fn resolve_generation_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    family_id: i64,
    designation_id: i64,
    catalog_writes: &mut usize,
) -> Result<Option<i64>, AircraftHierarchyPersistenceError> {
    let Some(proposal) = input.reviewable.proposal.generation.as_ref() else {
        let decision = &input.reviewable.adjudication.generation;
        ensure_no_generation_relation_sqlite(transaction, designation_id).await?;
        ensure_decision_sqlite(
            transaction,
            observation_id,
            input.expected_catalog_revision,
            prepared,
            claim_ids,
            DecisionSpec {
                suffix: "generation",
                resolution_scope: "generation",
                entity_kind: "generation",
                action: "no_supported_selection",
                status: "approved",
                selected_entity_id: None,
                rationale: &decision.rationale,
                evidence_ids: string_ids(&decision.evidence_ids),
            },
        )
        .await?;
        return Ok(None);
    };
    let decision = &input.reviewable.adjudication.generation;
    let normalized = normalize_aircraft_retrieval_text(&proposal.display_name);
    let generation_id = match decision.action {
        EntityResolutionAction::MatchExisting => {
            let id = proposal.existing_catalog_id.expect("projection validated");
            let row = sqlx::query_as::<_, (i64, String, String)>(
                "SELECT aircraft_model_family_id, name, normalized_name FROM aircraft_generations WHERE id = ?",
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "matched generation id {id} no longer exists"
                ))
            })?;
            if row.0 != family_id || row.1 != proposal.display_name || row.2 != normalized {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "matched generation id {id} changed identity or parent"
                )));
            }
            ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "generation",
                    resolution_scope: "generation",
                    entity_kind: "generation",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(id),
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            id
        }
        EntityResolutionAction::ProposeNew => {
            let (approval_decision_id, created) = ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "generation",
                    resolution_scope: "generation",
                    entity_kind: "generation",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<_, (i64, i64, String, String)>(
                    "SELECT id, aircraft_model_family_id, name, normalized_name FROM aircraft_generations WHERE approval_decision_id = ?",
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed generation decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != family_id || row.2 != proposal.display_name || row.3 != normalized {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed generation decision points to a different canonical row"
                            .to_string(),
                    ));
                }
                row.0
            } else {
                if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                    "SELECT id FROM aircraft_generations WHERE aircraft_model_family_id = ? AND normalized_name = ?",
                )
                .bind(family_id)
                .bind(&normalized)
                .fetch_optional(&mut **transaction)
                .await?
                {
                    return Err(AircraftHierarchyPersistenceError::Collision(format!(
                        "proposed generation collides with existing catalog id {existing_id}"
                    )));
                }
                let id = sqlx::query_scalar::<_, i64>(
                    "INSERT INTO aircraft_generations (aircraft_model_family_id, name, normalized_name, ordinal, approval_decision_id) VALUES (?, ?, ?, NULL, ?) RETURNING id",
                )
                .bind(family_id)
                .bind(&proposal.display_name)
                .bind(&normalized)
                .bind(approval_decision_id)
                .fetch_one(&mut **transaction)
                .await?;
                *catalog_writes += 1;
                id
            }
        }
        _ => {
            return Err(AircraftHierarchyPersistenceError::Invalid(
                "generation decision is not persistable".to_string(),
            ));
        }
    };
    let relation_exists = sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS (SELECT 1 FROM aircraft_generation_designations WHERE aircraft_generation_id = ? AND aircraft_designation_id = ?)",
    )
    .bind(generation_id)
    .bind(designation_id)
    .fetch_one(&mut **transaction)
    .await?
        != 0;
    if !relation_exists {
        let (link_decision_id, created) = ensure_decision_sqlite(
            transaction,
            observation_id,
            input.expected_catalog_revision,
            prepared,
            claim_ids,
            DecisionSpec {
                suffix: "generation_designation",
                resolution_scope: "generation",
                entity_kind: "generation_designation",
                action: "approve_new",
                status: "approved",
                selected_entity_id: None,
                rationale:
                    "verified hierarchy relates this generation to this certified designation",
                evidence_ids: string_ids(&decision.evidence_ids),
            },
        )
        .await?;
        if !created {
            return Err(AircraftHierarchyPersistenceError::Collision(
                "replayed generation/designation decision has no canonical relation".to_string(),
            ));
        }
        sqlx::query(
            "INSERT INTO aircraft_generation_designations (aircraft_generation_id, aircraft_designation_id, approval_decision_id) VALUES (?, ?, ?)",
        )
        .bind(generation_id)
        .bind(designation_id)
        .bind(link_decision_id)
        .execute(&mut **transaction)
        .await?;
        *catalog_writes += 1;
    } else if decision.action == EntityResolutionAction::ProposeNew {
        let (_, created) = ensure_decision_sqlite(
            transaction,
            observation_id,
            input.expected_catalog_revision,
            prepared,
            claim_ids,
            DecisionSpec {
                suffix: "generation_designation",
                resolution_scope: "generation",
                entity_kind: "generation_designation",
                action: "approve_new",
                status: "approved",
                selected_entity_id: None,
                rationale:
                    "verified hierarchy relates this generation to this certified designation",
                evidence_ids: string_ids(&decision.evidence_ids),
            },
        )
        .await?;
        if created {
            return Err(AircraftHierarchyPersistenceError::Collision(
                "existing generation/designation relation lacks its replayed approval decision"
                    .to_string(),
            ));
        }
    }
    Ok(Some(generation_id))
}

async fn ensure_no_generation_relation_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    designation_id: i64,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relation_exists = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM aircraft_generation_designations
          WHERE aircraft_designation_id = ?
        )
        "#,
    )
    .bind(designation_id)
    .fetch_one(&mut **transaction)
    .await?
        != 0;
    if relation_exists {
        return Err(
            AircraftHierarchyPersistenceError::OptionalSelectionInvalidated {
                dimension: "generation",
                reason: format!(
                    "the current catalog relates a generation to designation id {designation_id}"
                ),
            },
        );
    }
    Ok(())
}

async fn resolve_package_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    family_id: i64,
    designation_id: i64,
    generation_id: Option<i64>,
) -> Result<Option<i64>, AircraftHierarchyPersistenceError> {
    let Some(proposal) = input.reviewable.proposal.tier.as_ref() else {
        let decision = &input.reviewable.adjudication.package;
        ensure_no_applicable_trim_tier_sqlite(
            transaction,
            designation_id,
            generation_id,
            input.observation.model_year,
        )
        .await?;
        ensure_decision_sqlite(
            transaction,
            observation_id,
            input.expected_catalog_revision,
            prepared,
            claim_ids,
            DecisionSpec {
                suffix: "package",
                resolution_scope: "package",
                entity_kind: "package",
                action: "no_supported_selection",
                status: "approved",
                selected_entity_id: None,
                rationale: &decision.rationale,
                evidence_ids: string_ids(&decision.evidence_ids),
            },
        )
        .await?;
        return Ok(None);
    };
    let decision = &input.reviewable.adjudication.package;
    if decision.action != EntityResolutionAction::MatchExisting {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "new packages require typed kind and applicability evidence not present in a hierarchy review"
                .to_string(),
        ));
    }
    let id = proposal.existing_catalog_id.expect("projection validated");
    let row = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT aircraft_model_family_id, name, normalized_name FROM aircraft_factory_packages WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| {
        AircraftHierarchyPersistenceError::Collision(format!(
            "matched aircraft package id {id} no longer exists"
        ))
    })?;
    if row.0 != family_id
        || row.1 != proposal.display_name
        || row.2 != normalize_aircraft_retrieval_text(&proposal.display_name)
    {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "matched aircraft package id {id} changed identity or parent"
        )));
    }
    let applicable = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT EXISTS (
          SELECT 1 FROM aircraft_package_applicability
          WHERE aircraft_factory_package_id = ?
            AND aircraft_designation_id = ?
            AND (aircraft_generation_id IS NULL OR aircraft_generation_id = ?)
            AND (valid_from_model_year IS NULL OR valid_from_model_year <= ?)
            AND (valid_to_model_year IS NULL OR valid_to_model_year >= ?)
        )
        "#,
    )
    .bind(id)
    .bind(designation_id)
    .bind(generation_id)
    .bind(input.observation.model_year)
    .bind(input.observation.model_year)
    .fetch_one(&mut **transaction)
    .await?
        != 0;
    if !applicable {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "selected package lacks approved applicability for this designation/generation/year"
                .to_string(),
        ));
    }
    ensure_decision_sqlite(
        transaction,
        observation_id,
        input.expected_catalog_revision,
        prepared,
        claim_ids,
        DecisionSpec {
            suffix: "package",
            resolution_scope: "package",
            entity_kind: "package",
            action: "match_existing",
            status: "approved",
            selected_entity_id: Some(id),
            rationale: &decision.rationale,
            evidence_ids: string_ids(&decision.evidence_ids),
        },
    )
    .await?;
    Ok(Some(id))
}

fn alias_evidence_ids(relationship: &super::FaaMakeRelationshipDecision) -> Vec<&str> {
    relationship
        .evidence_ids
        .iter()
        .chain(&relationship.applicability_evidence_ids)
        .map(String::as_str)
        .collect()
}

fn family_label_evidence_ids(relationship: &super::FamilyLabelRelationshipDecision) -> Vec<&str> {
    relationship
        .evidence_ids
        .iter()
        .chain(&relationship.applicability_evidence_ids)
        .map(String::as_str)
        .collect()
}

fn tcds_family_evidence_ids(reviewable: &ReviewableAircraftHierarchy) -> Vec<&str> {
    if reviewable.adjudication.family_label_relationship.action
        != FamilyLabelRelationshipAction::MatchFaaTypeCertificateFamily
    {
        return Vec::new();
    }
    reviewable
        .adjudication
        .family_label_relationship
        .evidence_ids
        .iter()
        .chain(
            &reviewable
                .adjudication
                .family_label_relationship
                .applicability_evidence_ids,
        )
        .map(String::as_str)
        .filter(|evidence_id| is_server_faa_drs_evidence_id(evidence_id))
        .collect()
}

fn designation_evidence_ids(reviewable: &ReviewableAircraftHierarchy) -> Vec<&str> {
    reviewable
        .adjudication
        .designation
        .evidence_ids
        .iter()
        .map(String::as_str)
        .chain(tcds_family_evidence_ids(reviewable))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn contains_exact_numeric_year_token(value: &str, year: i64) -> bool {
    let needle = year.to_string();
    value.match_indices(&needle).any(|(offset, _)| {
        let before = value[..offset].chars().next_back();
        let after = value[offset + needle.len()..].chars().next();
        before.is_none_or(|character| !character.is_ascii_alphanumeric())
            && after.is_none_or(|character| !character.is_ascii_alphanumeric())
    })
}

fn validate_proposed_alias_year_evidence(
    input: &PersistReviewableAircraftHierarchy<'_>,
    alias_kind: &str,
    valid_from_model_year: Option<i64>,
    valid_to_model_year: Option<i64>,
    applicability_evidence_ids: &[String],
) -> Result<(), AircraftHierarchyPersistenceError> {
    let (first_year, last_year) =
        valid_from_model_year
            .zip(valid_to_model_year)
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Invalid(format!(
                    "new {alias_kind} alias requires finite first and last model-year bounds"
                ))
            })?;
    if !(1900..=2200).contains(&first_year)
        || !(1900..=2200).contains(&last_year)
        || first_year > last_year
    {
        return Err(AircraftHierarchyPersistenceError::Invalid(format!(
            "new {alias_kind} alias requires ordered model-year bounds in 1900..=2200"
        )));
    }

    let verified_ids = input
        .reviewable
        .verification
        .verified_evidence_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for year in [first_year, last_year] {
        let has_direct_primary_year_claim =
            input.reviewable.proposal.evidence.iter().any(|claim| {
                applicability_evidence_ids
                    .iter()
                    .any(|evidence_id| evidence_id == &claim.evidence_id)
                    && verified_ids.contains(claim.evidence_id.as_str())
                    && !is_reserved_server_faa_evidence_id(&claim.evidence_id)
                    && matches!(
                        claim.source_kind,
                        EvidenceSourceKind::Manufacturer
                            | EvidenceSourceKind::ManufacturerServicePublication
                    )
                    && claim
                        .supports
                        .contains(&EvidenceClaimKind::ProductionApplicability)
                    && contains_exact_numeric_year_token(&claim.evidence_excerpt, year)
            });
        if !has_direct_primary_year_claim {
            return Err(AircraftHierarchyPersistenceError::Invalid(format!(
                "new {alias_kind} alias model-year bound {year} is not an exact numeric year token in cited, verified direct-primary production-applicability evidence"
            )));
        }
    }
    Ok(())
}

fn validate_alias_year_scope(
    input: &PersistReviewableAircraftHierarchy<'_>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relationship = &input.reviewable.adjudication.faa_make_relationship;
    let mut years = vec![input.observation.model_year];
    if let Some(year) = input.grounding.year_manufactured {
        years.push(i64::from(year));
    }
    if years.iter().any(|year| {
        relationship
            .valid_from_model_year
            .is_some_and(|first| first > *year)
            || relationship
                .valid_to_model_year
                .is_some_and(|last| last < *year)
    }) {
        return Err(AircraftHierarchyPersistenceError::Faa(
            "FAA make alias applicability does not cover the listing and FAA manufacture years"
                .to_string(),
        ));
    }
    if relationship
        .valid_from_model_year
        .zip(relationship.valid_to_model_year)
        .is_some_and(|(first, last)| first > last)
    {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "FAA make alias applicability has a reversed year range".to_string(),
        ));
    }
    Ok(())
}

fn validate_family_label_year_scope(
    input: &PersistReviewableAircraftHierarchy<'_>,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relationship = &input.reviewable.adjudication.family_label_relationship;
    if relationship
        .valid_from_model_year
        .is_some_and(|year| !(1900..=2200).contains(&year))
        || relationship
            .valid_to_model_year
            .is_some_and(|year| !(1900..=2200).contains(&year))
        || relationship
            .valid_from_model_year
            .zip(relationship.valid_to_model_year)
            .is_some_and(|(first, last)| first > last)
    {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "family-label alias applicability has invalid or reversed year bounds".to_string(),
        ));
    }
    if relationship
        .valid_from_model_year
        .is_some_and(|first| first > input.observation.model_year)
        || relationship
            .valid_to_model_year
            .is_some_and(|last| last < input.observation.model_year)
    {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "family-label alias applicability does not cover the listing model year".to_string(),
        ));
    }
    Ok(())
}

async fn resolve_faa_make_alias_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    make_id: i64,
    catalog_writes: &mut usize,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relationship = &input.reviewable.adjudication.faa_make_relationship;
    match relationship.action {
        FaaMakeRelationshipAction::ExactCanonicalLabel => Ok(()),
        FaaMakeRelationshipAction::MatchApprovedAlias => {
            validate_alias_year_scope(input)?;
            let alias_id = relationship.existing_alias_id.ok_or_else(|| {
                AircraftHierarchyPersistenceError::Invalid(
                    "approved-alias match requires an exact alias id".to_string(),
                )
            })?;
            let row = sqlx::query_as::<
                _,
                (
                    i64,
                    String,
                    String,
                    Option<i64>,
                    Option<i64>,
                    Option<String>,
                ),
            >(
                r#"
                SELECT alias.aircraft_make_id, alias.alias, alias.normalized_alias,
                       alias.valid_from_model_year, alias.valid_to_model_year,
                       market.code
                FROM aircraft_make_aliases alias
                LEFT JOIN aircraft_markets market
                  ON market.id = alias.aircraft_market_id
                WHERE alias.id = ?
                "#,
            )
            .bind(alias_id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "approved FAA make alias id {alias_id} no longer exists"
                ))
            })?;
            if row.0 != make_id
                || row.1 != relationship.faa_manufacturer_name
                || row.2 != normalize_aircraft_retrieval_text(&relationship.faa_manufacturer_name)
                || row.3 != relationship.valid_from_model_year
                || row.4 != relationship.valid_to_model_year
                || !matches!(row.5.as_deref(), None | Some("GLOBAL") | Some("US"))
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "approved FAA make alias id {alias_id} changed identity, owner, scope, or market"
                )));
            }
            ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "faa_make_alias",
                    resolution_scope: "make",
                    entity_kind: "alias",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(alias_id),
                    rationale: &relationship.rationale,
                    evidence_ids: alias_evidence_ids(relationship),
                },
            )
            .await?;
            Ok(())
        }
        FaaMakeRelationshipAction::ProposeAlias => {
            validate_alias_year_scope(input)?;
            validate_proposed_alias_year_evidence(
                input,
                "FAA make",
                relationship.valid_from_model_year,
                relationship.valid_to_model_year,
                &relationship.applicability_evidence_ids,
            )?;
            if relationship.existing_alias_id.is_some()
                || relationship.evidence_ids.is_empty()
                || relationship.applicability_evidence_ids.is_empty()
            {
                return Err(AircraftHierarchyPersistenceError::Invalid(
                    "new FAA make alias requires separate identity and applicability evidence and no existing id"
                        .to_string(),
                ));
            }
            let normalized = normalize_aircraft_retrieval_text(&relationship.faa_manufacturer_name);
            let (approval_decision_id, created) = ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "faa_make_alias",
                    resolution_scope: "make",
                    entity_kind: "alias",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &relationship.rationale,
                    evidence_ids: alias_evidence_ids(relationship),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<
                    _,
                    (i64, i64, String, String, Option<i64>, Option<i64>, String),
                >(
                    r#"
                    SELECT alias.id, alias.aircraft_make_id, alias.alias,
                           alias.normalized_alias, alias.valid_from_model_year,
                           alias.valid_to_model_year, market.code
                    FROM aircraft_make_aliases alias
                    JOIN aircraft_markets market
                      ON market.id = alias.aircraft_market_id
                    WHERE alias.approval_decision_id = ?
                    "#,
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed FAA make alias decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != make_id
                    || row.2 != relationship.faa_manufacturer_name
                    || row.3 != normalized
                    || row.4 != relationship.valid_from_model_year
                    || row.5 != relationship.valid_to_model_year
                    || row.6 != "US"
                {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed FAA make alias differs from its approved decision".to_string(),
                    ));
                }
                return Ok(());
            }
            let alias_collision = sqlx::query_scalar::<_, i64>(
                r#"
                SELECT id FROM aircraft_make_aliases
                WHERE normalized_alias = ?
                ORDER BY id LIMIT 1
                "#,
            )
            .bind(&normalized)
            .fetch_optional(&mut **transaction)
            .await?;
            if let Some(existing_id) = alias_collision {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed FAA make alias collides with existing alias id {existing_id}"
                )));
            }
            let make_collision = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM aircraft_makes WHERE normalized_name = ? AND id <> ? ORDER BY id LIMIT 1",
            )
            .bind(normalize_aircraft_retrieval_text(
                &relationship.faa_manufacturer_name,
            ))
            .bind(make_id)
            .fetch_optional(&mut **transaction)
            .await?;
            if let Some(existing_id) = make_collision {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed FAA make alias collides with canonical make id {existing_id}"
                )));
            }
            let us_market_id =
                sqlx::query_scalar::<_, i64>("SELECT id FROM aircraft_markets WHERE code = 'US'")
                    .fetch_one(&mut **transaction)
                    .await?;
            sqlx::query(
                r#"
                INSERT INTO aircraft_make_aliases (
                  aircraft_make_id, alias, normalized_alias,
                  valid_from_model_year, valid_to_model_year,
                  aircraft_market_id, approval_decision_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(make_id)
            .bind(&relationship.faa_manufacturer_name)
            .bind(&normalized)
            .bind(relationship.valid_from_model_year)
            .bind(relationship.valid_to_model_year)
            .bind(us_market_id)
            .bind(approval_decision_id)
            .execute(&mut **transaction)
            .await?;
            *catalog_writes += 1;
            Ok(())
        }
        FaaMakeRelationshipAction::MatchTcdsMakeLineage => Ok(()),
        FaaMakeRelationshipAction::Unresolved => Err(AircraftHierarchyPersistenceError::Invalid(
            "FAA make relationship is unresolved".to_string(),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_family_label_relationship_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    make_id: i64,
    family_id: i64,
    family_name: &str,
    catalog_writes: &mut usize,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relationship = &input.reviewable.adjudication.family_label_relationship;
    if relationship.canonical_family_name.trim() != family_name.trim() {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "family-label relationship changed canonical family before persistence".to_string(),
        ));
    }
    match relationship.action {
        FamilyLabelRelationshipAction::ExactCanonicalLabel => Ok(()),
        FamilyLabelRelationshipAction::MatchManufacturerSeriesFamily => {
            ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family_label_manufacturer_series",
                    resolution_scope: "family",
                    entity_kind: "family",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(family_id),
                    rationale: &relationship.rationale,
                    evidence_ids: family_label_evidence_ids(relationship),
                },
            )
            .await?;
            Ok(())
        }
        FamilyLabelRelationshipAction::MatchApprovedAlias => {
            validate_family_label_year_scope(input)?;
            let alias_id = relationship.existing_alias_id.ok_or_else(|| {
                AircraftHierarchyPersistenceError::Invalid(
                    "approved family-label alias match requires an exact alias id".to_string(),
                )
            })?;
            let row = sqlx::query_as::<
                _,
                (
                    i64,
                    String,
                    String,
                    Option<i64>,
                    Option<i64>,
                    Option<String>,
                ),
            >(
                r#"
                SELECT alias.aircraft_model_family_id, alias.alias,
                       alias.normalized_alias, alias.valid_from_model_year,
                       alias.valid_to_model_year, market.code
                FROM aircraft_family_aliases alias
                LEFT JOIN aircraft_markets market
                  ON market.id = alias.aircraft_market_id
                WHERE alias.id = ?
                "#,
            )
            .bind(alias_id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "approved family-label alias id {alias_id} no longer exists"
                ))
            })?;
            if row.0 != family_id
                || row.1 != relationship.observed_family_label
                || row.2 != normalize_aircraft_retrieval_text(&relationship.observed_family_label)
                || row.3 != relationship.valid_from_model_year
                || row.4 != relationship.valid_to_model_year
                || !matches!(row.5.as_deref(), None | Some("GLOBAL") | Some("US"))
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "approved family-label alias id {alias_id} changed identity, owner, scope, or market"
                )));
            }
            ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family_label_alias",
                    resolution_scope: "family",
                    entity_kind: "alias",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(alias_id),
                    rationale: &relationship.rationale,
                    evidence_ids: family_label_evidence_ids(relationship),
                },
            )
            .await?;
            Ok(())
        }
        FamilyLabelRelationshipAction::ProposeAlias => {
            validate_family_label_year_scope(input)?;
            validate_proposed_alias_year_evidence(
                input,
                "family-label",
                relationship.valid_from_model_year,
                relationship.valid_to_model_year,
                &relationship.applicability_evidence_ids,
            )?;
            if relationship.existing_alias_id.is_some()
                || relationship.evidence_ids.is_empty()
                || relationship.applicability_evidence_ids.is_empty()
            {
                return Err(AircraftHierarchyPersistenceError::Invalid(
                    "new family-label alias requires separate identity and applicability evidence and no existing id"
                        .to_string(),
                ));
            }
            let normalized = normalize_aircraft_retrieval_text(&relationship.observed_family_label);
            let (approval_decision_id, created) = ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family_label_alias",
                    resolution_scope: "family",
                    entity_kind: "alias",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &relationship.rationale,
                    evidence_ids: family_label_evidence_ids(relationship),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<
                    _,
                    (i64, i64, String, String, Option<i64>, Option<i64>, String),
                >(
                    r#"
                    SELECT alias.id, alias.aircraft_model_family_id, alias.alias,
                           alias.normalized_alias, alias.valid_from_model_year,
                           alias.valid_to_model_year, market.code
                    FROM aircraft_family_aliases alias
                    JOIN aircraft_markets market
                      ON market.id = alias.aircraft_market_id
                    WHERE alias.approval_decision_id = ?
                    "#,
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed family-label alias decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != family_id
                    || row.2 != relationship.observed_family_label
                    || row.3 != normalized
                    || row.4 != relationship.valid_from_model_year
                    || row.5 != relationship.valid_to_model_year
                    || row.6 != "US"
                {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed family-label alias differs from its approved decision"
                            .to_string(),
                    ));
                }
                return Ok(());
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                r#"
                SELECT alias.id
                FROM aircraft_family_aliases alias
                JOIN aircraft_model_families owner
                  ON owner.id = alias.aircraft_model_family_id
                WHERE owner.aircraft_make_id = ?
                  AND alias.normalized_alias = ?
                ORDER BY alias.id
                LIMIT 1
                "#,
            )
            .bind(make_id)
            .bind(&normalized)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed family-label alias collides with existing same-make alias id {existing_id}"
                )));
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                r#"
                SELECT id
                FROM aircraft_model_families
                WHERE aircraft_make_id = ?
                  AND normalized_name = ?
                  AND id <> ?
                ORDER BY id
                LIMIT 1
                "#,
            )
            .bind(make_id)
            .bind(normalize_aircraft_retrieval_text(
                &relationship.observed_family_label,
            ))
            .bind(family_id)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed family-label alias collides with canonical same-make family id {existing_id}"
                )));
            }
            let us_market_id =
                sqlx::query_scalar::<_, i64>("SELECT id FROM aircraft_markets WHERE code = 'US'")
                    .fetch_one(&mut **transaction)
                    .await?;
            sqlx::query(
                r#"
                INSERT INTO aircraft_family_aliases (
                  aircraft_model_family_id, alias, normalized_alias,
                  valid_from_model_year, valid_to_model_year,
                  aircraft_market_id, approval_decision_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(family_id)
            .bind(&relationship.observed_family_label)
            .bind(&normalized)
            .bind(relationship.valid_from_model_year)
            .bind(relationship.valid_to_model_year)
            .bind(us_market_id)
            .bind(approval_decision_id)
            .execute(&mut **transaction)
            .await?;
            *catalog_writes += 1;
            Ok(())
        }
        FamilyLabelRelationshipAction::MatchFaaTypeCertificateFamily => {
            let evidence_ids = family_label_evidence_ids(relationship);
            ensure_decision_sqlite(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family_label_type_certificate",
                    resolution_scope: "family",
                    entity_kind: "family",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(family_id),
                    rationale: &relationship.rationale,
                    evidence_ids,
                },
            )
            .await?;
            Ok(())
        }
        FamilyLabelRelationshipAction::Unresolved => {
            Err(AircraftHierarchyPersistenceError::Invalid(
                "retained family label relationship is unresolved".to_string(),
            ))
        }
    }
}

async fn current_assignment_key_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    listing_id: i64,
) -> Result<Option<CurrentAssignmentKey>, sqlx::Error> {
    sqlx::query_as::<_, CurrentAssignmentKey>(
        r#"
        SELECT assignment.id AS assignment_id,
               assignment.aircraft_make_id,
               assignment.aircraft_model_family_id,
               assignment.aircraft_designation_id,
               assignment.aircraft_generation_id,
               assignment.aircraft_factory_package_id,
               assignment.faa_registry_snapshot_id,
               assignment.faa_n_number,
               assignment.faa_source_record_sha256
        FROM aircraft_sale_listing_current_identity_assignments current_assignment
        JOIN aircraft_sale_listing_identity_assignments assignment
          ON assignment.id = current_assignment.identity_assignment_id
         AND assignment.aircraft_sale_listing_id =
             current_assignment.aircraft_sale_listing_id
        WHERE current_assignment.aircraft_sale_listing_id = ?
        "#,
    )
    .bind(listing_id)
    .fetch_optional(&mut **transaction)
    .await
}

fn current_assignment_is_exact(
    current: &CurrentAssignmentKey,
    resolved: &ResolvedHierarchy,
    grounding: &AircraftGrounding,
) -> bool {
    current.aircraft_make_id == resolved.hierarchy.manufacturer_id
        && current.aircraft_model_family_id == resolved.hierarchy.model_family_id
        && current.aircraft_designation_id == resolved.hierarchy.certified_variant_id
        && current.aircraft_generation_id == resolved.hierarchy.generation_id
        && current.aircraft_factory_package_id == resolved.hierarchy.tier_id
        && current.faa_registry_snapshot_id == grounding.snapshot.id
        && current.faa_n_number == grounding.n_number
        && current.faa_source_record_sha256 == grounding.source_record_sha256
}

async fn ensure_assignment_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &PersistReviewableAircraftHierarchy<'_>,
    resolved: &ResolvedHierarchy,
) -> Result<i64, AircraftHierarchyPersistenceError> {
    let current = current_assignment_key_sqlite(transaction, input.listing_id).await?;
    if let Some(current) = &current {
        if current_assignment_is_exact(current, resolved, input.grounding) {
            let projected = sqlx::query_scalar::<_, i64>(
                r#"
                SELECT EXISTS (
                  SELECT 1
                  FROM aircraft_sale_listing_exact_compatibility_projections
                  WHERE listing_id = ? AND identity_assignment_id = ?
                )
                "#,
            )
            .bind(input.listing_id)
            .bind(current.assignment_id)
            .fetch_one(&mut **transaction)
            .await?
                != 0;
            if !projected {
                return Err(AircraftHierarchyPersistenceError::Assignment(
                    "current exact assignment is missing its valuation projection".to_string(),
                ));
            }
            return Ok(current.assignment_id);
        }
    }
    let reference = input.grounding.aircraft.as_ref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "current FAA grounding lacks reference identity".to_string(),
        )
    })?;
    let faa_make = reference.manufacturer_name.as_deref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "current FAA grounding lacks manufacturer".to_string(),
        )
    })?;
    let faa_model = reference.model_name.as_deref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "current FAA grounding lacks model designation".to_string(),
        )
    })?;
    let evidence = FaaIdentityEvidence::new(input.grounding, faa_make, faa_model);
    Ok(persist_assignment_sqlite_in_transaction(
        transaction,
        input.listing_id,
        &resolved.promotion_candidate(),
        current.map(|assignment| assignment.assignment_id),
        input.grounding,
        &evidence,
    )
    .await?)
}

async fn load_persisted_assignment_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    listing_id: i64,
    assignment_id: i64,
) -> Result<CanonicalAircraftIdentityAssignment, AircraftHierarchyPersistenceError> {
    let row = sqlx::query_as::<_, PersistedAssignmentRow>(
        r#"
        SELECT assignment.id AS assignment_id,
               assignment.aircraft_sale_listing_id,
               assignment.supersedes_assignment_id,
               assignment.aircraft_make_id,
               make.name AS make_name,
               assignment.aircraft_model_family_id,
               family.name AS family_name,
               assignment.aircraft_designation_id,
               designation.official_designation,
               assignment.aircraft_generation_id,
               assignment.aircraft_factory_package_id,
               assignment.identity_decision_id,
               assignment.identity_evidence_claim_id,
               assignment.faa_registry_snapshot_id,
               assignment.faa_n_number,
               binding.faa_aircraft_code,
               assignment.faa_source_record_sha256,
               assignment.created_at
        FROM aircraft_sale_listing_current_identity_assignments current_assignment
        JOIN aircraft_sale_listing_identity_assignments assignment
          ON assignment.id = current_assignment.identity_assignment_id
         AND assignment.aircraft_sale_listing_id =
             current_assignment.aircraft_sale_listing_id
        JOIN aircraft_makes make ON make.id = assignment.aircraft_make_id
        JOIN aircraft_model_families family
          ON family.id = assignment.aircraft_model_family_id
        JOIN aircraft_designations designation
          ON designation.id = assignment.aircraft_designation_id
        JOIN faa_registry_aircraft aircraft
          ON aircraft.snapshot_id = assignment.faa_registry_snapshot_id
         AND aircraft.n_number = assignment.faa_n_number
         AND aircraft.source_record_sha256 = assignment.faa_source_record_sha256
        JOIN faa_registry_snapshots snapshot
          ON snapshot.id = assignment.faa_registry_snapshot_id
        JOIN aircraft_designation_faa_bindings binding
          ON binding.faa_snapshot_date = snapshot.snapshot_date
         AND binding.faa_archive_sha256 = snapshot.archive_sha256
         AND binding.faa_aircraft_code = aircraft.aircraft_code
         AND binding.aircraft_designation_id = assignment.aircraft_designation_id
        WHERE current_assignment.aircraft_sale_listing_id = ?
          AND assignment.id = ?
        "#,
    )
    .bind(listing_id)
    .bind(assignment_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| {
        AircraftHierarchyPersistenceError::Assignment(format!(
            "transaction did not retain exact current assignment {assignment_id} for listing {listing_id}"
        ))
    })?;
    Ok(row.into_public())
}

async fn persist_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    _db: &AppDb,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    observation_id: i64,
    current_revision: &str,
) -> Result<ResolvedHierarchy, AircraftHierarchyPersistenceError> {
    let replay = approval_case_exists_postgres(transaction, prepared).await?;
    if !replay && current_revision != input.expected_catalog_revision {
        return Err(AircraftHierarchyPersistenceError::StaleCatalog {
            expected: input.expected_catalog_revision.to_string(),
            actual: current_revision.to_string(),
        });
    }
    let claim_ids = if replay {
        BTreeMap::new()
    } else {
        persist_claims_postgres(transaction, prepared, input.grounding).await?
    };
    let mut catalog_writes = 0;
    reject_tcds_holder_make_duplicate_postgres(transaction, input).await?;
    let (make_id, make_name, make_decision_id) = resolve_make_postgres(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        &mut catalog_writes,
    )
    .await?;
    resolve_faa_make_alias_postgres(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        make_id,
        &mut catalog_writes,
    )
    .await?;
    let (family_id, family_name) = resolve_family_postgres(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        make_id,
        &mut catalog_writes,
    )
    .await?;
    resolve_family_label_relationship_postgres(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        make_id,
        family_id,
        &family_name,
        &mut catalog_writes,
    )
    .await?;
    let (designation_id, official_designation, designation_approval_decision_id) =
        resolve_designation_postgres(
            transaction,
            observation_id,
            input,
            prepared,
            &claim_ids,
            family_id,
            &mut catalog_writes,
        )
        .await?;
    if !replay {
        attach_tcds_claims_to_designation_approval_postgres(
            transaction,
            designation_id,
            designation_approval_decision_id,
            input.reviewable,
            prepared,
            &claim_ids,
        )
        .await?;
    }
    persist_tcds_make_lineage_postgres(
        transaction,
        input,
        prepared,
        &claim_ids,
        make_id,
        make_decision_id,
        designation_id,
        replay,
        &mut catalog_writes,
    )
    .await?;
    let generation_id = resolve_generation_postgres(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        family_id,
        designation_id,
        &mut catalog_writes,
    )
    .await?;
    let package_id = resolve_package_postgres(
        transaction,
        observation_id,
        input,
        prepared,
        &claim_ids,
        family_id,
        designation_id,
        generation_id,
    )
    .await?;
    let mut resolved = ResolvedHierarchy {
        hierarchy: AircraftHierarchy {
            manufacturer_id: make_id,
            model_family_id: family_id,
            certified_variant_id: designation_id,
            generation_id,
            tier_id: package_id,
        },
        make_name,
        family_name,
        official_designation,
        designation_approval_decision_id,
        catalog_writes,
        idempotent_replay: replay,
        assignment: None,
    };
    let assignment_id = ensure_assignment_postgres(transaction, input, &resolved).await?;
    resolved.assignment = Some(
        load_persisted_assignment_postgres(transaction, input.listing_id, assignment_id).await?,
    );
    Ok(resolved)
}

async fn approval_case_exists_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    prepared: &PreparedApproval,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM aircraft_identity_resolution_cases WHERE job_fingerprint = $1)",
    )
    .bind(decision_fingerprint(prepared, "make"))
    .fetch_one(&mut **transaction)
    .await
}

async fn resolve_make_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    catalog_writes: &mut usize,
) -> Result<(i64, String, i64), AircraftHierarchyPersistenceError> {
    let proposal = &input.reviewable.proposal.manufacturer;
    let decision = &input.reviewable.adjudication.make;
    match decision.action {
        EntityResolutionAction::MatchExisting => {
            let id = proposal.existing_catalog_id.expect("projection validated");
            let row = sqlx::query_as::<_, (String, String)>(
                "SELECT name, normalized_name FROM aircraft_makes WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft make id {id} no longer exists"
                ))
            })?;
            if row.0 != proposal.display_name
                || row.1 != normalize_aircraft_retrieval_text(&proposal.display_name)
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft make id {id} changed identity"
                )));
            }
            let (approval_decision_id, _) = ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "make",
                    resolution_scope: "make",
                    entity_kind: "make",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(id),
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            Ok((id, row.0, approval_decision_id))
        }
        EntityResolutionAction::ProposeNew => {
            let normalized = normalize_aircraft_retrieval_text(&proposal.display_name);
            let (approval_decision_id, created) = ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "make",
                    resolution_scope: "make",
                    entity_kind: "make",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<_, (i64, String, String)>(
                    "SELECT id, name, normalized_name FROM aircraft_makes WHERE approval_decision_id = $1",
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed make decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != proposal.display_name || row.2 != normalized {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed make decision points to a different canonical row".to_string(),
                    ));
                }
                return Ok((row.0, row.1, approval_decision_id));
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM aircraft_makes WHERE normalized_name = $1",
            )
            .bind(&normalized)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed make collides with existing catalog id {existing_id}"
                )));
            }
            let id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO aircraft_makes (name, normalized_name, approval_decision_id) VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(&proposal.display_name)
            .bind(&normalized)
            .bind(approval_decision_id)
            .fetch_one(&mut **transaction)
            .await?;
            *catalog_writes += 1;
            Ok((id, proposal.display_name.clone(), approval_decision_id))
        }
        _ => Err(AircraftHierarchyPersistenceError::Invalid(
            "make decision is not persistable".to_string(),
        )),
    }
}

async fn resolve_family_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    make_id: i64,
    catalog_writes: &mut usize,
) -> Result<(i64, String), AircraftHierarchyPersistenceError> {
    let proposal = &input.reviewable.proposal.model_family;
    let decision = &input.reviewable.adjudication.family;
    let normalized = normalize_aircraft_retrieval_text(&proposal.display_name);
    match decision.action {
        EntityResolutionAction::MatchExisting => {
            let id = proposal.existing_catalog_id.expect("projection validated");
            let row = sqlx::query_as::<_, (i64, String, String)>(
                "SELECT aircraft_make_id, name, normalized_name FROM aircraft_model_families WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft family id {id} no longer exists"
                ))
            })?;
            if row.0 != make_id || row.1 != proposal.display_name || row.2 != normalized {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft family id {id} changed identity or parent"
                )));
            }
            ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family",
                    resolution_scope: "family",
                    entity_kind: "family",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(id),
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            Ok((id, row.1))
        }
        EntityResolutionAction::ProposeNew => {
            let (approval_decision_id, created) = ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family",
                    resolution_scope: "family",
                    entity_kind: "family",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<_, (i64, i64, String, String)>(
                    "SELECT id, aircraft_make_id, name, normalized_name FROM aircraft_model_families WHERE approval_decision_id = $1",
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed family decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != make_id || row.2 != proposal.display_name || row.3 != normalized {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed family decision points to a different canonical row".to_string(),
                    ));
                }
                return Ok((row.0, row.2));
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM aircraft_model_families WHERE aircraft_make_id = $1 AND normalized_name = $2",
            )
            .bind(make_id)
            .bind(&normalized)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed family collides with existing catalog id {existing_id}"
                )));
            }
            let id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO aircraft_model_families (aircraft_make_id, name, normalized_name, approval_decision_id) VALUES ($1, $2, $3, $4) RETURNING id",
            )
            .bind(make_id)
            .bind(&proposal.display_name)
            .bind(&normalized)
            .bind(approval_decision_id)
            .fetch_one(&mut **transaction)
            .await?;
            *catalog_writes += 1;
            Ok((id, proposal.display_name.clone()))
        }
        _ => Err(AircraftHierarchyPersistenceError::Invalid(
            "family decision is not persistable".to_string(),
        )),
    }
}

async fn resolve_designation_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    family_id: i64,
    catalog_writes: &mut usize,
) -> Result<(i64, String, i64), AircraftHierarchyPersistenceError> {
    let proposal = &input.reviewable.proposal.certified_variant;
    let decision = &input.reviewable.adjudication.designation;
    let official = proposal
        .authoritative_designator
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AircraftHierarchyPersistenceError::Invalid(
                "certified designation requires an authoritative designator".to_string(),
            )
        })?;
    let normalized = normalize_aircraft_designator_retrieval_key(official);
    match decision.action {
        EntityResolutionAction::MatchExisting => {
            let id = proposal.existing_catalog_id.expect("projection validated");
            let row = sqlx::query_as::<_, (i64, String, String, String, i64)>(
                r#"
                SELECT aircraft_model_family_id, official_designation,
                       normalized_official_designation, display_name,
                       approval_decision_id
                FROM aircraft_designations WHERE id = $1
                "#,
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft designation id {id} no longer exists"
                ))
            })?;
            if row.0 != family_id
                || row.1 != official
                || row.2 != normalized
                || row.3 != proposal.display_name
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "matched aircraft designation id {id} changed identity or parent"
                )));
            }
            ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "designation",
                    resolution_scope: "designation",
                    entity_kind: "designation",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(id),
                    rationale: &decision.rationale,
                    evidence_ids: designation_evidence_ids(input.reviewable),
                },
            )
            .await?;
            Ok((id, row.1, row.4))
        }
        EntityResolutionAction::ProposeNew => {
            let (approval_decision_id, created) = ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "designation",
                    resolution_scope: "designation",
                    entity_kind: "designation",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &decision.rationale,
                    evidence_ids: designation_evidence_ids(input.reviewable),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<_, (i64, i64, String, String, String)>(
                    r#"
                    SELECT id, aircraft_model_family_id, official_designation,
                           normalized_official_designation, display_name
                    FROM aircraft_designations WHERE approval_decision_id = $1
                    "#,
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed designation decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != family_id
                    || row.2 != official
                    || row.3 != normalized
                    || row.4 != proposal.display_name
                {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed designation decision points to a different canonical row"
                            .to_string(),
                    ));
                }
                return Ok((row.0, row.2, approval_decision_id));
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM aircraft_designations WHERE aircraft_model_family_id = $1 AND normalized_official_designation = $2",
            )
            .bind(family_id)
            .bind(&normalized)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed designation collides with existing catalog id {existing_id}"
                )));
            }
            let id = sqlx::query_scalar::<_, i64>(
                r#"
                INSERT INTO aircraft_designations (
                  aircraft_model_family_id, official_designation,
                  normalized_official_designation, display_name,
                  approval_decision_id
                ) VALUES ($1, $2, $3, $4, $5) RETURNING id
                "#,
            )
            .bind(family_id)
            .bind(official)
            .bind(&normalized)
            .bind(&proposal.display_name)
            .bind(approval_decision_id)
            .fetch_one(&mut **transaction)
            .await?;
            *catalog_writes += 1;
            Ok((id, official.to_string(), approval_decision_id))
        }
        _ => Err(AircraftHierarchyPersistenceError::Invalid(
            "designation decision is not persistable".to_string(),
        )),
    }
}

async fn ensure_no_applicable_trim_tier_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    designation_id: i64,
    generation_id: Option<i64>,
    model_year: i64,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relation_exists = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM aircraft_package_applicability applicability
          JOIN aircraft_factory_packages package
            ON package.id = applicability.aircraft_factory_package_id
          WHERE package.package_kind = 'trim_tier'
            AND applicability.aircraft_designation_id = ?
            AND (
              applicability.valid_from_model_year IS NULL
              OR applicability.valid_from_model_year <= ?
            )
            AND (
              applicability.valid_to_model_year IS NULL
              OR applicability.valid_to_model_year >= ?
            )
            AND (
              applicability.aircraft_generation_id IS NULL
              OR applicability.aircraft_generation_id = ?
            )
        )
        "#,
    )
    .bind(designation_id)
    .bind(model_year)
    .bind(model_year)
    .bind(generation_id)
    .fetch_one(&mut **transaction)
    .await?
        != 0;
    if relation_exists {
        return Err(
            AircraftHierarchyPersistenceError::OptionalSelectionInvalidated {
                dimension: "package",
                reason: format!(
                    "the current catalog has an applicable trim-tier package for designation id {designation_id}, generation id {generation_id:?}, model year {model_year}"
                ),
            },
        );
    }
    Ok(())
}

async fn resolve_generation_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    family_id: i64,
    designation_id: i64,
    catalog_writes: &mut usize,
) -> Result<Option<i64>, AircraftHierarchyPersistenceError> {
    let Some(proposal) = input.reviewable.proposal.generation.as_ref() else {
        let decision = &input.reviewable.adjudication.generation;
        ensure_no_generation_relation_postgres(transaction, designation_id).await?;
        ensure_decision_postgres(
            transaction,
            observation_id,
            input.expected_catalog_revision,
            prepared,
            claim_ids,
            DecisionSpec {
                suffix: "generation",
                resolution_scope: "generation",
                entity_kind: "generation",
                action: "no_supported_selection",
                status: "approved",
                selected_entity_id: None,
                rationale: &decision.rationale,
                evidence_ids: string_ids(&decision.evidence_ids),
            },
        )
        .await?;
        return Ok(None);
    };
    let decision = &input.reviewable.adjudication.generation;
    let normalized = normalize_aircraft_retrieval_text(&proposal.display_name);
    let generation_id = match decision.action {
        EntityResolutionAction::MatchExisting => {
            let id = proposal.existing_catalog_id.expect("projection validated");
            let row = sqlx::query_as::<_, (i64, String, String)>(
                "SELECT aircraft_model_family_id, name, normalized_name FROM aircraft_generations WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "matched generation id {id} no longer exists"
                ))
            })?;
            if row.0 != family_id || row.1 != proposal.display_name || row.2 != normalized {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "matched generation id {id} changed identity or parent"
                )));
            }
            ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "generation",
                    resolution_scope: "generation",
                    entity_kind: "generation",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(id),
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            id
        }
        EntityResolutionAction::ProposeNew => {
            let (approval_decision_id, created) = ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "generation",
                    resolution_scope: "generation",
                    entity_kind: "generation",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &decision.rationale,
                    evidence_ids: string_ids(&decision.evidence_ids),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<_, (i64, i64, String, String)>(
                    "SELECT id, aircraft_model_family_id, name, normalized_name FROM aircraft_generations WHERE approval_decision_id = $1",
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed generation decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != family_id || row.2 != proposal.display_name || row.3 != normalized {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed generation decision points to a different canonical row"
                            .to_string(),
                    ));
                }
                row.0
            } else {
                if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                    "SELECT id FROM aircraft_generations WHERE aircraft_model_family_id = $1 AND normalized_name = $2",
                )
                .bind(family_id)
                .bind(&normalized)
                .fetch_optional(&mut **transaction)
                .await?
                {
                    return Err(AircraftHierarchyPersistenceError::Collision(format!(
                        "proposed generation collides with existing catalog id {existing_id}"
                    )));
                }
                let id = sqlx::query_scalar::<_, i64>(
                    "INSERT INTO aircraft_generations (aircraft_model_family_id, name, normalized_name, ordinal, approval_decision_id) VALUES ($1, $2, $3, NULL, $4) RETURNING id",
                )
                .bind(family_id)
                .bind(&proposal.display_name)
                .bind(&normalized)
                .bind(approval_decision_id)
                .fetch_one(&mut **transaction)
                .await?;
                *catalog_writes += 1;
                id
            }
        }
        _ => {
            return Err(AircraftHierarchyPersistenceError::Invalid(
                "generation decision is not persistable".to_string(),
            ));
        }
    };
    let relation_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM aircraft_generation_designations WHERE aircraft_generation_id = $1 AND aircraft_designation_id = $2)",
    )
    .bind(generation_id)
    .bind(designation_id)
    .fetch_one(&mut **transaction)
    .await?;
    if !relation_exists {
        let (link_decision_id, created) = ensure_decision_postgres(
            transaction,
            observation_id,
            input.expected_catalog_revision,
            prepared,
            claim_ids,
            DecisionSpec {
                suffix: "generation_designation",
                resolution_scope: "generation",
                entity_kind: "generation_designation",
                action: "approve_new",
                status: "approved",
                selected_entity_id: None,
                rationale:
                    "verified hierarchy relates this generation to this certified designation",
                evidence_ids: string_ids(&decision.evidence_ids),
            },
        )
        .await?;
        if !created {
            return Err(AircraftHierarchyPersistenceError::Collision(
                "replayed generation/designation decision has no canonical relation".to_string(),
            ));
        }
        sqlx::query(
            "INSERT INTO aircraft_generation_designations (aircraft_generation_id, aircraft_designation_id, approval_decision_id) VALUES ($1, $2, $3)",
        )
        .bind(generation_id)
        .bind(designation_id)
        .bind(link_decision_id)
        .execute(&mut **transaction)
        .await?;
        *catalog_writes += 1;
    } else if decision.action == EntityResolutionAction::ProposeNew {
        let (_, created) = ensure_decision_postgres(
            transaction,
            observation_id,
            input.expected_catalog_revision,
            prepared,
            claim_ids,
            DecisionSpec {
                suffix: "generation_designation",
                resolution_scope: "generation",
                entity_kind: "generation_designation",
                action: "approve_new",
                status: "approved",
                selected_entity_id: None,
                rationale:
                    "verified hierarchy relates this generation to this certified designation",
                evidence_ids: string_ids(&decision.evidence_ids),
            },
        )
        .await?;
        if created {
            return Err(AircraftHierarchyPersistenceError::Collision(
                "existing generation/designation relation lacks its replayed approval decision"
                    .to_string(),
            ));
        }
    }
    Ok(Some(generation_id))
}

async fn ensure_no_generation_relation_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    designation_id: i64,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relation_exists = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM aircraft_generation_designations
          WHERE aircraft_designation_id = $1
        )
        "#,
    )
    .bind(designation_id)
    .fetch_one(&mut **transaction)
    .await?;
    if relation_exists {
        return Err(
            AircraftHierarchyPersistenceError::OptionalSelectionInvalidated {
                dimension: "generation",
                reason: format!(
                    "the current catalog relates a generation to designation id {designation_id}"
                ),
            },
        );
    }
    Ok(())
}

async fn resolve_package_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    family_id: i64,
    designation_id: i64,
    generation_id: Option<i64>,
) -> Result<Option<i64>, AircraftHierarchyPersistenceError> {
    let Some(proposal) = input.reviewable.proposal.tier.as_ref() else {
        let decision = &input.reviewable.adjudication.package;
        ensure_no_applicable_trim_tier_postgres(
            transaction,
            designation_id,
            generation_id,
            input.observation.model_year,
        )
        .await?;
        ensure_decision_postgres(
            transaction,
            observation_id,
            input.expected_catalog_revision,
            prepared,
            claim_ids,
            DecisionSpec {
                suffix: "package",
                resolution_scope: "package",
                entity_kind: "package",
                action: "no_supported_selection",
                status: "approved",
                selected_entity_id: None,
                rationale: &decision.rationale,
                evidence_ids: string_ids(&decision.evidence_ids),
            },
        )
        .await?;
        return Ok(None);
    };
    let decision = &input.reviewable.adjudication.package;
    if decision.action != EntityResolutionAction::MatchExisting {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "new packages require typed kind and applicability evidence not present in a hierarchy review"
                .to_string(),
        ));
    }
    let id = proposal.existing_catalog_id.expect("projection validated");
    let row = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT aircraft_model_family_id, name, normalized_name FROM aircraft_factory_packages WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| {
        AircraftHierarchyPersistenceError::Collision(format!(
            "matched aircraft package id {id} no longer exists"
        ))
    })?;
    if row.0 != family_id
        || row.1 != proposal.display_name
        || row.2 != normalize_aircraft_retrieval_text(&proposal.display_name)
    {
        return Err(AircraftHierarchyPersistenceError::Collision(format!(
            "matched aircraft package id {id} changed identity or parent"
        )));
    }
    let applicable = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
          SELECT 1 FROM aircraft_package_applicability
          WHERE aircraft_factory_package_id = $1
            AND aircraft_designation_id = $2
            AND (aircraft_generation_id IS NULL OR aircraft_generation_id = $3)
            AND (valid_from_model_year IS NULL OR valid_from_model_year <= $4)
            AND (valid_to_model_year IS NULL OR valid_to_model_year >= $5)
        )
        "#,
    )
    .bind(id)
    .bind(designation_id)
    .bind(generation_id)
    .bind(input.observation.model_year)
    .bind(input.observation.model_year)
    .fetch_one(&mut **transaction)
    .await?;
    if !applicable {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "selected package lacks approved applicability for this designation/generation/year"
                .to_string(),
        ));
    }
    ensure_decision_postgres(
        transaction,
        observation_id,
        input.expected_catalog_revision,
        prepared,
        claim_ids,
        DecisionSpec {
            suffix: "package",
            resolution_scope: "package",
            entity_kind: "package",
            action: "match_existing",
            status: "approved",
            selected_entity_id: Some(id),
            rationale: &decision.rationale,
            evidence_ids: string_ids(&decision.evidence_ids),
        },
    )
    .await?;
    Ok(Some(id))
}

async fn ensure_no_applicable_trim_tier_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    designation_id: i64,
    generation_id: Option<i64>,
    model_year: i64,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relation_exists = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM aircraft_package_applicability applicability
          JOIN aircraft_factory_packages package
            ON package.id = applicability.aircraft_factory_package_id
          WHERE package.package_kind = 'trim_tier'
            AND applicability.aircraft_designation_id = $1
            AND (
              applicability.valid_from_model_year IS NULL
              OR applicability.valid_from_model_year <= $2
            )
            AND (
              applicability.valid_to_model_year IS NULL
              OR applicability.valid_to_model_year >= $2
            )
            AND (
              applicability.aircraft_generation_id IS NULL
              OR applicability.aircraft_generation_id = $3
            )
        )
        "#,
    )
    .bind(designation_id)
    .bind(model_year)
    .bind(generation_id)
    .fetch_one(&mut **transaction)
    .await?;
    if relation_exists {
        return Err(
            AircraftHierarchyPersistenceError::OptionalSelectionInvalidated {
                dimension: "package",
                reason: format!(
                    "the current catalog has an applicable trim-tier package for designation id {designation_id}, generation id {generation_id:?}, model year {model_year}"
                ),
            },
        );
    }
    Ok(())
}

async fn resolve_faa_make_alias_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    make_id: i64,
    catalog_writes: &mut usize,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relationship = &input.reviewable.adjudication.faa_make_relationship;
    match relationship.action {
        FaaMakeRelationshipAction::ExactCanonicalLabel => Ok(()),
        FaaMakeRelationshipAction::MatchApprovedAlias => {
            validate_alias_year_scope(input)?;
            let alias_id = relationship.existing_alias_id.ok_or_else(|| {
                AircraftHierarchyPersistenceError::Invalid(
                    "approved-alias match requires an exact alias id".to_string(),
                )
            })?;
            let row = sqlx::query_as::<
                _,
                (
                    i64,
                    String,
                    String,
                    Option<i64>,
                    Option<i64>,
                    Option<String>,
                ),
            >(
                r#"
                SELECT alias.aircraft_make_id, alias.alias, alias.normalized_alias,
                       alias.valid_from_model_year, alias.valid_to_model_year,
                       market.code
                FROM aircraft_make_aliases alias
                LEFT JOIN aircraft_markets market
                  ON market.id = alias.aircraft_market_id
                WHERE alias.id = $1
                "#,
            )
            .bind(alias_id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "approved FAA make alias id {alias_id} no longer exists"
                ))
            })?;
            if row.0 != make_id
                || row.1 != relationship.faa_manufacturer_name
                || row.2 != normalize_aircraft_retrieval_text(&relationship.faa_manufacturer_name)
                || row.3 != relationship.valid_from_model_year
                || row.4 != relationship.valid_to_model_year
                || !matches!(row.5.as_deref(), None | Some("GLOBAL") | Some("US"))
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "approved FAA make alias id {alias_id} changed identity, owner, scope, or market"
                )));
            }
            ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "faa_make_alias",
                    resolution_scope: "make",
                    entity_kind: "alias",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(alias_id),
                    rationale: &relationship.rationale,
                    evidence_ids: alias_evidence_ids(relationship),
                },
            )
            .await?;
            Ok(())
        }
        FaaMakeRelationshipAction::ProposeAlias => {
            validate_alias_year_scope(input)?;
            validate_proposed_alias_year_evidence(
                input,
                "FAA make",
                relationship.valid_from_model_year,
                relationship.valid_to_model_year,
                &relationship.applicability_evidence_ids,
            )?;
            if relationship.existing_alias_id.is_some()
                || relationship.evidence_ids.is_empty()
                || relationship.applicability_evidence_ids.is_empty()
            {
                return Err(AircraftHierarchyPersistenceError::Invalid(
                    "new FAA make alias requires separate identity and applicability evidence and no existing id"
                        .to_string(),
                ));
            }
            let normalized = normalize_aircraft_retrieval_text(&relationship.faa_manufacturer_name);
            let (approval_decision_id, created) = ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "faa_make_alias",
                    resolution_scope: "make",
                    entity_kind: "alias",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &relationship.rationale,
                    evidence_ids: alias_evidence_ids(relationship),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<
                    _,
                    (i64, i64, String, String, Option<i64>, Option<i64>, String),
                >(
                    r#"
                    SELECT alias.id, alias.aircraft_make_id, alias.alias,
                           alias.normalized_alias, alias.valid_from_model_year,
                           alias.valid_to_model_year, market.code
                    FROM aircraft_make_aliases alias
                    JOIN aircraft_markets market
                      ON market.id = alias.aircraft_market_id
                    WHERE alias.approval_decision_id = $1
                    "#,
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed FAA make alias decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != make_id
                    || row.2 != relationship.faa_manufacturer_name
                    || row.3 != normalized
                    || row.4 != relationship.valid_from_model_year
                    || row.5 != relationship.valid_to_model_year
                    || row.6 != "US"
                {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed FAA make alias differs from its approved decision".to_string(),
                    ));
                }
                return Ok(());
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM aircraft_make_aliases WHERE normalized_alias = $1 ORDER BY id LIMIT 1",
            )
            .bind(&normalized)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed FAA make alias collides with existing alias id {existing_id}"
                )));
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM aircraft_makes WHERE normalized_name = $1 AND id <> $2 ORDER BY id LIMIT 1",
            )
            .bind(normalize_aircraft_retrieval_text(
                &relationship.faa_manufacturer_name,
            ))
            .bind(make_id)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed FAA make alias collides with canonical make id {existing_id}"
                )));
            }
            let us_market_id =
                sqlx::query_scalar::<_, i64>("SELECT id FROM aircraft_markets WHERE code = 'US'")
                    .fetch_one(&mut **transaction)
                    .await?;
            sqlx::query(
                r#"
                INSERT INTO aircraft_make_aliases (
                  aircraft_make_id, alias, normalized_alias,
                  valid_from_model_year, valid_to_model_year,
                  aircraft_market_id, approval_decision_id
                ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
            )
            .bind(make_id)
            .bind(&relationship.faa_manufacturer_name)
            .bind(&normalized)
            .bind(relationship.valid_from_model_year)
            .bind(relationship.valid_to_model_year)
            .bind(us_market_id)
            .bind(approval_decision_id)
            .execute(&mut **transaction)
            .await?;
            *catalog_writes += 1;
            Ok(())
        }
        FaaMakeRelationshipAction::MatchTcdsMakeLineage => Ok(()),
        FaaMakeRelationshipAction::Unresolved => Err(AircraftHierarchyPersistenceError::Invalid(
            "FAA make relationship is unresolved".to_string(),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_family_label_relationship_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    observation_id: i64,
    input: &PersistReviewableAircraftHierarchy<'_>,
    prepared: &PreparedApproval,
    claim_ids: &BTreeMap<String, i64>,
    make_id: i64,
    family_id: i64,
    family_name: &str,
    catalog_writes: &mut usize,
) -> Result<(), AircraftHierarchyPersistenceError> {
    let relationship = &input.reviewable.adjudication.family_label_relationship;
    if relationship.canonical_family_name.trim() != family_name.trim() {
        return Err(AircraftHierarchyPersistenceError::Invalid(
            "family-label relationship changed canonical family before persistence".to_string(),
        ));
    }
    match relationship.action {
        FamilyLabelRelationshipAction::ExactCanonicalLabel => Ok(()),
        FamilyLabelRelationshipAction::MatchManufacturerSeriesFamily => {
            ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family_label_manufacturer_series",
                    resolution_scope: "family",
                    entity_kind: "family",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(family_id),
                    rationale: &relationship.rationale,
                    evidence_ids: family_label_evidence_ids(relationship),
                },
            )
            .await?;
            Ok(())
        }
        FamilyLabelRelationshipAction::MatchApprovedAlias => {
            validate_family_label_year_scope(input)?;
            let alias_id = relationship.existing_alias_id.ok_or_else(|| {
                AircraftHierarchyPersistenceError::Invalid(
                    "approved family-label alias match requires an exact alias id".to_string(),
                )
            })?;
            let row = sqlx::query_as::<
                _,
                (
                    i64,
                    String,
                    String,
                    Option<i64>,
                    Option<i64>,
                    Option<String>,
                ),
            >(
                r#"
                SELECT alias.aircraft_model_family_id, alias.alias,
                       alias.normalized_alias, alias.valid_from_model_year,
                       alias.valid_to_model_year, market.code
                FROM aircraft_family_aliases alias
                LEFT JOIN aircraft_markets market
                  ON market.id = alias.aircraft_market_id
                WHERE alias.id = $1
                "#,
            )
            .bind(alias_id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| {
                AircraftHierarchyPersistenceError::Collision(format!(
                    "approved family-label alias id {alias_id} no longer exists"
                ))
            })?;
            if row.0 != family_id
                || row.1 != relationship.observed_family_label
                || row.2 != normalize_aircraft_retrieval_text(&relationship.observed_family_label)
                || row.3 != relationship.valid_from_model_year
                || row.4 != relationship.valid_to_model_year
                || !matches!(row.5.as_deref(), None | Some("GLOBAL") | Some("US"))
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "approved family-label alias id {alias_id} changed identity, owner, scope, or market"
                )));
            }
            ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family_label_alias",
                    resolution_scope: "family",
                    entity_kind: "alias",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(alias_id),
                    rationale: &relationship.rationale,
                    evidence_ids: family_label_evidence_ids(relationship),
                },
            )
            .await?;
            Ok(())
        }
        FamilyLabelRelationshipAction::ProposeAlias => {
            validate_family_label_year_scope(input)?;
            validate_proposed_alias_year_evidence(
                input,
                "family-label",
                relationship.valid_from_model_year,
                relationship.valid_to_model_year,
                &relationship.applicability_evidence_ids,
            )?;
            if relationship.existing_alias_id.is_some()
                || relationship.evidence_ids.is_empty()
                || relationship.applicability_evidence_ids.is_empty()
            {
                return Err(AircraftHierarchyPersistenceError::Invalid(
                    "new family-label alias requires separate identity and applicability evidence and no existing id"
                        .to_string(),
                ));
            }
            let normalized = normalize_aircraft_retrieval_text(&relationship.observed_family_label);
            let (approval_decision_id, created) = ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family_label_alias",
                    resolution_scope: "family",
                    entity_kind: "alias",
                    action: "approve_new",
                    status: "approved",
                    selected_entity_id: None,
                    rationale: &relationship.rationale,
                    evidence_ids: family_label_evidence_ids(relationship),
                },
            )
            .await?;
            if !created {
                let row = sqlx::query_as::<
                    _,
                    (i64, i64, String, String, Option<i64>, Option<i64>, String),
                >(
                    r#"
                    SELECT alias.id, alias.aircraft_model_family_id, alias.alias,
                           alias.normalized_alias, alias.valid_from_model_year,
                           alias.valid_to_model_year, market.code
                    FROM aircraft_family_aliases alias
                    JOIN aircraft_markets market
                      ON market.id = alias.aircraft_market_id
                    WHERE alias.approval_decision_id = $1
                    "#,
                )
                .bind(approval_decision_id)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or_else(|| {
                    AircraftHierarchyPersistenceError::Collision(
                        "replayed family-label alias decision has no canonical row".to_string(),
                    )
                })?;
                if row.1 != family_id
                    || row.2 != relationship.observed_family_label
                    || row.3 != normalized
                    || row.4 != relationship.valid_from_model_year
                    || row.5 != relationship.valid_to_model_year
                    || row.6 != "US"
                {
                    return Err(AircraftHierarchyPersistenceError::Collision(
                        "replayed family-label alias differs from its approved decision"
                            .to_string(),
                    ));
                }
                return Ok(());
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                r#"
                SELECT alias.id
                FROM aircraft_family_aliases alias
                JOIN aircraft_model_families owner
                  ON owner.id = alias.aircraft_model_family_id
                WHERE owner.aircraft_make_id = $1
                  AND alias.normalized_alias = $2
                ORDER BY alias.id
                LIMIT 1
                "#,
            )
            .bind(make_id)
            .bind(&normalized)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed family-label alias collides with existing same-make alias id {existing_id}"
                )));
            }
            if let Some(existing_id) = sqlx::query_scalar::<_, i64>(
                r#"
                SELECT id
                FROM aircraft_model_families
                WHERE aircraft_make_id = $1
                  AND normalized_name = $2
                  AND id <> $3
                ORDER BY id
                LIMIT 1
                "#,
            )
            .bind(make_id)
            .bind(normalize_aircraft_retrieval_text(
                &relationship.observed_family_label,
            ))
            .bind(family_id)
            .fetch_optional(&mut **transaction)
            .await?
            {
                return Err(AircraftHierarchyPersistenceError::Collision(format!(
                    "proposed family-label alias collides with canonical same-make family id {existing_id}"
                )));
            }
            let us_market_id =
                sqlx::query_scalar::<_, i64>("SELECT id FROM aircraft_markets WHERE code = 'US'")
                    .fetch_one(&mut **transaction)
                    .await?;
            sqlx::query(
                r#"
                INSERT INTO aircraft_family_aliases (
                  aircraft_model_family_id, alias, normalized_alias,
                  valid_from_model_year, valid_to_model_year,
                  aircraft_market_id, approval_decision_id
                ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
            )
            .bind(family_id)
            .bind(&relationship.observed_family_label)
            .bind(&normalized)
            .bind(relationship.valid_from_model_year)
            .bind(relationship.valid_to_model_year)
            .bind(us_market_id)
            .bind(approval_decision_id)
            .execute(&mut **transaction)
            .await?;
            *catalog_writes += 1;
            Ok(())
        }
        FamilyLabelRelationshipAction::MatchFaaTypeCertificateFamily => {
            let evidence_ids = family_label_evidence_ids(relationship);
            ensure_decision_postgres(
                transaction,
                observation_id,
                input.expected_catalog_revision,
                prepared,
                claim_ids,
                DecisionSpec {
                    suffix: "family_label_type_certificate",
                    resolution_scope: "family",
                    entity_kind: "family",
                    action: "match_existing",
                    status: "approved",
                    selected_entity_id: Some(family_id),
                    rationale: &relationship.rationale,
                    evidence_ids,
                },
            )
            .await?;
            Ok(())
        }
        FamilyLabelRelationshipAction::Unresolved => {
            Err(AircraftHierarchyPersistenceError::Invalid(
                "retained family label relationship is unresolved".to_string(),
            ))
        }
    }
}

async fn current_assignment_key_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    listing_id: i64,
) -> Result<Option<CurrentAssignmentKey>, sqlx::Error> {
    sqlx::query_as::<_, CurrentAssignmentKey>(
        r#"
        SELECT assignment.id AS assignment_id,
               assignment.aircraft_make_id,
               assignment.aircraft_model_family_id,
               assignment.aircraft_designation_id,
               assignment.aircraft_generation_id,
               assignment.aircraft_factory_package_id,
               assignment.faa_registry_snapshot_id,
               assignment.faa_n_number,
               assignment.faa_source_record_sha256
        FROM aircraft_sale_listing_current_identity_assignments current_assignment
        JOIN aircraft_sale_listing_identity_assignments assignment
          ON assignment.id = current_assignment.identity_assignment_id
         AND assignment.aircraft_sale_listing_id =
             current_assignment.aircraft_sale_listing_id
        WHERE current_assignment.aircraft_sale_listing_id = $1
        FOR SHARE OF assignment
        "#,
    )
    .bind(listing_id)
    .fetch_optional(&mut **transaction)
    .await
}

async fn ensure_assignment_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &PersistReviewableAircraftHierarchy<'_>,
    resolved: &ResolvedHierarchy,
) -> Result<i64, AircraftHierarchyPersistenceError> {
    let current = current_assignment_key_postgres(transaction, input.listing_id).await?;
    if let Some(current) = &current {
        if current_assignment_is_exact(current, resolved, input.grounding) {
            let projected = sqlx::query_scalar::<_, bool>(
                r#"
                SELECT EXISTS (
                  SELECT 1
                  FROM aircraft_sale_listing_exact_compatibility_projections
                  WHERE listing_id = $1 AND identity_assignment_id = $2
                )
                "#,
            )
            .bind(input.listing_id)
            .bind(current.assignment_id)
            .fetch_one(&mut **transaction)
            .await?;
            if !projected {
                return Err(AircraftHierarchyPersistenceError::Assignment(
                    "current exact assignment is missing its valuation projection".to_string(),
                ));
            }
            return Ok(current.assignment_id);
        }
    }
    let reference = input.grounding.aircraft.as_ref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "current FAA grounding lacks reference identity".to_string(),
        )
    })?;
    let faa_make = reference.manufacturer_name.as_deref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "current FAA grounding lacks manufacturer".to_string(),
        )
    })?;
    let faa_model = reference.model_name.as_deref().ok_or_else(|| {
        AircraftHierarchyPersistenceError::Faa(
            "current FAA grounding lacks model designation".to_string(),
        )
    })?;
    let evidence = FaaIdentityEvidence::new(input.grounding, faa_make, faa_model);
    Ok(persist_assignment_postgres_in_transaction(
        transaction,
        input.listing_id,
        &resolved.promotion_candidate(),
        current.map(|assignment| assignment.assignment_id),
        input.grounding,
        &evidence,
    )
    .await?)
}

async fn load_persisted_assignment_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    listing_id: i64,
    assignment_id: i64,
) -> Result<CanonicalAircraftIdentityAssignment, AircraftHierarchyPersistenceError> {
    let row = sqlx::query_as::<_, PersistedAssignmentRow>(
        r#"
        SELECT assignment.id AS assignment_id,
               assignment.aircraft_sale_listing_id,
               assignment.supersedes_assignment_id,
               assignment.aircraft_make_id,
               make.name AS make_name,
               assignment.aircraft_model_family_id,
               family.name AS family_name,
               assignment.aircraft_designation_id,
               designation.official_designation,
               assignment.aircraft_generation_id,
               assignment.aircraft_factory_package_id,
               assignment.identity_decision_id,
               assignment.identity_evidence_claim_id,
               assignment.faa_registry_snapshot_id,
               assignment.faa_n_number,
               binding.faa_aircraft_code,
               assignment.faa_source_record_sha256,
               assignment.created_at::text AS created_at
        FROM aircraft_sale_listing_current_identity_assignments current_assignment
        JOIN aircraft_sale_listing_identity_assignments assignment
          ON assignment.id = current_assignment.identity_assignment_id
         AND assignment.aircraft_sale_listing_id =
             current_assignment.aircraft_sale_listing_id
        JOIN aircraft_makes make ON make.id = assignment.aircraft_make_id
        JOIN aircraft_model_families family
          ON family.id = assignment.aircraft_model_family_id
        JOIN aircraft_designations designation
          ON designation.id = assignment.aircraft_designation_id
        JOIN faa_registry_aircraft aircraft
          ON aircraft.snapshot_id = assignment.faa_registry_snapshot_id
         AND aircraft.n_number = assignment.faa_n_number
         AND aircraft.source_record_sha256 = assignment.faa_source_record_sha256
        JOIN faa_registry_snapshots snapshot
          ON snapshot.id = assignment.faa_registry_snapshot_id
        JOIN aircraft_designation_faa_bindings binding
          ON binding.faa_snapshot_date = snapshot.snapshot_date
         AND binding.faa_archive_sha256 = snapshot.archive_sha256
         AND binding.faa_aircraft_code = aircraft.aircraft_code
         AND binding.aircraft_designation_id = assignment.aircraft_designation_id
        WHERE current_assignment.aircraft_sale_listing_id = $1
          AND assignment.id = $2
        FOR SHARE OF current_assignment, assignment, make, family,
                     designation, aircraft, snapshot, binding
        "#,
    )
    .bind(listing_id)
    .bind(assignment_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| {
        AircraftHierarchyPersistenceError::Assignment(format!(
            "transaction did not retain exact current assignment {assignment_id} for listing {listing_id}"
        ))
    })?;
    Ok(row.into_public())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::catalog::{AircraftHierarchyProposal, EvidenceSourceKind};
    use crate::aircraft::curation::regulator::{
        SelectedTcdsExcerpt, TcdsFamilyBinding, TcdsSerialEligibility,
    };
    use crate::aircraft::curation::{
        approved_aircraft_catalog_revision, AircraftHierarchyAdjudication,
        AircraftHierarchyVerification, AircraftIdentityEvidenceResearch, CatalogEntityDecision,
        FaaMakeRelationshipDecision, FamilyLabelRelationshipDecision, ServerFaaIdentityEvidence,
        ServerFaaObservationBinding, ServerFetchedAircraftSourceProofs,
    };
    use crate::aircraft::faa::{
        require_listing_faa_admission, store_release, AircraftRecord, AircraftReference,
        MemberProvenance, Release, ReleaseMetadata, TargetCoverage, AIRCRAFT_MEMBER_NAME,
        ENGINE_MEMBER_NAME, MASTER_MEMBER_NAME,
    };
    use crate::aircraft::observations::load_aircraft_identity_observations;
    use crate::gemini::curation::workflow::{SourceEvidenceProof, SourceEvidenceSpanProof};

    struct Fixture {
        db: AppDb,
        listing_id: i64,
        observation: AircraftIdentityObservation,
        grounding: AircraftGrounding,
    }

    #[test]
    fn proposed_alias_year_support_requires_a_standalone_numeric_token() {
        assert!(contains_exact_numeric_year_token(
            "Production years: 2022–2023.",
            2022
        ));
        assert!(!contains_exact_numeric_year_token(
            "The model MY2022 remains in production.",
            2022
        ));
        assert!(!contains_exact_numeric_year_token(
            "The product code is 12022.",
            2022
        ));
    }

    #[test]
    fn only_exact_official_faa_drs_download_urls_are_accepted() {
        let guid = "01234567-89ab-cdef-0123-456789abcdef";
        assert!(is_official_faa_drs_download_url(&format!(
            "https://drs.faa.gov/api/drs/data-pull/download/{guid}"
        )));
        for rejected in [
            format!("http://drs.faa.gov/api/drs/data-pull/download/{guid}"),
            format!("https://evil.example/api/drs/data-pull/download/{guid}"),
            format!("https://drs.faa.gov/api/content/alf/{guid}"),
            format!("https://drs.faa.gov/api/drs/data-pull/download/{guid}/extra"),
            format!("https://drs.faa.gov/api/drs/data-pull/download/{guid}?download=1"),
            "https://drs.faa.gov/api/drs/data-pull/download/not-a-guid".to_string(),
        ] {
            assert!(
                !is_official_faa_drs_download_url(&rejected),
                "unexpectedly accepted {rejected}"
            );
        }
    }

    fn release(
        n_number: &str,
        serial: &str,
        faa_make: &str,
        faa_model: &str,
        snapshot_digest: char,
    ) -> Release {
        release_on(
            "2026-07-20",
            n_number,
            serial,
            faa_make,
            faa_model,
            snapshot_digest,
        )
    }

    fn release_on(
        snapshot_date: &str,
        n_number: &str,
        serial: &str,
        faa_make: &str,
        faa_model: &str,
        snapshot_digest: char,
    ) -> Release {
        Release {
            metadata: ReleaseMetadata::official(
                snapshot_date,
                snapshot_digest.to_string().repeat(64),
            ),
            source_manifest_sha256: "b".repeat(64),
            target_set_sha256: "c".repeat(64),
            master: MemberProvenance {
                member_name: MASTER_MEMBER_NAME.to_string(),
                sha256: "d".repeat(64),
            },
            aircraft_reference: MemberProvenance {
                member_name: AIRCRAFT_MEMBER_NAME.to_string(),
                sha256: "e".repeat(64),
            },
            engine_reference: MemberProvenance {
                member_name: ENGINE_MEMBER_NAME.to_string(),
                sha256: "f".repeat(64),
            },
            coverage: vec![TargetCoverage {
                n_number: n_number.to_string(),
                matched: true,
            }],
            aircraft: vec![AircraftRecord {
                n_number: n_number.to_string(),
                manufacturer_serial_raw: Some(serial.to_string()),
                manufacturer_serial_key: Some(
                    serial
                        .chars()
                        .filter(|character| character.is_ascii_alphanumeric())
                        .map(|character| character.to_ascii_uppercase())
                        .collect(),
                ),
                aircraft_code: "2072723".to_string(),
                engine_code: None,
                year_manufactured: Some(2022),
                source_record_sha256: "1".repeat(64),
            }],
            aircraft_references: vec![AircraftReference {
                aircraft_code: "2072723".to_string(),
                manufacturer_name: Some(faa_make.to_string()),
                model_name: Some(faa_model.to_string()),
                aircraft_type_code: None,
                engine_type_code: None,
                category_code: None,
                certification_indicator_code: None,
                engine_count: Some(1),
                seat_count: Some(4),
                weight_class_code: None,
                cruise_speed_mph: None,
                type_certificate_data_sheet: Some("3A13".to_string()),
                type_certificate_holder: Some("TEXTRON AVIATION INC".to_string()),
            }],
            engine_references: Vec::new(),
        }
    }

    async fn fixture(faa_make: &str, observed_make: &str) -> Fixture {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let raw_make_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES (?, ?) RETURNING id",
        )
        .bind(observed_make)
        .bind(crate::normalize::normalize_name(observed_make))
        .fetch_one(pool)
        .await
        .unwrap();
        let raw_model_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name) VALUES (?, '182', '182') RETURNING id",
        )
        .bind(raw_make_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name) VALUES (?, '182T', '182t')",
        )
        .bind(raw_model_id)
        .execute(pool)
        .await
        .unwrap();
        let pending_variant_id = sqlx::query_scalar::<_, i64>(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let listing_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours,
              registration_number, serial_number, ingestion_state
            ) VALUES (
              ?, ?, 'https://example.test/aircraft', 2022, 550000, 400,
              'N89225', '18283169', 'incomplete'
            ) RETURNING id
            "#,
        )
        .bind(pending_variant_id)
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let install_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'test-key') RETURNING id",
        )
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let html = format!(
            "<html><body><p>2022 {observed_make} 182 model 182T, N89225, serial 18283169</p></body></html>"
        );
        let html_sha = format!("{:x}", Sha256::digest(html.as_bytes()));
        let extraction = json!({
            "manufacturer": observed_make,
            "model": "182",
            "variant": "182T",
            "model_year": 2022,
            "registration_number": "N89225",
            "serial_number": "18283169",
        });
        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64,
              extracted_listing_json, canonical_listing_id
            ) VALUES (?, ?, 'https://example.test/aircraft', ?, ?, 'signature', ?, ?)
            "#,
        )
        .bind(user.id)
        .bind(install_id)
        .bind(&html)
        .bind(&html_sha)
        .bind(extraction.to_string())
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();
        store_release(&db, &release("N89225", "18283169", faa_make, "182T", 'a'))
            .await
            .unwrap();
        let grounding = require_listing_faa_admission(&db, listing_id)
            .await
            .unwrap();
        let observation = load_aircraft_identity_observations(&db, 1, Some(listing_id))
            .await
            .unwrap()
            .observations
            .pop()
            .unwrap();
        assert!(
            observation.source_excerpt_is_exact,
            "observation was not exact: {observation:#?}"
        );
        Fixture {
            db,
            listing_id,
            observation,
            grounding,
        }
    }

    async fn duplicate_listing_fixture(original: &Fixture) -> Fixture {
        let db = original.db.clone();
        let user = db.current_user(None).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let pending_variant_id = sqlx::query_scalar::<_, i64>(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let listing_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours,
              registration_number, serial_number, ingestion_state
            ) VALUES (
              ?, ?, 'https://example.test/aircraft-duplicate', 2022, 549000, 401,
              'N89225', '18283169', 'incomplete'
            ) RETURNING id
            "#,
        )
        .bind(pending_variant_id)
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let install_id =
            sqlx::query_scalar::<_, i64>("SELECT id FROM plugin_installs ORDER BY id LIMIT 1")
                .fetch_one(pool)
                .await
                .unwrap();
        let html =
            "<html><body><p>2022 CESSNA AIRCRAFT CO 182 model 182T, N89225, serial 18283169</p></body></html>";
        let html_sha = format!("{:x}", Sha256::digest(html.as_bytes()));
        let extraction = json!({
            "manufacturer": "CESSNA AIRCRAFT CO",
            "model": "182",
            "variant": "182T",
            "model_year": 2022,
            "registration_number": "N89225",
            "serial_number": "18283169",
        });
        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64,
              extracted_listing_json, canonical_listing_id
            ) VALUES (
              ?, ?, 'https://example.test/aircraft-duplicate', ?, ?,
              'signature-duplicate', ?, ?
            )
            "#,
        )
        .bind(user.id)
        .bind(install_id)
        .bind(html)
        .bind(html_sha)
        .bind(extraction.to_string())
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();
        let grounding = require_listing_faa_admission(&db, listing_id)
            .await
            .unwrap();
        let observation = load_aircraft_identity_observations(&db, 1, Some(listing_id))
            .await
            .unwrap()
            .observations
            .pop()
            .unwrap();
        assert!(observation.source_excerpt_is_exact);
        Fixture {
            db,
            listing_id,
            observation,
            grounding,
        }
    }

    async fn insert_primary_identifier(fixture: &Fixture, persisted: &PersistedAircraftHierarchy) {
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        let observation_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM aircraft_identity_observations WHERE aircraft_sale_listing_id = ? ORDER BY id LIMIT 1",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let case_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_identity_resolution_cases (
              observation_id, resolution_scope, job_fingerprint,
              catalog_revision, case_status
            ) VALUES (?, 'designation', 'test-identifier-case', ?, 'resolved')
            RETURNING id
            "#,
        )
        .bind(observation_id)
        .bind(revision)
        .fetch_one(pool)
        .await
        .unwrap();
        let decision_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_identity_decisions (
              resolution_case_id, entity_kind, decision_action, decision_status,
              selected_entity_id, decision_payload_json,
              deterministic_validation_json, deterministic_validation_passed,
              rationale, decided_at
            ) VALUES (
              ?, 'identifier', 'approve_new', 'approved', NULL, '{}',
              '{"passed":true}', 1, 'FAA primary source confirms identifier',
              CURRENT_TIMESTAMP
            ) RETURNING id
            "#,
        )
        .bind(case_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_identity_decision_claims (decision_id, evidence_claim_id, evidence_role) VALUES (?, ?, 'identity')",
        )
        .bind(decision_id)
        .bind(persisted.assignment.identity_evidence_claim_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_designation_identifiers (
              aircraft_designation_id, authority, identifier_kind,
              identifier_value, normalized_identifier_value,
              approval_decision_id
            ) VALUES (?, 'FAA', 'type_certificate_model', '182T-TC',
                      '182t tc', ?)
            "#,
        )
        .bind(persisted.hierarchy.certified_variant_id)
        .bind(decision_id)
        .execute(pool)
        .await
        .unwrap();
    }

    fn web_claim(id: &str, supports: &[EvidenceClaimKind]) -> EvidenceClaimProposal {
        let evidence_excerpt = if supports.contains(&EvidenceClaimKind::ProductionApplicability) {
            "Manufacturer primary evidence establishes production applicability from 2022 through 2022."
        } else {
            "Manufacturer primary evidence establishes the exact named relationship."
        };
        EvidenceClaimProposal {
            evidence_id: id.to_string(),
            source_url: format!("https://cessna.example.test/{id}"),
            source_title: format!("Cessna primary evidence {id}"),
            evidence_excerpt: evidence_excerpt.to_string(),
            source_kind: EvidenceSourceKind::Manufacturer,
            supports: supports.iter().copied().collect(),
        }
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

    fn direct_source_proofs(
        evidence: &[EvidenceClaimProposal],
        adjudication: &AircraftHierarchyAdjudication,
        server: &ServerFaaIdentityEvidence,
    ) -> ServerFetchedAircraftSourceProofs {
        let research = AircraftIdentityEvidenceResearch {
            subject_summary: "test aircraft hierarchy".to_string(),
            claims: evidence.to_vec(),
            family_candidates: Vec::new(),
            generation_candidates: Vec::new(),
            package_candidates: Vec::new(),
            contradictions: Vec::new(),
            unresolved_questions: Vec::new(),
        };
        let fetched = evidence
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
        ServerFetchedAircraftSourceProofs::bind_research(&research, server, &fetched)
            .expect("test evidence binds to fetched direct sources")
            .for_used_decisions(
                &research,
                server,
                &super::super::adjudication_evidence_ids(adjudication),
            )
            .expect("test direct-source proofs cover every used web claim")
    }

    fn entity_decision(
        display_name: &str,
        authoritative_designator: Option<&str>,
        evidence_ids: Vec<String>,
    ) -> CatalogEntityDecision {
        CatalogEntityDecision {
            action: EntityResolutionAction::ProposeNew,
            existing_catalog_id: None,
            display_name: Some(display_name.to_string()),
            authoritative_designator: authoritative_designator.map(str::to_string),
            evidence_ids,
            rationale: format!("primary evidence confirms {display_name}"),
        }
    }

    fn reviewable(
        fixture: &Fixture,
        canonical_make: &str,
        relationship_action: FaaMakeRelationshipAction,
        include_unused_claim: bool,
    ) -> ReviewableAircraftHierarchy {
        reviewable_for_fixtures(
            &[fixture],
            canonical_make,
            relationship_action,
            include_unused_claim,
        )
    }

    fn with_tcds_family_binding(
        fixture: &Fixture,
        mut reviewable: ReviewableAircraftHierarchy,
        canonical_family: &str,
    ) -> ReviewableAircraftHierarchy {
        let source_url = concat!(
            "https://drs.faa.gov/api/drs/data-pull/download/",
            "01234567-89ab-cdef-0123-456789abcdef"
        );
        let excerpt = |page_number, text: &str| SelectedTcdsExcerpt {
            page_number,
            excerpt: text.to_string(),
            normalized_excerpt_sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        };
        let serial_excerpt = "Serial Numbers Eligible\n182T: 18280945 and On";
        let binding = TcdsFamilyBinding {
            document_guid: "01234567-89ab-cdef-0123-456789abcdef".to_string(),
            document_url: "https://drs.faa.gov/browse/TCDSMODEL/3A13".to_string(),
            tcds_number: "3A13".to_string(),
            revision_number: Some("75".to_string()),
            revision_date: Some("2024-08-09".to_string()),
            source_url: source_url.to_string(),
            pdf_sha256: "6".repeat(64),
            exact_faa_model: "182T".to_string(),
            observed_model: fixture.observation.model.clone(),
            canonical_family_name: canonical_family.to_string(),
            faa_serial_key: fixture
                .grounding
                .manufacturer_serial_key
                .clone()
                .expect("fixture has an FAA manufacturer serial"),
            faa_model_heading: excerpt(34, "Model 182T, Skylane, 4 PCLM (Normal Category)."),
            serial_eligibility: TcdsSerialEligibility {
                page_number: 35,
                excerpt: serial_excerpt.to_string(),
                normalized_excerpt_sha256: format!(
                    "{:x}",
                    Sha256::digest(serial_excerpt.as_bytes())
                ),
                model: "182T".to_string(),
                first_serial_key: "18280945".to_string(),
                last_serial_key: None,
            },
        };
        reviewable
            .server_faa_evidence
            .attach_tcds_identity_binding(binding.identity_binding())
            .expect("synthetic TCDS identity matches the exact fixture");
        reviewable
            .server_faa_evidence
            .attach_tcds_family_binding(binding)
            .expect("synthetic TCDS binding matches the exact fixture");
        for claim in reviewable.server_faa_evidence.claims() {
            if !reviewable
                .proposal
                .evidence
                .iter()
                .any(|existing| existing.evidence_id == claim.evidence_id)
            {
                reviewable.proposal.evidence.push(claim.clone());
            }
        }
        let relationship = reviewable
            .server_faa_evidence
            .tcds_family_relationship(canonical_family)
            .expect("synthetic binding names the selected family");
        let family_evidence_ids = reviewable
            .server_faa_evidence
            .tcds_family_claim_ids()
            .expect("synthetic binding has exact claim IDs")
            .hierarchy();
        reviewable.proposal.model_family.display_name = canonical_family.to_string();
        reviewable.adjudication.family.display_name = Some(canonical_family.to_string());
        reviewable.adjudication.family.evidence_ids = family_evidence_ids;
        reviewable.adjudication.family_label_relationship = relationship;
        reviewable.verification.verified_evidence_ids = reviewable
            .proposal
            .evidence
            .iter()
            .map(|claim| claim.evidence_id.clone())
            .collect();
        reviewable.direct_source_proofs = direct_source_proofs(
            &reviewable.proposal.evidence,
            &reviewable.adjudication,
            &reviewable.server_faa_evidence,
        );
        reviewable
    }

    fn with_proposed_family_label_alias(
        mut reviewable: ReviewableAircraftHierarchy,
        observed_family_label: &str,
        canonical_family_name: &str,
        model_year: i64,
    ) -> ReviewableAircraftHierarchy {
        let identity_id = "family-label-relationship".to_string();
        let applicability_id = "family-label-applicability".to_string();
        let mut identity_claim = web_claim(&identity_id, &[EvidenceClaimKind::HierarchyIdentity]);
        identity_claim.evidence_excerpt = format!(
            "The manufacturer identifies the Cessna {canonical_family_name} {observed_family_label}."
        );
        reviewable.proposal.evidence.push(identity_claim);
        reviewable.proposal.evidence.push(web_claim(
            &applicability_id,
            &[EvidenceClaimKind::ProductionApplicability],
        ));
        reviewable.adjudication.family.display_name = Some(canonical_family_name.to_string());
        reviewable.adjudication.family.authoritative_designator = None;
        reviewable.adjudication.family.evidence_ids = vec![identity_id.clone()];
        reviewable.proposal.model_family.display_name = canonical_family_name.to_string();
        reviewable.proposal.model_family.authoritative_designator = None;
        reviewable.adjudication.family_label_relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::ProposeAlias,
            observed_family_label: observed_family_label.to_string(),
            canonical_family_name: canonical_family_name.to_string(),
            existing_alias_id: None,
            valid_from_model_year: Some(model_year),
            valid_to_model_year: Some(model_year),
            evidence_ids: vec![identity_id.clone()],
            applicability_evidence_ids: vec![applicability_id.clone()],
            rationale: "manufacturer primary evidence relates the retained label to this family"
                .to_string(),
        };
        reviewable
            .verification
            .verified_evidence_ids
            .extend([identity_id, applicability_id]);
        reviewable.direct_source_proofs = direct_source_proofs(
            &reviewable.proposal.evidence,
            &reviewable.adjudication,
            &reviewable.server_faa_evidence,
        );
        reviewable
    }

    fn with_manufacturer_series_family_relationship(
        mut reviewable: ReviewableAircraftHierarchy,
        observed_family_label: &str,
        canonical_family_name: &str,
    ) -> ReviewableAircraftHierarchy {
        let evidence_id = "family-label-manufacturer-series".to_string();
        let mut claim = web_claim(&evidence_id, &[EvidenceClaimKind::HierarchyIdentity]);
        claim.evidence_excerpt =
            format!("Today the manufacturer celebrates the Cessna {canonical_family_name} 182.");
        reviewable.proposal.evidence.push(claim);
        reviewable.adjudication.family.display_name = Some(canonical_family_name.to_string());
        reviewable.adjudication.family.authoritative_designator = None;
        reviewable.adjudication.family.evidence_ids = vec![evidence_id.clone()];
        reviewable.proposal.model_family.display_name = canonical_family_name.to_string();
        reviewable.proposal.model_family.authoritative_designator = None;
        reviewable.adjudication.family_label_relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::MatchManufacturerSeriesFamily,
            observed_family_label: observed_family_label.to_string(),
            canonical_family_name: canonical_family_name.to_string(),
            existing_alias_id: None,
            valid_from_model_year: None,
            valid_to_model_year: None,
            evidence_ids: vec![evidence_id.clone()],
            applicability_evidence_ids: Vec::new(),
            rationale:
                "manufacturer evidence co-names the FAA-bound numeric series and canonical family"
                    .to_string(),
        };
        reviewable
            .verification
            .verified_evidence_ids
            .push(evidence_id);
        reviewable.direct_source_proofs = direct_source_proofs(
            &reviewable.proposal.evidence,
            &reviewable.adjudication,
            &reviewable.server_faa_evidence,
        );
        reviewable
    }

    fn reviewable_for_fixtures(
        fixtures: &[&Fixture],
        canonical_make: &str,
        relationship_action: FaaMakeRelationshipAction,
        include_unused_claim: bool,
    ) -> ReviewableAircraftHierarchy {
        let fixture = fixtures.first().expect("at least one fixture");
        let reference = fixture.grounding.aircraft.as_ref().unwrap();
        let server = ServerFaaIdentityEvidence::new(
            "case-test",
            fixture.grounding.snapshot.clone(),
            fixtures
                .iter()
                .map(|fixture| {
                    ServerFaaObservationBinding::new(
                        fixture.listing_id,
                        fixture.observation.observation_sha256.clone(),
                        fixture.observation.manufacturer.clone(),
                        fixture.observation.model.clone(),
                        fixture.observation.variant.clone(),
                        fixture.observation.model_year,
                        fixture.grounding.clone(),
                    )
                })
                .collect(),
            reference.manufacturer_name.clone().unwrap(),
            reference.model_name.clone().unwrap(),
        )
        .unwrap();
        let make_evidence_id = server.make_claim_id().to_string();
        let designation_evidence_id = server.designation_claim_id().to_string();
        let mut evidence = server.claims().to_vec();
        let mut relationship_evidence_ids = vec![make_evidence_id.clone()];
        let mut applicability_evidence_ids = Vec::new();
        let (existing_alias_id, first_year, last_year) =
            if relationship_action == FaaMakeRelationshipAction::ProposeAlias {
                evidence.push(web_claim(
                    "brand-relationship",
                    &[EvidenceClaimKind::HierarchyIdentity],
                ));
                evidence.push(web_claim(
                    "brand-applicability",
                    &[EvidenceClaimKind::ProductionApplicability],
                ));
                relationship_evidence_ids = vec!["brand-relationship".to_string()];
                applicability_evidence_ids = vec!["brand-applicability".to_string()];
                (None, Some(2022), Some(2022))
            } else {
                (None, None, None)
            };
        if include_unused_claim {
            evidence.push(EvidenceClaimProposal {
                evidence_id: "unused-dossier-claim".to_string(),
                source_url: "https://secondary.example.test/dossier".to_string(),
                source_title: "Unused dossier context".to_string(),
                evidence_excerpt:
                    "This grounded context is valid but no final catalog decision depends on it."
                        .to_string(),
                source_kind: EvidenceSourceKind::RecognizedSecondary,
                supports: [EvidenceClaimKind::HierarchyIdentity].into_iter().collect(),
            });
        }
        let make = entity_decision(canonical_make, None, vec![make_evidence_id.clone()]);
        let family = entity_decision("182", None, vec![designation_evidence_id.clone()]);
        let designation =
            entity_decision("182T", Some("182T"), vec![designation_evidence_id.clone()]);
        let no_supported_selection = CatalogEntityDecision {
            action: EntityResolutionAction::NoSupportedSelection,
            existing_catalog_id: None,
            display_name: None,
            authoritative_designator: None,
            evidence_ids: Vec::new(),
            rationale: "the exact grounded case has no safely selectable value".to_string(),
        };
        let adjudication = AircraftHierarchyAdjudication {
            confidence: CurationConfidence::VeryHigh,
            make: make.clone(),
            faa_make_relationship: FaaMakeRelationshipDecision {
                action: relationship_action,
                faa_manufacturer_name: reference.manufacturer_name.clone().unwrap(),
                canonical_make_name: canonical_make.to_string(),
                existing_alias_id,
                valid_from_model_year: first_year,
                valid_to_model_year: last_year,
                evidence_ids: relationship_evidence_ids,
                applicability_evidence_ids,
                rationale: "primary evidence confirms the FAA legal make relationship".to_string(),
            },
            family: family.clone(),
            family_label_relationship: FamilyLabelRelationshipDecision {
                action: FamilyLabelRelationshipAction::ExactCanonicalLabel,
                observed_family_label: fixture.observation.model.clone(),
                canonical_family_name: fixture.observation.model.clone(),
                existing_alias_id: None,
                valid_from_model_year: None,
                valid_to_model_year: None,
                evidence_ids: Vec::new(),
                applicability_evidence_ids: Vec::new(),
                rationale: "retained and canonical family labels are exact".to_string(),
            },
            designation: designation.clone(),
            generation: no_supported_selection.clone(),
            package: no_supported_selection,
            material_distinctions: Vec::new(),
            unresolved_questions: Vec::new(),
            rationale: "all hierarchy dimensions are independently resolved".to_string(),
        };
        let proposal = AircraftHierarchyProposal {
            manufacturer: CatalogEntityProposal {
                existing_catalog_id: None,
                display_name: canonical_make.to_string(),
                authoritative_designator: None,
            },
            model_family: CatalogEntityProposal {
                existing_catalog_id: None,
                display_name: "182".to_string(),
                authoritative_designator: None,
            },
            certified_variant: CatalogEntityProposal {
                existing_catalog_id: None,
                display_name: "182T".to_string(),
                authoritative_designator: Some("182T".to_string()),
            },
            generation: None,
            tier: None,
            evidence,
        };
        let verification = AircraftHierarchyVerification {
            verdict: VerificationVerdict::Confirm,
            confidence: CurationConfidence::VeryHigh,
            verified_evidence_ids: proposal
                .evidence
                .iter()
                .map(|claim| claim.evidence_id.clone())
                .collect(),
            differentiation_checks: Vec::new(),
            errors: Vec::new(),
            rationale: "independent verification confirms every selected identity".to_string(),
        };
        ReviewableAircraftHierarchy {
            direct_source_proofs: direct_source_proofs(&proposal.evidence, &adjudication, &server),
            proposal,
            adjudication,
            verification,
            server_faa_evidence: server,
        }
    }

    async fn table_counts(db: &AppDb) -> Vec<i64> {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let mut counts = Vec::new();
        for table in [
            "aircraft_identity_observations",
            "curation_evidence_sources",
            "curation_evidence_claims",
            "aircraft_identity_resolution_cases",
            "aircraft_identity_decisions",
            "aircraft_identity_decision_claims",
            "aircraft_makes",
            "aircraft_make_aliases",
            "aircraft_model_families",
            "aircraft_family_aliases",
            "aircraft_designations",
            "aircraft_designation_faa_bindings",
            "aircraft_sale_listing_identity_assignments",
            "aircraft_sale_listing_current_identity_assignments",
            "aircraft_sale_listing_exact_compatibility_projections",
        ] {
            counts.push(
                sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(pool)
                    .await
                    .unwrap(),
            );
        }
        counts
    }

    async fn persist(
        fixture: &Fixture,
        revision: &str,
        reviewable: &ReviewableAircraftHierarchy,
    ) -> Result<PersistedAircraftHierarchy, AircraftHierarchyPersistenceError> {
        persist_reviewable_aircraft_hierarchy(
            &fixture.db,
            PersistReviewableAircraftHierarchy {
                listing_id: fixture.listing_id,
                observation: &fixture.observation,
                expected_catalog_revision: revision,
                reviewable,
                grounding: &fixture.grounding,
            },
        )
        .await
    }

    async fn insert_test_catalog_decision(
        fixture: &Fixture,
        entity_kind: &str,
        resolution_scope: &str,
        suffix: &str,
        with_primary_claim: bool,
    ) -> i64 {
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        let observation_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM aircraft_identity_observations WHERE aircraft_sale_listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let case_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_identity_resolution_cases (
              observation_id, resolution_scope, job_fingerprint,
              catalog_revision, case_status
            ) VALUES (?, ?, ?, ?, 'resolved')
            RETURNING id
            "#,
        )
        .bind(observation_id)
        .bind(resolution_scope)
        .bind(format!("test-no-supported-{suffix}"))
        .bind(revision)
        .fetch_one(pool)
        .await
        .unwrap();
        let decision_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_identity_decisions (
              resolution_case_id, entity_kind, decision_action, decision_status,
              selected_entity_id, decision_payload_json,
              deterministic_validation_json, deterministic_validation_passed,
              rationale, decided_at
            ) VALUES (
              ?, ?, 'approve_new', 'approved', NULL, '{}', '{"passed":true}',
              1, 'test catalog relation', CURRENT_TIMESTAMP
            )
            RETURNING id
            "#,
        )
        .bind(case_id)
        .bind(entity_kind)
        .fetch_one(pool)
        .await
        .unwrap();
        if with_primary_claim {
            let evidence_claim_id = sqlx::query_scalar::<_, i64>(
                r#"
                SELECT claim.id
                FROM curation_evidence_claims claim
                JOIN curation_evidence_sources source
                  ON source.id = claim.evidence_source_id
                WHERE claim.validation_status = 'validated'
                  AND source.source_tier IN (
                    'manufacturer_primary', 'regulator_primary'
                  )
                ORDER BY claim.id
                LIMIT 1
                "#,
            )
            .fetch_one(pool)
            .await
            .unwrap();
            sqlx::query(
                r#"
                INSERT INTO aircraft_identity_decision_claims (
                  decision_id, evidence_claim_id, evidence_role
                ) VALUES (?, ?, 'identity')
                "#,
            )
            .bind(decision_id)
            .bind(evidence_claim_id)
            .execute(pool)
            .await
            .unwrap();
        }
        decision_id
    }

    async fn insert_test_generation_relation(fixture: &Fixture, hierarchy: &AircraftHierarchy) {
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        let generation_decision_id =
            insert_test_catalog_decision(fixture, "generation", "generation", "generation", true)
                .await;
        let generation_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_generations (
              aircraft_model_family_id, name, normalized_name,
              ordinal, approval_decision_id
            ) VALUES (?, 'Test generation', 'test generation', NULL, ?)
            RETURNING id
            "#,
        )
        .bind(hierarchy.model_family_id)
        .bind(generation_decision_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let relation_decision_id = insert_test_catalog_decision(
            fixture,
            "generation_designation",
            "generation",
            "generation-designation",
            false,
        )
        .await;
        sqlx::query(
            r#"
            INSERT INTO aircraft_generation_designations (
              aircraft_generation_id, aircraft_designation_id,
              approval_decision_id
            ) VALUES (?, ?, ?)
            "#,
        )
        .bind(generation_id)
        .bind(hierarchy.certified_variant_id)
        .bind(relation_decision_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_test_package_applicability(
        fixture: &Fixture,
        hierarchy: &AircraftHierarchy,
        package_kind: &str,
        valid_from_model_year: Option<i64>,
        valid_to_model_year: Option<i64>,
    ) {
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        let package_decision_id =
            insert_test_catalog_decision(fixture, "package", "package", "package", true).await;
        let package_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_factory_packages (
              aircraft_model_family_id, name, normalized_name,
              package_kind, exclusivity_group, approval_decision_id
            ) VALUES (
              ?, 'Test package', 'test package', ?, NULL, ?
            )
            RETURNING id
            "#,
        )
        .bind(hierarchy.model_family_id)
        .bind(package_kind)
        .bind(package_decision_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let applicability_decision_id = insert_test_catalog_decision(
            fixture,
            "package_applicability",
            "package",
            "package-applicability",
            false,
        )
        .await;
        sqlx::query(
            r#"
            INSERT INTO aircraft_package_applicability (
              aircraft_factory_package_id, aircraft_designation_id,
              aircraft_generation_id, valid_from_model_year,
              valid_to_model_year, approval_decision_id
            ) VALUES (?, ?, NULL, ?, ?, ?)
            "#,
        )
        .bind(package_id)
        .bind(hierarchy.certified_variant_id)
        .bind(valid_from_model_year)
        .bind(valid_to_model_year)
        .bind(applicability_decision_id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn source_revalidation_accepts_versioned_structured_identity_evidence() {
        let fixture = fixture("TEXTRON AVIATION INC", "Cessna").await;
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        let html = concat!(
            "<html><head><title>2022 CESSNA 182T SKYLANE For Sale</title></head>",
            "<body><p>N89225, serial 18283169</p></body></html>"
        );
        let html_sha = format!("{:x}", Sha256::digest(html.as_bytes()));
        let extraction = json!({
            "manufacturer": "Cessna",
            "model": "182 Skylane",
            "variant": "182T",
            "model_year": 2022,
            "registration_number": "N89225",
            "serial_number": "18283169",
        });
        sqlx::query(
            r#"
            UPDATE plugin_submissions
               SET rendered_html = ?,
                   rendered_html_sha256 = ?,
                   extracted_listing_json = ?
             WHERE canonical_listing_id = ?
            "#,
        )
        .bind(html)
        .bind(&html_sha)
        .bind(extraction.to_string())
        .bind(fixture.listing_id)
        .execute(pool)
        .await
        .unwrap();

        let observation =
            load_aircraft_identity_observations(&fixture.db, 1, Some(fixture.listing_id))
                .await
                .unwrap()
                .observations
                .pop()
                .unwrap();
        assert_eq!(observation.model, "182 Skylane");
        assert_eq!(
            observation.source_excerpt.as_deref(),
            Some("CESSNA 182T SKYLANE")
        );
        assert!(observation.source_excerpt_is_exact);

        let mut row = sqlx::query_as::<_, CurrentListingSourceRow>(CURRENT_LISTING_SOURCE_SQLITE)
            .bind(fixture.listing_id)
            .fetch_one(pool)
            .await
            .unwrap();
        validate_current_listing_source(&row, &observation)
            .expect("the exact versioned publisher evidence must revalidate");

        row.extracted_listing_json = Some(
            json!({
                "manufacturer": "Cessna",
                "model": "182 Skylane",
                "variant": "182T",
                "model_year": 2022,
                "registration_number": "N89225",
                "serial_number": null,
            })
            .to_string(),
        );
        validate_current_listing_source(&row, &observation)
            .expect("an omitted retained serial is safe when current FAA admission is exact");
        row.extracted_listing_json = Some(
            json!({
                "manufacturer": "Cessna",
                "model": "182 Skylane",
                "variant": "182T",
                "model_year": 2022,
                "registration_number": null,
                "serial_number": "18283169",
            })
            .to_string(),
        );
        validate_current_listing_source(&row, &observation)
            .expect("an omitted retained registration is safe when current FAA admission is exact");
        for (registration_number, serial_number) in [
            (Some("N89226"), Some("18283169")),
            (Some("N89225"), Some("18283170")),
        ] {
            row.extracted_listing_json = Some(
                json!({
                    "manufacturer": "Cessna",
                    "model": "182 Skylane",
                    "variant": "182T",
                    "model_year": 2022,
                    "registration_number": registration_number,
                    "serial_number": serial_number,
                })
                .to_string(),
            );
            assert!(
                validate_current_listing_source(&row, &observation).is_err(),
                "a nonempty conflicting retained identifier must fail closed"
            );
        }

        row.extracted_listing_json = Some(
            json!({
                "manufacturer": "Cessna",
                "model": "182",
                "variant": "182T",
                "model_year": 2022,
                "registration_number": "N89225",
                "serial_number": "18283169",
            })
            .to_string(),
        );
        assert!(validate_current_listing_source(&row, &observation).is_err());
        row.extracted_listing_json = Some(extraction.to_string());
        row.rendered_html_sha256 = Some("0".repeat(64));
        assert!(validate_current_listing_source(&row, &observation).is_err());

        let base_model_html = concat!(
            "<html><head><title>2022 CESSNA TURBO 182T SKYLANE For Sale</title></head>",
            "<body><p>N89225, serial 18283169</p></body></html>"
        );
        let base_model_html_sha = format!("{:x}", Sha256::digest(base_model_html.as_bytes()));
        let base_model_extraction = json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "Turbo 182T Skylane",
            "model_year": 2022,
            "registration_number": "N89225",
            "serial_number": "18283169",
        });
        sqlx::query(
            r#"
            UPDATE plugin_submissions
               SET rendered_html = ?,
                   rendered_html_sha256 = ?,
                   extracted_listing_json = ?
             WHERE canonical_listing_id = ?
            "#,
        )
        .bind(base_model_html)
        .bind(&base_model_html_sha)
        .bind(base_model_extraction.to_string())
        .bind(fixture.listing_id)
        .execute(pool)
        .await
        .unwrap();
        let base_model_observation =
            load_aircraft_identity_observations(&fixture.db, 1, Some(fixture.listing_id))
                .await
                .unwrap()
                .observations
                .pop()
                .unwrap();
        assert_eq!(base_model_observation.model, "182");
        assert_eq!(
            base_model_observation.source_excerpt.as_deref(),
            Some("CESSNA TURBO 182T SKYLANE")
        );
        assert!(base_model_observation.source_excerpt_is_exact);
        let base_model_row =
            sqlx::query_as::<_, CurrentListingSourceRow>(CURRENT_LISTING_SOURCE_SQLITE)
                .bind(fixture.listing_id)
                .fetch_one(pool)
                .await
                .unwrap();
        validate_current_listing_source(&base_model_row, &base_model_observation)
            .expect("the exact full variant phrase must revalidate");
    }

    #[tokio::test]
    async fn direct_source_digest_changes_the_approval_fingerprint() {
        let fixture = fixture("TEXTRON AVIATION INC", "Cessna").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let reviewable = reviewable(
            &fixture,
            "Cessna",
            FaaMakeRelationshipAction::ProposeAlias,
            false,
        );
        let original = prepare_request(&PersistReviewableAircraftHierarchy {
            listing_id: fixture.listing_id,
            observation: &fixture.observation,
            expected_catalog_revision: &revision,
            reviewable: &reviewable,
            grounding: &fixture.grounding,
        })
        .unwrap();

        let mut changed = reviewable.clone();
        changed
            .direct_source_proofs
            .by_evidence_id
            .values_mut()
            .next()
            .expect("proposed alias has direct web evidence")
            .content_sha256 = "0".repeat(64);
        let changed = prepare_request(&PersistReviewableAircraftHierarchy {
            listing_id: fixture.listing_id,
            observation: &fixture.observation,
            expected_catalog_revision: &revision,
            reviewable: &changed,
            grounding: &fixture.grounding,
        })
        .unwrap();

        assert_ne!(original.approval_fingerprint, changed.approval_fingerprint);
        assert_ne!(original.payload_json, changed.payload_json);
    }

    #[tokio::test]
    async fn prepared_approval_identifies_the_v8_canonical_key_contract() {
        let fixture = fixture("TEXTRON AVIATION INC", "TEXTRON AVIATION INC").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let reviewable = reviewable(
            &fixture,
            "TEXTRON AVIATION INC",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            false,
        );
        let prepared = prepare_request(&PersistReviewableAircraftHierarchy {
            listing_id: fixture.listing_id,
            observation: &fixture.observation,
            expected_catalog_revision: &revision,
            reviewable: &reviewable,
            grounding: &fixture.grounding,
        })
        .unwrap();

        assert_eq!(PERSISTENCE_VERSION, "aircraft-hierarchy-persistence-v8");
        let payload: serde_json::Value = serde_json::from_str(&prepared.payload_json).unwrap();
        assert_eq!(
            payload["version"],
            serde_json::Value::String(PERSISTENCE_VERSION.to_string())
        );
        let validation: serde_json::Value =
            serde_json::from_str(&prepared.validation_json).unwrap();
        assert_eq!(
            validation["persistence_version"],
            serde_json::Value::String(PERSISTENCE_VERSION.to_string())
        );
        assert!(decision_fingerprint(&prepared, "make")
            .starts_with("aircraft-hierarchy-persistence-v8:sha256:"));
    }

    #[tokio::test]
    async fn approval_fingerprint_ignores_unused_claims_and_model_source_titles() {
        let fixture = fixture("TEXTRON AVIATION INC", "Cessna").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let reviewable = reviewable(
            &fixture,
            "Cessna",
            FaaMakeRelationshipAction::ProposeAlias,
            false,
        );
        let prepare = |reviewable: &ReviewableAircraftHierarchy| {
            prepare_request(&PersistReviewableAircraftHierarchy {
                listing_id: fixture.listing_id,
                observation: &fixture.observation,
                expected_catalog_revision: &revision,
                reviewable,
                grounding: &fixture.grounding,
            })
            .unwrap()
        };
        let original = prepare(&reviewable);

        let mut title_changed = reviewable.clone();
        title_changed
            .proposal
            .evidence
            .iter_mut()
            .find(|claim| claim.evidence_id == "brand-relationship")
            .unwrap()
            .source_title = "A harmless alternate publisher page title".to_string();
        let title_changed = prepare(&title_changed);
        assert_eq!(
            original.approval_fingerprint,
            title_changed.approval_fingerprint
        );
        assert_eq!(original.payload_json, title_changed.payload_json);

        let mut unused_changed = reviewable;
        unused_changed
            .proposal
            .evidence
            .push(EvidenceClaimProposal {
                evidence_id: "unused-extra-context".to_string(),
                source_url: "https://secondary.example.test/unused".to_string(),
                source_title: "Unused secondary context".to_string(),
                evidence_excerpt:
                    "This valid dossier claim is not selected by any aircraft identity decision."
                        .to_string(),
                source_kind: EvidenceSourceKind::RecognizedSecondary,
                supports: [EvidenceClaimKind::HierarchyIdentity].into_iter().collect(),
            });
        unused_changed
            .verification
            .verified_evidence_ids
            .push("unused-extra-context".to_string());
        let unused_changed = prepare(&unused_changed);
        assert_eq!(
            original.approval_fingerprint,
            unused_changed.approval_fingerprint
        );
        assert_eq!(original.payload_json, unused_changed.payload_json);
    }

    #[tokio::test]
    async fn missing_or_mismatched_used_source_proof_produces_zero_writes() {
        let fixture = fixture("TEXTRON AVIATION INC", "Cessna").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let baseline = reviewable(
            &fixture,
            "Cessna",
            FaaMakeRelationshipAction::ProposeAlias,
            false,
        );
        let before = table_counts(&fixture.db).await;

        let mut missing = baseline.clone();
        missing.direct_source_proofs = ServerFetchedAircraftSourceProofs::default();
        let error = persist(&fixture, &revision, &missing).await.unwrap_err();
        assert!(error.to_string().contains("direct-source proof"));
        assert_eq!(table_counts(&fixture.db).await, before);

        let mut mismatched = baseline;
        mismatched
            .direct_source_proofs
            .by_evidence_id
            .values_mut()
            .next()
            .expect("proposed alias has direct web evidence")
            .normalized_span_sha256 = "0".repeat(64);
        let error = persist(&fixture, &revision, &mismatched).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("final URL or normalized source span proof"),
            "unexpected error: {error}"
        );
        assert_eq!(table_counts(&fixture.db).await, before);
    }

    #[tokio::test]
    async fn exact_tcds_family_claims_persist_once_without_creating_an_alias() {
        let fixture = fixture("TEXTRON AVIATION INC", "TEXTRON AVIATION INC").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let reviewable = with_tcds_family_binding(
            &fixture,
            reviewable(
                &fixture,
                "TEXTRON AVIATION INC",
                FaaMakeRelationshipAction::ExactCanonicalLabel,
                false,
            ),
            "Skylane",
        );
        let source_url = concat!(
            "https://drs.faa.gov/api/drs/data-pull/download/",
            "01234567-89ab-cdef-0123-456789abcdef"
        );

        let first = persist(&fixture, &revision, &reviewable).await.unwrap();
        assert_eq!(
            first.hierarchy.model_family_id,
            first.assignment.aircraft_model_family_id
        );
        assert_eq!(first.assignment.family_name, "Skylane");
        assert_eq!(first.catalog_writes, 3);

        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        let canonical_make = sqlx::query_as::<_, (String, String)>(
            "SELECT name, normalized_name FROM aircraft_makes WHERE id = ?",
        )
        .bind(first.hierarchy.manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            canonical_make,
            (
                "TEXTRON AVIATION INC".to_string(),
                "textron aviation inc".to_string(),
            )
        );
        assert_ne!(
            canonical_make.1,
            normalize_aircraft_retrieval_text("Cessna")
        );
        assert_eq!(
            sqlx::query_as::<_, (String, String, String, String, String)>(
                r#"
                SELECT source_url, resolved_url, source_domain, source_tier,
                       content_sha256
                FROM curation_evidence_sources
                WHERE source_url = ?
                "#,
            )
            .bind(source_url)
            .fetch_one(pool)
            .await
            .unwrap(),
            (
                source_url.to_string(),
                source_url.to_string(),
                "drs.faa.gov".to_string(),
                "regulator_primary".to_string(),
                "6".repeat(64),
            )
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM aircraft_family_aliases")
                .fetch_one(pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT COUNT(*)
                FROM aircraft_identity_resolution_cases resolution_case
                JOIN aircraft_identity_decisions decision
                  ON decision.resolution_case_id = resolution_case.id
                JOIN aircraft_identity_decision_claims decision_claim
                  ON decision_claim.decision_id = decision.id
                JOIN curation_evidence_claims claim
                  ON claim.id = decision_claim.evidence_claim_id
                JOIN curation_evidence_sources source
                  ON source.id = claim.evidence_source_id
                WHERE resolution_case.job_fingerprint
                        LIKE '%:family_label_type_certificate'
                  AND source.source_url = ?
                "#,
            )
            .bind(source_url)
            .fetch_one(pool)
            .await
            .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT COUNT(*)
                FROM curation_evidence_claims claim
                JOIN curation_evidence_sources source
                  ON source.id = claim.evidence_source_id
                WHERE source.source_url = ?
                  AND claim.quoted_evidence LIKE 'Model 182,%'
                "#,
            )
            .bind(source_url)
            .fetch_one(pool)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                r#"
                SELECT
                  SUM(CASE WHEN decision_claim.evidence_role = 'identity'
                           THEN 1 ELSE 0 END),
                  SUM(CASE WHEN decision_claim.evidence_role = 'applicability'
                           THEN 1 ELSE 0 END)
                FROM aircraft_identity_decision_claims decision_claim
                JOIN curation_evidence_claims claim
                  ON claim.id = decision_claim.evidence_claim_id
                JOIN curation_evidence_sources source
                  ON source.id = claim.evidence_source_id
                WHERE decision_claim.decision_id = ?
                  AND source.source_url = ?
                "#,
            )
            .bind(first.assignment.identity_decision_id)
            .bind(source_url)
            .fetch_one(pool)
            .await
            .unwrap(),
            (1, 1)
        );

        let counts = table_counts(&fixture.db).await;
        let replay = persist(&fixture, &revision, &reviewable).await.unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(replay.catalog_writes, 0);
        assert_eq!(table_counts(&fixture.db).await, counts);
    }

    #[tokio::test]
    async fn forged_reserved_tcds_claim_is_rejected_before_writes() {
        let fixture = fixture("TEXTRON AVIATION INC", "TEXTRON AVIATION INC").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let mut reviewable = with_tcds_family_binding(
            &fixture,
            reviewable(
                &fixture,
                "TEXTRON AVIATION INC",
                FaaMakeRelationshipAction::ExactCanonicalLabel,
                false,
            ),
            "Skylane",
        );
        reviewable
            .proposal
            .evidence
            .iter_mut()
            .find(|claim| is_server_faa_drs_evidence_id(&claim.evidence_id))
            .expect("reviewable has a TCDS claim")
            .source_url =
            "https://drs.faa.gov/api/drs/data-pull/download/ffffffff-ffff-ffff-ffff-ffffffffffff"
                .to_string();
        let before = table_counts(&fixture.db).await;

        let error = persist(&fixture, &revision, &reviewable).await.unwrap_err();
        assert!(matches!(error, AircraftHierarchyPersistenceError::Faa(_)));
        assert!(
            error.to_string().contains("not an exact claim"),
            "unexpected error: {error}"
        );
        assert_eq!(table_counts(&fixture.db).await, before);
    }

    #[tokio::test]
    async fn fresh_exact_faa_hierarchy_commits_atomically_and_replays_idempotently() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let reviewable = reviewable(
            &fixture,
            "CESSNA AIRCRAFT CO",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            true,
        );

        let first = persist(&fixture, &revision, &reviewable).await.unwrap();
        assert_eq!(first.catalog_writes, 3);
        assert!(!first.idempotent_replay);
        assert_eq!(first.assignment.faa_n_number, "N89225");

        let counts = table_counts(&fixture.db).await;
        let replay = persist(&fixture, &revision, &reviewable).await.unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(replay.catalog_writes, 0);
        assert_eq!(
            replay.assignment.assignment_id,
            first.assignment.assignment_id
        );
        assert_eq!(table_counts(&fixture.db).await, counts);

        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM aircraft_make_aliases")
                .fetch_one(pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM curation_evidence_claims WHERE object_text LIKE '%unused-dossier-claim%'",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM aircraft_identity_decisions WHERE decision_action = 'no_supported_selection' AND decision_status = 'approved'",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM aircraft_designation_faa_bindings",)
                .fetch_one(pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM aircraft_sale_listing_exact_compatibility_projections WHERE listing_id = ?",
            )
            .bind(fixture.listing_id)
            .fetch_one(pool)
            .await
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn manufacturer_series_family_match_persists_evidence_without_creating_an_alias() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let reviewable = with_manufacturer_series_family_relationship(
            reviewable(
                &fixture,
                "CESSNA AIRCRAFT CO",
                FaaMakeRelationshipAction::ExactCanonicalLabel,
                false,
            ),
            "182",
            "Skylane",
        );

        let first = persist(&fixture, &revision, &reviewable).await.unwrap();
        assert_eq!(first.catalog_writes, 3);
        assert!(!first.idempotent_replay);
        assert_eq!(first.assignment.family_name, "Skylane");

        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM aircraft_family_aliases")
                .fetch_one(pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT COUNT(*)
                FROM aircraft_identity_decisions decision
                JOIN aircraft_identity_decision_claims link
                  ON link.decision_id = decision.id
                JOIN curation_evidence_claims claim
                  ON claim.id = link.evidence_claim_id
                JOIN curation_evidence_sources source
                  ON source.id = claim.evidence_source_id
                WHERE decision.entity_kind = 'family'
                  AND decision.decision_action = 'match_existing'
                  AND decision.decision_status = 'approved'
                  AND decision.selected_entity_id = ?
                  AND link.evidence_role = 'identity'
                  AND source.source_url =
                      'https://cessna.example.test/family-label-manufacturer-series'
                "#,
            )
            .bind(first.hierarchy.model_family_id)
            .fetch_one(pool)
            .await
            .unwrap(),
            1
        );

        let counts = table_counts(&fixture.db).await;
        let replay = persist(&fixture, &revision, &reviewable).await.unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(replay.catalog_writes, 0);
        assert_eq!(
            replay.assignment.assignment_id,
            first.assignment.assignment_id
        );
        assert_eq!(table_counts(&fixture.db).await, counts);
    }

    #[tokio::test]
    async fn malformed_manufacturer_series_family_match_is_rejected_without_writes() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let baseline = with_manufacturer_series_family_relationship(
            reviewable(
                &fixture,
                "CESSNA AIRCRAFT CO",
                FaaMakeRelationshipAction::ExactCanonicalLabel,
                false,
            ),
            "182",
            "Skylane",
        );
        let before = table_counts(&fixture.db).await;

        let mut missing_evidence = baseline.clone();
        missing_evidence
            .adjudication
            .family_label_relationship
            .evidence_ids
            .clear();
        let error = persist(&fixture, &revision, &missing_evidence)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("relationship evidence"),
            "unexpected persistence error: {error}"
        );
        assert_eq!(table_counts(&fixture.db).await, before);

        let mut alias_shaped = baseline;
        alias_shaped
            .adjudication
            .family_label_relationship
            .valid_from_model_year = Some(fixture.observation.model_year);
        let error = persist(&fixture, &revision, &alias_shaped)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("no alias id, model-year bounds"),
            "unexpected persistence error: {error}"
        );
        assert_eq!(table_counts(&fixture.db).await, before);
    }

    #[tokio::test]
    async fn proposed_family_label_alias_commits_with_family_and_replays_idempotently() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let reviewable = with_proposed_family_label_alias(
            reviewable(
                &fixture,
                "CESSNA AIRCRAFT CO",
                FaaMakeRelationshipAction::ExactCanonicalLabel,
                false,
            ),
            "182",
            "Skylane",
            fixture.observation.model_year,
        );

        let first = persist(&fixture, &revision, &reviewable).await.unwrap();
        assert_eq!(first.catalog_writes, 4);
        assert!(!first.idempotent_replay);

        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        let alias = sqlx::query_as::<_, (i64, String, String, Option<i64>, Option<i64>, String)>(
            r#"
            SELECT alias.aircraft_model_family_id, alias.alias,
                   alias.normalized_alias, alias.valid_from_model_year,
                   alias.valid_to_model_year, market.code
            FROM aircraft_family_aliases alias
            JOIN aircraft_markets market ON market.id = alias.aircraft_market_id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(alias.0, first.hierarchy.model_family_id);
        assert_eq!(alias.1, "182");
        assert_eq!(alias.2, normalize_aircraft_retrieval_text("182"));
        assert_eq!(
            (alias.3, alias.4, alias.5.as_str()),
            (Some(2022), Some(2022), "US")
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT name FROM aircraft_model_families WHERE id = ?",
            )
            .bind(first.hierarchy.model_family_id)
            .fetch_one(pool)
            .await
            .unwrap(),
            "Skylane"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT COUNT(*)
                FROM aircraft_identity_decisions decision
                JOIN aircraft_identity_decision_claims link
                  ON link.decision_id = decision.id
                WHERE decision.entity_kind = 'alias'
                  AND decision.decision_action = 'approve_new'
                  AND decision.rationale LIKE '%retained label%'
                "#,
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            2
        );

        let counts = table_counts(&fixture.db).await;
        let replay = persist(&fixture, &revision, &reviewable).await.unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(replay.catalog_writes, 0);
        assert_eq!(
            replay.assignment.assignment_id,
            first.assignment.assignment_id
        );
        assert_eq!(table_counts(&fixture.db).await, counts);
    }

    #[tokio::test]
    async fn proposed_family_label_alias_rejects_open_or_unsupported_years_without_writes() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let baseline = with_proposed_family_label_alias(
            reviewable(
                &fixture,
                "CESSNA AIRCRAFT CO",
                FaaMakeRelationshipAction::ExactCanonicalLabel,
                false,
            ),
            "182",
            "Skylane",
            fixture.observation.model_year,
        );
        let before = table_counts(&fixture.db).await;

        for (first_year, last_year, expected_error) in [
            (None, None, "finite first and last"),
            (Some(2021), Some(2022), "bound 2021"),
        ] {
            let mut reviewable = baseline.clone();
            reviewable
                .adjudication
                .family_label_relationship
                .valid_from_model_year = first_year;
            reviewable
                .adjudication
                .family_label_relationship
                .valid_to_model_year = last_year;

            let error = persist(&fixture, &revision, &reviewable).await.unwrap_err();
            assert!(
                error.to_string().contains(expected_error),
                "unexpected persistence error: {error}"
            );
            assert_eq!(table_counts(&fixture.db).await, before);
        }
    }

    #[tokio::test]
    async fn approved_local_family_alias_matches_without_a_new_alias_or_family() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let proposed = with_proposed_family_label_alias(
            reviewable(
                &fixture,
                "CESSNA AIRCRAFT CO",
                FaaMakeRelationshipAction::ExactCanonicalLabel,
                false,
            ),
            "182",
            "Skylane",
            fixture.observation.model_year,
        );
        let first = persist(&fixture, &revision, &proposed).await.unwrap();
        let duplicate = duplicate_listing_fixture(&fixture).await;

        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        let alias_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM aircraft_family_aliases WHERE alias='182'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let current_revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let mut matched = reviewable(
            &duplicate,
            "CESSNA AIRCRAFT CO",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            false,
        );
        matched.adjudication.make.action = EntityResolutionAction::MatchExisting;
        matched.adjudication.make.existing_catalog_id = Some(first.hierarchy.manufacturer_id);
        matched.proposal.manufacturer.existing_catalog_id = Some(first.hierarchy.manufacturer_id);
        matched.adjudication.family.action = EntityResolutionAction::MatchExisting;
        matched.adjudication.family.existing_catalog_id = Some(first.hierarchy.model_family_id);
        matched.adjudication.family.display_name = Some("Skylane".to_string());
        matched.proposal.model_family.existing_catalog_id = Some(first.hierarchy.model_family_id);
        matched.proposal.model_family.display_name = "Skylane".to_string();
        matched.adjudication.designation.action = EntityResolutionAction::MatchExisting;
        matched.adjudication.designation.existing_catalog_id =
            Some(first.hierarchy.certified_variant_id);
        matched.proposal.certified_variant.existing_catalog_id =
            Some(first.hierarchy.certified_variant_id);
        matched.adjudication.family_label_relationship = FamilyLabelRelationshipDecision {
            action: FamilyLabelRelationshipAction::MatchApprovedAlias,
            observed_family_label: "182".to_string(),
            canonical_family_name: "Skylane".to_string(),
            existing_alias_id: Some(alias_id),
            valid_from_model_year: Some(2022),
            valid_to_model_year: Some(2022),
            evidence_ids: Vec::new(),
            applicability_evidence_ids: Vec::new(),
            rationale: "the approved local family alias exactly matches this listing".to_string(),
        };

        let matched_result = persist(&duplicate, &current_revision, &matched)
            .await
            .unwrap();
        assert!(!matched_result.idempotent_replay);
        assert_eq!(matched_result.catalog_writes, 0);
        assert_eq!(matched_result.hierarchy, first.hierarchy);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM aircraft_family_aliases")
                .fetch_one(pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT COUNT(*)
                FROM aircraft_identity_decisions
                WHERE entity_kind='alias'
                  AND decision_action='match_existing'
                  AND selected_entity_id=?
                "#,
            )
            .bind(alias_id)
            .fetch_one(pool)
            .await
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn replay_rechecks_no_supported_generation_and_package_relations() {
        let generation_fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let generation_revision = approved_aircraft_catalog_revision(&generation_fixture.db)
            .await
            .unwrap();
        let generation_reviewable = reviewable(
            &generation_fixture,
            "CESSNA AIRCRAFT CO",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            false,
        );
        let generation_persisted = persist(
            &generation_fixture,
            &generation_revision,
            &generation_reviewable,
        )
        .await
        .unwrap();
        insert_test_generation_relation(&generation_fixture, &generation_persisted.hierarchy).await;
        let generation_counts = table_counts(&generation_fixture.db).await;
        let generation_error = persist(
            &generation_fixture,
            &generation_revision,
            &generation_reviewable,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            generation_error,
            AircraftHierarchyPersistenceError::OptionalSelectionInvalidated {
                dimension: "generation",
                ..
            }
        ));
        assert_eq!(
            table_counts(&generation_fixture.db).await,
            generation_counts
        );

        let package_fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let package_revision = approved_aircraft_catalog_revision(&package_fixture.db)
            .await
            .unwrap();
        let package_reviewable = reviewable(
            &package_fixture,
            "CESSNA AIRCRAFT CO",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            false,
        );
        let package_persisted = persist(&package_fixture, &package_revision, &package_reviewable)
            .await
            .unwrap();
        insert_test_package_applicability(
            &package_fixture,
            &package_persisted.hierarchy,
            "trim_tier",
            Some(package_fixture.observation.model_year),
            Some(package_fixture.observation.model_year),
        )
        .await;
        let package_counts = table_counts(&package_fixture.db).await;
        let package_error = persist(&package_fixture, &package_revision, &package_reviewable)
            .await
            .unwrap_err();
        assert!(matches!(
            package_error,
            AircraftHierarchyPersistenceError::OptionalSelectionInvalidated {
                dimension: "package",
                ..
            }
        ));
        assert_eq!(table_counts(&package_fixture.db).await, package_counts);
    }

    #[tokio::test]
    async fn replay_ignores_non_tier_and_out_of_year_package_relations() {
        for (package_kind, first_year, last_year) in [
            ("option_bundle", Some(2022), Some(2022)),
            ("trim_tier", Some(2023), Some(2024)),
        ] {
            let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
            let revision = approved_aircraft_catalog_revision(&fixture.db)
                .await
                .unwrap();
            let reviewable = reviewable(
                &fixture,
                "CESSNA AIRCRAFT CO",
                FaaMakeRelationshipAction::ExactCanonicalLabel,
                false,
            );
            let first = persist(&fixture, &revision, &reviewable).await.unwrap();
            insert_test_package_applicability(
                &fixture,
                &first.hierarchy,
                package_kind,
                first_year,
                last_year,
            )
            .await;
            let replay = persist(&fixture, &revision, &reviewable).await.unwrap();
            assert!(replay.idempotent_replay);
            assert_eq!(
                replay.assignment.assignment_id,
                first.assignment.assignment_id
            );
        }
    }

    #[tokio::test]
    async fn stale_revision_and_observation_or_faa_mismatch_roll_back_every_write() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let reviewable = reviewable(
            &fixture,
            "CESSNA AIRCRAFT CO",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            false,
        );
        let before = table_counts(&fixture.db).await;
        let stale = persist(&fixture, "sha256:stale", &reviewable)
            .await
            .unwrap_err();
        assert!(matches!(
            stale,
            AircraftHierarchyPersistenceError::StaleCatalog { .. }
        ));
        assert_eq!(table_counts(&fixture.db).await, before);

        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let mut wrong_observation = fixture.observation.clone();
        wrong_observation.observation_sha256 = "f".repeat(64);
        let error = persist_reviewable_aircraft_hierarchy(
            &fixture.db,
            PersistReviewableAircraftHierarchy {
                listing_id: fixture.listing_id,
                observation: &wrong_observation,
                expected_catalog_revision: &revision,
                reviewable: &reviewable,
                grounding: &fixture.grounding,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AircraftHierarchyPersistenceError::Faa(_)));
        assert_eq!(table_counts(&fixture.db).await, before);

        let mut wrong_grounding = fixture.grounding.clone();
        wrong_grounding.source_record_sha256 = "9".repeat(64);
        let error = persist_reviewable_aircraft_hierarchy(
            &fixture.db,
            PersistReviewableAircraftHierarchy {
                listing_id: fixture.listing_id,
                observation: &fixture.observation,
                expected_catalog_revision: &revision,
                reviewable: &reviewable,
                grounding: &wrong_grounding,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AircraftHierarchyPersistenceError::Faa(_)));
        assert_eq!(table_counts(&fixture.db).await, before);
    }

    #[tokio::test]
    async fn proposed_faa_make_alias_is_exact_scoped_and_evidence_backed() {
        let fixture = fixture("TEXTRON AVIATION INC", "Cessna").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let mut reviewable = reviewable(
            &fixture,
            "Cessna",
            FaaMakeRelationshipAction::ProposeAlias,
            true,
        );
        let shared_source_url = "https://cessna.example.test/brand-proof";
        for claim in reviewable.proposal.evidence.iter_mut().filter(|claim| {
            matches!(
                claim.evidence_id.as_str(),
                "brand-relationship" | "brand-applicability"
            )
        }) {
            claim.source_url = shared_source_url.to_string();
        }
        reviewable.direct_source_proofs = direct_source_proofs(
            &reviewable.proposal.evidence,
            &reviewable.adjudication,
            &reviewable.server_faa_evidence,
        );
        let result = persist(&fixture, &revision, &reviewable).await.unwrap();
        assert_eq!(result.catalog_writes, 4);

        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        let alias = sqlx::query_as::<_, (String, String, Option<i64>, Option<i64>, String)>(
            r#"
            SELECT alias.alias, alias.normalized_alias,
                   alias.valid_from_model_year, alias.valid_to_model_year,
                   market.code
            FROM aircraft_make_aliases alias
            JOIN aircraft_markets market ON market.id = alias.aircraft_market_id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(alias.0, "TEXTRON AVIATION INC");
        assert_eq!(
            alias.1,
            normalize_aircraft_retrieval_text("TEXTRON AVIATION INC")
        );
        assert_eq!(
            (alias.2, alias.3, alias.4.as_str()),
            (Some(2022), Some(2022), "US")
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT COUNT(*)
                FROM aircraft_identity_decisions decision
                JOIN aircraft_identity_decision_claims link
                  ON link.decision_id = decision.id
                WHERE decision.entity_kind = 'alias'
                  AND decision.decision_action = 'approve_new'
                "#,
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM curation_evidence_claims WHERE object_text LIKE '%unused-dossier-claim%'",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            0
        );
        let expected_content_sha256 = format!("{:x}", Sha256::digest(shared_source_url.as_bytes()));
        assert_eq!(
            sqlx::query_as::<_, (i64, String)>(
                r#"
                SELECT COUNT(*), MIN(content_sha256)
                FROM curation_evidence_sources
                WHERE source_url = ?
                "#,
            )
            .bind(shared_source_url)
            .fetch_one(pool)
            .await
            .unwrap(),
            (1, expected_content_sha256)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT COUNT(DISTINCT claim.evidence_source_id)
                FROM curation_evidence_claims claim
                WHERE claim.object_text LIKE '%brand-relationship%'
                   OR claim.object_text LIKE '%brand-applicability%'
                "#,
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            1
        );
        let source_count_before_replay =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM curation_evidence_sources")
                .fetch_one(pool)
                .await
                .unwrap();
        let replay = persist(&fixture, &revision, &reviewable).await.unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM curation_evidence_sources")
                .fetch_one(pool)
                .await
                .unwrap(),
            source_count_before_replay
        );

        let mut retitled = reviewable;
        for claim in retitled.proposal.evidence.iter_mut().filter(|claim| {
            matches!(
                claim.evidence_id.as_str(),
                "brand-relationship" | "brand-applicability"
            )
        }) {
            claim.source_title = format!("Alternate page title for {}", claim.evidence_id);
        }
        let retitled_replay = persist(&fixture, &revision, &retitled).await.unwrap();
        assert!(retitled_replay.idempotent_replay);
        assert_eq!(
            retitled_replay.approval_fingerprint,
            replay.approval_fingerprint
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM curation_evidence_sources")
                .fetch_one(pool)
                .await
                .unwrap(),
            source_count_before_replay
        );
    }

    #[tokio::test]
    async fn proposed_faa_make_alias_rejects_open_or_unsupported_years_without_writes() {
        let fixture = fixture("TEXTRON AVIATION INC", "Cessna").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let baseline = reviewable(
            &fixture,
            "Cessna",
            FaaMakeRelationshipAction::ProposeAlias,
            false,
        );
        let before = table_counts(&fixture.db).await;

        for (first_year, last_year, expected_error) in [
            (None, None, "finite first and last"),
            (Some(2021), Some(2022), "bound 2021"),
        ] {
            let mut reviewable = baseline.clone();
            reviewable
                .adjudication
                .faa_make_relationship
                .valid_from_model_year = first_year;
            reviewable
                .adjudication
                .faa_make_relationship
                .valid_to_model_year = last_year;

            let error = persist(&fixture, &revision, &reviewable).await.unwrap_err();
            assert!(
                error.to_string().contains(expected_error),
                "unexpected persistence error: {error}"
            );
            assert_eq!(table_counts(&fixture.db).await, before);
        }
    }

    #[tokio::test]
    async fn colliding_proposal_rolls_back_claims_decisions_and_catalog_rows() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let initial_revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let first_reviewable = reviewable(
            &fixture,
            "CESSNA AIRCRAFT CO",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            false,
        );
        persist(&fixture, &initial_revision, &first_reviewable)
            .await
            .unwrap();

        let current_revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        assert_ne!(current_revision, initial_revision);
        let before = table_counts(&fixture.db).await;
        let colliding_reviewable = reviewable(
            &fixture,
            "CESSNA AIRCRAFT CO",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            true,
        );
        let error = persist(&fixture, &current_revision, &colliding_reviewable)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AircraftHierarchyPersistenceError::Collision(_)
        ));
        assert_eq!(table_counts(&fixture.db).await, before);
    }

    #[tokio::test]
    async fn family_label_alias_collision_with_same_make_family_rolls_back_atomically() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let initial_revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let initial_reviewable = reviewable(
            &fixture,
            "CESSNA AIRCRAFT CO",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            false,
        );
        let initial = persist(&fixture, &initial_revision, &initial_reviewable)
            .await
            .unwrap();

        let current_revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let mut colliding = with_proposed_family_label_alias(
            reviewable(
                &fixture,
                "CESSNA AIRCRAFT CO",
                FaaMakeRelationshipAction::ExactCanonicalLabel,
                false,
            ),
            "182",
            "Skylane",
            fixture.observation.model_year,
        );
        colliding.adjudication.make.action = EntityResolutionAction::MatchExisting;
        colliding.adjudication.make.existing_catalog_id = Some(initial.hierarchy.manufacturer_id);
        colliding.proposal.manufacturer.existing_catalog_id =
            Some(initial.hierarchy.manufacturer_id);

        let before = table_counts(&fixture.db).await;
        let error = persist(&fixture, &current_revision, &colliding)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AircraftHierarchyPersistenceError::Collision(_)
        ));
        assert_eq!(table_counts(&fixture.db).await, before);
    }

    #[tokio::test]
    async fn newer_faa_snapshot_rejects_stale_grounding_without_partial_writes() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let reviewable = reviewable(
            &fixture,
            "CESSNA AIRCRAFT CO",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            false,
        );
        store_release(
            &fixture.db,
            &release_on(
                "2026-07-21",
                "N89225",
                "18283169",
                "CESSNA AIRCRAFT CO",
                "182T",
                '2',
            ),
        )
        .await
        .unwrap();
        let before = table_counts(&fixture.db).await;

        let error = persist(&fixture, &revision, &reviewable).await.unwrap_err();
        assert!(matches!(error, AircraftHierarchyPersistenceError::Faa(_)));
        assert_eq!(table_counts(&fixture.db).await, before);
    }

    #[tokio::test]
    async fn one_cluster_approval_replays_catalog_for_a_second_exact_listing() {
        let fixture = fixture("CESSNA AIRCRAFT CO", "CESSNA AIRCRAFT CO").await;
        let duplicate = duplicate_listing_fixture(&fixture).await;
        let original_revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let cluster_reviewable = reviewable_for_fixtures(
            &[&fixture, &duplicate],
            "CESSNA AIRCRAFT CO",
            FaaMakeRelationshipAction::ExactCanonicalLabel,
            false,
        );
        let first = persist(&fixture, &original_revision, &cluster_reviewable)
            .await
            .unwrap();

        let second = persist(&duplicate, &original_revision, &cluster_reviewable)
            .await
            .unwrap();
        assert!(second.idempotent_replay);
        assert_eq!(second.catalog_writes, 0);
        assert_eq!(second.hierarchy, first.hierarchy);
        assert_ne!(
            second.assignment.assignment_id,
            first.assignment.assignment_id
        );
        assert_eq!(
            second.assignment.aircraft_sale_listing_id,
            duplicate.listing_id
        );

        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM aircraft_sale_listing_current_identity_assignments",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM aircraft_makes")
                .fetch_one(pool)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn transaction_revision_matches_public_revision_with_alias_and_identifier() {
        let fixture = fixture("TEXTRON AVIATION INC", "Cessna").await;
        let revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let reviewable = reviewable(
            &fixture,
            "Cessna",
            FaaMakeRelationshipAction::ProposeAlias,
            false,
        );
        let persisted = persist(&fixture, &revision, &reviewable).await.unwrap();
        insert_primary_identifier(&fixture, &persisted).await;

        let public_revision = approved_aircraft_catalog_revision(&fixture.db)
            .await
            .unwrap();
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        let mut transaction = pool.begin().await.unwrap();
        let transaction_revision = catalog_revision_sqlite(&mut transaction).await.unwrap();
        transaction.rollback().await.unwrap();
        assert_eq!(transaction_revision, public_revision);
    }
}
