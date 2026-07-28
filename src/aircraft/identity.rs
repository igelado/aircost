//! Listing-to-catalog aircraft identity assignments.
//!
//! FAA facts admit an aircraft, but they do not invent the product hierarchy.
//! This module only promotes an exact, already-curated make/family/designation
//! candidate. Ambiguity or a missing family remains pending curation.

use std::collections::BTreeSet;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sqlx::FromRow;

use super::catalog::{
    normalize_aircraft_designator_retrieval_key, normalize_aircraft_retrieval_text,
};
use super::faa::{
    normalize_serial_key, require_listing_faa_admission, AircraftAdmissionError, AircraftGrounding,
};
use crate::db::{AppDb, DatabaseBackend};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentityAssignmentError {
    ListingNotFound(i64),
    Missing(i64),
    Mismatch { listing_id: i64, reason: String },
    Database(String),
}

impl fmt::Display for IdentityAssignmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListingNotFound(id) => write!(formatter, "listing {id} was not found"),
            Self::Missing(id) => write!(
                formatter,
                "listing {id} has no current FAA-backed curated aircraft identity assignment"
            ),
            Self::Mismatch { listing_id, reason } => {
                write!(
                    formatter,
                    "listing {listing_id} aircraft identity mismatch: {reason}"
                )
            }
            Self::Database(message) => {
                write!(formatter, "aircraft identity database error: {message}")
            }
        }
    }
}

impl std::error::Error for IdentityAssignmentError {}

impl From<sqlx::Error> for IdentityAssignmentError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CanonicalAircraftIdentityAssignment {
    pub assignment_id: i64,
    pub aircraft_sale_listing_id: i64,
    pub supersedes_assignment_id: Option<i64>,
    pub aircraft_make_id: i64,
    pub make_name: String,
    pub aircraft_model_family_id: i64,
    pub family_name: String,
    pub aircraft_designation_id: i64,
    pub official_designation: String,
    pub aircraft_generation_id: Option<i64>,
    pub aircraft_factory_package_id: Option<i64>,
    pub identity_decision_id: i64,
    pub identity_evidence_claim_id: i64,
    pub faa_registry_snapshot_id: i64,
    pub faa_n_number: String,
    pub faa_aircraft_code: String,
    pub faa_source_record_sha256: String,
    pub created_at: String,
}

/// The canonical identity dimensions used to select the legacy valuation
/// compatibility projection. These IDs, never listing prose, are the
/// projection identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CanonicalAircraftCompatibilityIdentity {
    pub aircraft_make_id: i64,
    pub make_name: String,
    pub aircraft_model_family_id: i64,
    pub family_name: String,
    pub aircraft_designation_id: i64,
    pub official_designation: String,
    pub aircraft_generation_id: Option<i64>,
    pub aircraft_factory_package_id: Option<i64>,
}

impl From<&CanonicalAircraftIdentityAssignment> for CanonicalAircraftCompatibilityIdentity {
    fn from(assignment: &CanonicalAircraftIdentityAssignment) -> Self {
        Self {
            aircraft_make_id: assignment.aircraft_make_id,
            make_name: assignment.make_name.clone(),
            aircraft_model_family_id: assignment.aircraft_model_family_id,
            family_name: assignment.family_name.clone(),
            aircraft_designation_id: assignment.aircraft_designation_id,
            official_designation: assignment.official_designation.clone(),
            aircraft_generation_id: assignment.aircraft_generation_id,
            aircraft_factory_package_id: assignment.aircraft_factory_package_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResolveCompatibilityIdentityOutcome {
    Resolved {
        identity: CanonicalAircraftCompatibilityIdentity,
    },
    PendingCuration {
        reason: String,
        candidate_count: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EnsureIdentityAssignmentOutcome {
    Current {
        assignment: CanonicalAircraftIdentityAssignment,
    },
    Assigned {
        assignment: CanonicalAircraftIdentityAssignment,
    },
    PendingCuration {
        reason: String,
        candidate_count: usize,
    },
}

/// Resolve the canonical dimensions needed by listing persistence before any
/// legacy valuation hierarchy row is selected or created.
///
/// If an existing listing already has a current assignment, that immutable
/// assignment is revalidated against the supplied FAA record and model year.
/// Otherwise the resolver considers only FAA identity plus the approved
/// canonical catalog. Literal listing make/model/variant labels are not inputs.
pub async fn resolve_faa_backed_compatibility_identity(
    db: &AppDb,
    existing_listing_id: Option<i64>,
    model_year: i64,
    grounding: &AircraftGrounding,
) -> Result<ResolveCompatibilityIdentityOutcome, IdentityAssignmentError> {
    let reference =
        grounding
            .aircraft
            .as_ref()
            .ok_or_else(|| IdentityAssignmentError::Mismatch {
                listing_id: existing_listing_id.unwrap_or(0),
                reason: "current FAA record has no aircraft reference identity".to_string(),
            })?;
    nonblank(reference.manufacturer_name.as_deref()).ok_or_else(|| {
        IdentityAssignmentError::Mismatch {
            listing_id: existing_listing_id.unwrap_or(0),
            reason: "current FAA record has no manufacturer".to_string(),
        }
    })?;
    nonblank(reference.model_name.as_deref()).ok_or_else(|| IdentityAssignmentError::Mismatch {
        listing_id: existing_listing_id.unwrap_or(0),
        reason: "current FAA record has no model designation".to_string(),
    })?;

    if let Some(listing_id) = existing_listing_id {
        if let Some(current) = load_current_assignment(db, listing_id).await? {
            if validate_assignment_dimensions(&current, grounding).is_err()
                || validate_assignment_make(db, &current, model_year, grounding)
                    .await
                    .is_err()
                || validate_assignment_provenance(&current, grounding).is_err()
                || invalid_or_unresolved_material_dimensions(
                    db,
                    current.aircraft_designation_id,
                    model_year,
                    current.aircraft_generation_id,
                    current.aircraft_factory_package_id,
                )
                .await?
            {
                return Ok(ResolveCompatibilityIdentityOutcome::PendingCuration {
                    reason:
                        "current curated assignment does not match the supplied FAA identity and model year"
                            .to_string(),
                    candidate_count: 0,
                });
            }
            return Ok(ResolveCompatibilityIdentityOutcome::Resolved {
                identity: PromotionCandidate::from_current(&current).into_compatibility_identity(),
            });
        }
    }

    let candidates = exact_promotion_candidates(db, model_year, grounding).await?;
    if candidates.len() != 1 {
        let reason = if candidates.is_empty() {
            "no exact existing curated make/designation matches the FAA record"
        } else {
            "multiple curated make/designation candidates match the FAA record exactly"
        };
        return Ok(ResolveCompatibilityIdentityOutcome::PendingCuration {
            reason: reason.to_string(),
            candidate_count: candidates.len(),
        });
    }
    let candidate = candidates.into_iter().next().expect("one candidate");

    if invalid_or_unresolved_material_dimensions(
        db,
        candidate.aircraft_designation_id,
        model_year,
        candidate.aircraft_generation_id,
        candidate.aircraft_factory_package_id,
    )
    .await?
    {
        return Ok(ResolveCompatibilityIdentityOutcome::PendingCuration {
            reason: "curated generation or trim-tier dimensions require an exact curation decision"
                .to_string(),
            candidate_count: 1,
        });
    }
    let bound_designation_id = faa_code_binding(
        db,
        &grounding.snapshot.snapshot_date,
        &grounding.snapshot.archive_sha256,
        &grounding.aircraft_code,
    )
    .await?;
    match bound_designation_id {
        Some(bound_designation_id) if bound_designation_id != candidate.aircraft_designation_id => {
            return Ok(ResolveCompatibilityIdentityOutcome::PendingCuration {
                reason: "FAA aircraft code is already bound to a different curated designation"
                    .to_string(),
                candidate_count: 1,
            });
        }
        None => {
            return Ok(ResolveCompatibilityIdentityOutcome::PendingCuration {
                reason: "FAA aircraft code has no existing approved designation binding"
                    .to_string(),
                candidate_count: 1,
            });
        }
        Some(_) => {}
    }

    Ok(ResolveCompatibilityIdentityOutcome::Resolved {
        identity: candidate.into_compatibility_identity(),
    })
}

/// Read and revalidate the current assignment against the exact current FAA
/// record. Legacy valuation labels are not identity evidence.
pub async fn require_listing_identity_assignment(
    db: &AppDb,
    listing_id: i64,
    grounding: &AircraftGrounding,
) -> Result<CanonicalAircraftIdentityAssignment, IdentityAssignmentError> {
    let listing = load_listing_identity(db, listing_id)
        .await?
        .ok_or(IdentityAssignmentError::ListingNotFound(listing_id))?;
    let assignment = load_current_assignment(db, listing_id)
        .await?
        .ok_or(IdentityAssignmentError::Missing(listing_id))?;
    validate_assignment_dimensions(&assignment, grounding)?;
    validate_assignment_make(db, &assignment, listing.model_year, grounding).await?;
    validate_assignment_provenance(&assignment, grounding)?;
    if invalid_or_unresolved_material_dimensions(
        db,
        assignment.aircraft_designation_id,
        listing.model_year,
        assignment.aircraft_generation_id,
        assignment.aircraft_factory_package_id,
    )
    .await?
    {
        return Err(mismatch(
            listing_id,
            "current assignment has an invalid or unresolved curated generation or trim-tier dimension",
        ));
    }
    if !listing_has_exact_compatibility_projection(db, listing_id, assignment.assignment_id).await?
    {
        return Err(mismatch(
            listing_id,
            "current canonical assignment is not bound to the listing's exact valuation compatibility projection",
        ));
    }
    Ok(assignment.into_public())
}

/// Assign an identity through the non-mutating approved-catalog reuse path.
///
/// This boundary reloads the listing's current FAA admission immediately
/// before assignment and requires the complete FAA projection used during
/// curation to remain byte-for-byte identical. It may create listing-owned
/// assignment/projection state, but it never creates canonical catalog rows or
/// an FAA-to-designation relationship: that exact approved binding must
/// already exist.
pub async fn ensure_listing_identity_assignment_from_approved_catalog(
    db: &AppDb,
    listing_id: i64,
    grounded_during_curation: &AircraftGrounding,
) -> Result<EnsureIdentityAssignmentOutcome, IdentityAssignmentError> {
    let current = require_listing_faa_admission(db, listing_id)
        .await
        .map_err(|error| map_faa_admission_error(listing_id, error))?;
    let mismatches = exact_faa_projection_mismatches(grounded_during_curation, &current);
    if !mismatches.is_empty() {
        return Err(mismatch(
            listing_id,
            format!(
                "current FAA admission changed after curation ({})",
                mismatches.join(", ")
            ),
        ));
    }
    ensure_listing_identity_assignment_from_current_approved_catalog(db, listing_id, &current).await
}

fn map_faa_admission_error(
    listing_id: i64,
    error: AircraftAdmissionError,
) -> IdentityAssignmentError {
    match error {
        AircraftAdmissionError::ListingNotFound { .. } => {
            IdentityAssignmentError::ListingNotFound(listing_id)
        }
        AircraftAdmissionError::LookupFailed { message, .. } => {
            IdentityAssignmentError::Database(message)
        }
        AircraftAdmissionError::Rejected { .. } => mismatch(
            listing_id,
            format!("current FAA admission failed after curation: {error}"),
        ),
    }
}

fn exact_faa_projection_mismatches(
    expected: &AircraftGrounding,
    current: &AircraftGrounding,
) -> Vec<&'static str> {
    let mut mismatches = Vec::new();
    if expected.snapshot != current.snapshot {
        mismatches.push("snapshot");
    }
    if expected.n_number != current.n_number {
        mismatches.push("N-number");
    }
    if expected.source_record_sha256 != current.source_record_sha256 {
        mismatches.push("source record");
    }
    if expected.aircraft_code != current.aircraft_code {
        mismatches.push("aircraft code");
    }
    if expected.manufacturer_serial_raw != current.manufacturer_serial_raw
        || expected.manufacturer_serial_key != current.manufacturer_serial_key
    {
        mismatches.push("manufacturer serial");
    }
    let expected_model = expected
        .aircraft
        .as_ref()
        .and_then(|reference| reference.model_name.as_deref());
    let current_model = current
        .aircraft
        .as_ref()
        .and_then(|reference| reference.model_name.as_deref());
    if expected_model != current_model {
        mismatches.push("aircraft model");
    }
    mismatches
}

async fn ensure_listing_identity_assignment_from_current_approved_catalog(
    db: &AppDb,
    listing_id: i64,
    grounding: &AircraftGrounding,
) -> Result<EnsureIdentityAssignmentOutcome, IdentityAssignmentError> {
    let listing = load_listing_identity(db, listing_id)
        .await?
        .ok_or(IdentityAssignmentError::ListingNotFound(listing_id))?;
    let reference =
        grounding
            .aircraft
            .as_ref()
            .ok_or_else(|| IdentityAssignmentError::Mismatch {
                listing_id,
                reason: "current FAA record has no aircraft reference identity".to_string(),
            })?;
    let faa_make = nonblank(reference.manufacturer_name.as_deref()).ok_or_else(|| {
        IdentityAssignmentError::Mismatch {
            listing_id,
            reason: "current FAA record has no manufacturer".to_string(),
        }
    })?;
    let faa_model = nonblank(reference.model_name.as_deref()).ok_or_else(|| {
        IdentityAssignmentError::Mismatch {
            listing_id,
            reason: "current FAA record has no model designation".to_string(),
        }
    })?;

    let current = load_current_assignment(db, listing_id).await?;
    let candidate = if let Some(current) = current {
        if validate_assignment_dimensions(&current, grounding).is_err() {
            return Ok(pending(
                "current curated assignment disagrees with the listing or FAA identity",
                0,
            ));
        }
        if validate_assignment_make(db, &current, listing.model_year, grounding)
            .await
            .is_err()
        {
            return Ok(pending(
                "current curated make is not an unambiguous US/year match for the FAA manufacturer",
                0,
            ));
        }
        if invalid_or_unresolved_material_dimensions(
            db,
            current.aircraft_designation_id,
            listing.model_year,
            current.aircraft_generation_id,
            current.aircraft_factory_package_id,
        )
        .await?
        {
            return Ok(pending(
                "current assignment has an invalid or unresolved curated generation or trim-tier dimension",
                0,
            ));
        }
        if validate_assignment_provenance(&current, grounding).is_ok() {
            ensure_current_assignment_projection(db, &current).await?;
            return Ok(EnsureIdentityAssignmentOutcome::Current {
                assignment: current.into_public(),
            });
        }
        PromotionCandidate::from_current(&current)
    } else {
        let candidates = exact_promotion_candidates(db, listing.model_year, grounding).await?;
        if candidates.len() != 1 {
            let reason = if candidates.is_empty() {
                "no exact existing curated make/family/designation matches the listing and FAA record"
            } else {
                "multiple curated make/family/designation candidates match exactly"
            };
            return Ok(pending(reason, candidates.len()));
        }
        candidates.into_iter().next().expect("one candidate")
    };

    if !make_has_unambiguous_faa_identity(
        db,
        candidate.aircraft_make_id,
        candidate.aircraft_designation_id,
        faa_make,
        listing.model_year,
        grounding,
    )
    .await?
    {
        return Ok(pending(
            "curated make lacks the exact FAA-reported manufacturer label or approved alias",
            1,
        ));
    }

    if invalid_or_unresolved_material_dimensions(
        db,
        candidate.aircraft_designation_id,
        listing.model_year,
        candidate.aircraft_generation_id,
        candidate.aircraft_factory_package_id,
    )
    .await?
    {
        return Ok(pending(
            "curated generation or trim-tier dimensions are invalid or require an exact curation decision",
            1,
        ));
    }

    let bound_designation_id = faa_code_binding(
        db,
        &grounding.snapshot.snapshot_date,
        &grounding.snapshot.archive_sha256,
        &grounding.aircraft_code,
    )
    .await?;
    match bound_designation_id {
        Some(bound_designation_id) if bound_designation_id != candidate.aircraft_designation_id => {
            return Ok(pending(
                "FAA aircraft code is already bound to a different curated designation",
                1,
            ));
        }
        None => {
            return Ok(pending(
                "FAA aircraft code has no existing approved designation binding",
                1,
            ));
        }
        Some(_) => {}
    }

    let evidence = FaaIdentityEvidence::new(grounding, faa_make, faa_model);
    let supersedes_assignment_id = load_current_assignment(db, listing_id)
        .await?
        .map(|assignment| assignment.assignment_id);
    let assignment_id = persist_assignment_reusing_existing_binding(
        db,
        listing_id,
        &candidate,
        supersedes_assignment_id,
        grounding,
        &evidence,
    )
    .await?;
    let assignment = load_current_assignment(db, listing_id)
        .await?
        .filter(|assignment| assignment.assignment_id == assignment_id)
        .ok_or_else(|| IdentityAssignmentError::Mismatch {
            listing_id,
            reason: "new immutable assignment was not selected as current".to_string(),
        })?;
    validate_assignment_dimensions(&assignment, grounding)?;
    validate_assignment_make(db, &assignment, listing.model_year, grounding).await?;
    validate_assignment_provenance(&assignment, grounding)?;
    Ok(EnsureIdentityAssignmentOutcome::Assigned {
        assignment: assignment.into_public(),
    })
}

fn pending(reason: impl Into<String>, candidate_count: usize) -> EnsureIdentityAssignmentOutcome {
    EnsureIdentityAssignmentOutcome::PendingCuration {
        reason: reason.into(),
        candidate_count,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProjectionTransitionKind {
    Initial,
    CurrentRepair,
    Successor,
}

impl ProjectionTransitionKind {
    fn for_new_assignment(supersedes_assignment_id: Option<i64>) -> Self {
        if supersedes_assignment_id.is_some() {
            Self::Successor
        } else {
            Self::Initial
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::CurrentRepair => "current_repair",
            Self::Successor => "successor",
        }
    }
}

#[derive(Debug, FromRow)]
struct ListingIdentity {
    model_year: i64,
}

#[derive(Clone, Debug, FromRow)]
struct CurrentAssignment {
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
    faa_snapshot_date: String,
    faa_archive_sha256: String,
    faa_n_number: String,
    faa_aircraft_code: String,
    faa_source_record_sha256: String,
    created_at: String,
}

impl CurrentAssignment {
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

#[derive(Clone, Debug, FromRow)]
pub(crate) struct PromotionCandidate {
    pub(crate) aircraft_make_id: i64,
    pub(crate) make_name: String,
    pub(crate) aircraft_model_family_id: i64,
    pub(crate) family_name: String,
    pub(crate) aircraft_designation_id: i64,
    pub(crate) official_designation: String,
    pub(crate) identity_decision_id: i64,
    pub(crate) aircraft_generation_id: Option<i64>,
    pub(crate) aircraft_factory_package_id: Option<i64>,
}

impl PromotionCandidate {
    fn from_current(current: &CurrentAssignment) -> Self {
        Self {
            aircraft_make_id: current.aircraft_make_id,
            make_name: current.make_name.clone(),
            aircraft_model_family_id: current.aircraft_model_family_id,
            family_name: current.family_name.clone(),
            aircraft_designation_id: current.aircraft_designation_id,
            official_designation: current.official_designation.clone(),
            identity_decision_id: current.identity_decision_id,
            aircraft_generation_id: current.aircraft_generation_id,
            aircraft_factory_package_id: current.aircraft_factory_package_id,
        }
    }

    fn into_compatibility_identity(self) -> CanonicalAircraftCompatibilityIdentity {
        CanonicalAircraftCompatibilityIdentity {
            aircraft_make_id: self.aircraft_make_id,
            make_name: self.make_name,
            aircraft_model_family_id: self.aircraft_model_family_id,
            family_name: self.family_name,
            aircraft_designation_id: self.aircraft_designation_id,
            official_designation: self.official_designation,
            aircraft_generation_id: self.aircraft_generation_id,
            aircraft_factory_package_id: self.aircraft_factory_package_id,
        }
    }
}

async fn load_listing_identity(
    db: &AppDb,
    listing_id: i64,
) -> Result<Option<ListingIdentity>, IdentityAssignmentError> {
    let sql = db.sql(
        r#"
        SELECT listing.model_year
        FROM aircraft_sale_listings listing
        WHERE listing.id = ?
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ListingIdentity>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ListingIdentity>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(row)
}

async fn load_current_assignment(
    db: &AppDb,
    listing_id: i64,
) -> Result<Option<CurrentAssignment>, IdentityAssignmentError> {
    let sql = db.sql(
        r#"
        SELECT
          assignment.id AS assignment_id,
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
          assignment_snapshot.snapshot_date AS faa_snapshot_date,
          assignment_snapshot.archive_sha256 AS faa_archive_sha256,
          assignment.faa_n_number,
          binding.faa_aircraft_code,
          assignment.faa_source_record_sha256,
          assignment.created_at
        FROM aircraft_sale_listing_current_identity_assignments current_assignment
        JOIN aircraft_sale_listing_identity_assignments assignment
          ON assignment.id = current_assignment.identity_assignment_id
         AND assignment.aircraft_sale_listing_id = current_assignment.aircraft_sale_listing_id
        JOIN aircraft_makes make ON make.id = assignment.aircraft_make_id
        JOIN aircraft_model_families family
          ON family.id = assignment.aircraft_model_family_id
        JOIN aircraft_designations designation
          ON designation.id = assignment.aircraft_designation_id
        JOIN faa_registry_aircraft aircraft
          ON aircraft.snapshot_id = assignment.faa_registry_snapshot_id
         AND aircraft.n_number = assignment.faa_n_number
         AND aircraft.source_record_sha256 = assignment.faa_source_record_sha256
        JOIN faa_registry_snapshots assignment_snapshot
          ON assignment_snapshot.id = assignment.faa_registry_snapshot_id
        JOIN aircraft_designation_faa_bindings binding
          ON binding.faa_snapshot_date = assignment_snapshot.snapshot_date
         AND binding.faa_archive_sha256 = assignment_snapshot.archive_sha256
         AND binding.faa_aircraft_code = aircraft.aircraft_code
         AND binding.aircraft_designation_id = assignment.aircraft_designation_id
        WHERE current_assignment.aircraft_sale_listing_id = ?
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, CurrentAssignment>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, CurrentAssignment>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(row)
}

async fn listing_has_exact_compatibility_projection(
    db: &AppDb,
    listing_id: i64,
    assignment_id: i64,
) -> Result<bool, IdentityAssignmentError> {
    let sql = db.sql(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_exact_compatibility_projections
          WHERE listing_id = ?
            AND identity_assignment_id = ?
        )
        "#,
    );
    let exists = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(listing_id)
                .bind(assignment_id)
                .fetch_one(pool)
                .await?
                != 0
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, bool>(&sql)
                .bind(listing_id)
                .bind(assignment_id)
                .fetch_one(pool)
                .await?
        }
    };
    Ok(exists)
}

async fn ensure_current_assignment_projection(
    db: &AppDb,
    current: &CurrentAssignment,
) -> Result<(), IdentityAssignmentError> {
    if listing_has_exact_compatibility_projection(
        db,
        current.aircraft_sale_listing_id,
        current.assignment_id,
    )
    .await?
    {
        return Ok(());
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            execute_projection_transition_sqlite(
                &mut transaction,
                current.aircraft_sale_listing_id,
                current.assignment_id,
                ProjectionTransitionKind::CurrentRepair,
            )
            .await?;
            transaction.commit().await?;
        }
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool.begin().await?;
            execute_projection_transition_postgres(
                &mut transaction,
                current.aircraft_sale_listing_id,
                current.assignment_id,
                ProjectionTransitionKind::CurrentRepair,
            )
            .await?;
            transaction.commit().await?;
        }
    }

    if !listing_has_exact_compatibility_projection(
        db,
        current.aircraft_sale_listing_id,
        current.assignment_id,
    )
    .await?
    {
        return Err(mismatch(
            current.aircraft_sale_listing_id,
            "projection repair command did not produce an exact compatibility projection",
        ));
    }
    Ok(())
}

async fn exact_promotion_candidates(
    db: &AppDb,
    model_year: i64,
    grounding: &AircraftGrounding,
) -> Result<Vec<PromotionCandidate>, IdentityAssignmentError> {
    let reference =
        grounding
            .aircraft
            .as_ref()
            .ok_or_else(|| IdentityAssignmentError::Mismatch {
                listing_id: 0,
                reason: "current FAA record has no aircraft reference identity".to_string(),
            })?;
    let faa_make = nonblank(reference.manufacturer_name.as_deref()).ok_or_else(|| {
        IdentityAssignmentError::Mismatch {
            listing_id: 0,
            reason: "current FAA record has no manufacturer".to_string(),
        }
    })?;
    let faa_model = nonblank(reference.model_name.as_deref()).ok_or_else(|| {
        IdentityAssignmentError::Mismatch {
            listing_id: 0,
            reason: "current FAA record has no model designation".to_string(),
        }
    })?;
    let sql = r#"
        SELECT
          make.id AS aircraft_make_id,
          make.name AS make_name,
          family.id AS aircraft_model_family_id,
          family.name AS family_name,
          designation.id AS aircraft_designation_id,
          designation.official_designation,
          designation.approval_decision_id AS identity_decision_id,
          NULL AS aircraft_generation_id,
          NULL AS aircraft_factory_package_id
        FROM aircraft_designations designation
        JOIN aircraft_model_families family
          ON family.id = designation.aircraft_model_family_id
        JOIN aircraft_makes make ON make.id = family.aircraft_make_id
        ORDER BY make.id, family.id, designation.id
    "#;
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, PromotionCandidate>(sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, PromotionCandidate>(sql)
                .fetch_all(pool)
                .await?
        }
    };
    let mut candidates = Vec::new();
    for candidate in rows.into_iter().filter(|candidate| {
        normalize_aircraft_designator_retrieval_key(&candidate.official_designation)
            == normalize_aircraft_designator_retrieval_key(faa_model)
    }) {
        if make_has_unambiguous_faa_identity(
            db,
            candidate.aircraft_make_id,
            candidate.aircraft_designation_id,
            faa_make,
            model_year,
            grounding,
        )
        .await?
        {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

fn validate_assignment_dimensions(
    assignment: &CurrentAssignment,
    grounding: &AircraftGrounding,
) -> Result<(), IdentityAssignmentError> {
    let listing_id = assignment.aircraft_sale_listing_id;
    let reference = grounding.aircraft.as_ref().ok_or_else(|| {
        mismatch(
            listing_id,
            "current FAA record has no aircraft reference identity",
        )
    })?;
    let faa_model = nonblank(reference.model_name.as_deref())
        .ok_or_else(|| mismatch(listing_id, "current FAA record has no model designation"))?;
    if normalize_aircraft_designator_retrieval_key(&assignment.official_designation)
        != normalize_aircraft_designator_retrieval_key(faa_model)
    {
        return Err(mismatch(
            listing_id,
            "canonical designation projection disagrees",
        ));
    }
    Ok(())
}

async fn validate_assignment_make(
    db: &AppDb,
    assignment: &CurrentAssignment,
    model_year: i64,
    grounding: &AircraftGrounding,
) -> Result<(), IdentityAssignmentError> {
    let faa_make = grounding
        .aircraft
        .as_ref()
        .and_then(|reference| nonblank(reference.manufacturer_name.as_deref()))
        .ok_or_else(|| {
            mismatch(
                assignment.aircraft_sale_listing_id,
                "current FAA record has no manufacturer",
            )
        })?;
    if !make_has_unambiguous_faa_identity(
        db,
        assignment.aircraft_make_id,
        assignment.aircraft_designation_id,
        faa_make,
        model_year,
        grounding,
    )
    .await?
    {
        return Err(mismatch(
            assignment.aircraft_sale_listing_id,
            "canonical make is not an unambiguous US/year match for the FAA manufacturer",
        ));
    }
    Ok(())
}

fn validate_assignment_provenance(
    assignment: &CurrentAssignment,
    grounding: &AircraftGrounding,
) -> Result<(), IdentityAssignmentError> {
    let listing_id = assignment.aircraft_sale_listing_id;
    if assignment.faa_snapshot_date != grounding.snapshot.snapshot_date
        || assignment.faa_archive_sha256 != grounding.snapshot.archive_sha256
        || assignment.faa_n_number != grounding.n_number
        || assignment.faa_aircraft_code != grounding.aircraft_code
        || assignment.faa_source_record_sha256 != grounding.source_record_sha256
    {
        return Err(mismatch(
            listing_id,
            "assignment does not cite the exact current FAA source record",
        ));
    }
    Ok(())
}

fn mismatch(listing_id: i64, reason: impl Into<String>) -> IdentityAssignmentError {
    IdentityAssignmentError::Mismatch {
        listing_id,
        reason: reason.into(),
    }
}

#[derive(Debug, FromRow)]
struct MakeLabelRow {
    make_id: i64,
    make_name: String,
    alias: Option<String>,
    normalized_alias: Option<String>,
    valid_from_model_year: Option<i64>,
    valid_to_model_year: Option<i64>,
    market_code: Option<String>,
}

#[derive(Debug, FromRow)]
struct MakeLineageRow {
    aircraft_make_id: i64,
    aircraft_designation_id: i64,
    serial_prefix: String,
    serial_digits_width: i64,
    first_serial_number: i64,
    last_serial_number: Option<i64>,
}

fn lineage_serial_number(serial_key: &str, prefix: &str, digits_width: i64) -> Option<i64> {
    let serial_key = normalize_serial_key(serial_key)?;
    let digits = serial_key.strip_prefix(prefix)?;
    (i64::try_from(digits.len()).ok()? == digits_width
        && !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit()))
    .then(|| digits.parse::<i64>().ok())
    .flatten()
}

async fn matching_faa_label_make_ids(
    db: &AppDb,
    faa_make: &str,
    model_year: i64,
) -> Result<BTreeSet<i64>, IdentityAssignmentError> {
    let sql = r#"
        SELECT
          make.id AS make_id,
          make.name AS make_name,
          alias.alias,
          alias.normalized_alias,
          alias.valid_from_model_year,
          alias.valid_to_model_year,
          market.code AS market_code
        FROM aircraft_makes make
        LEFT JOIN aircraft_make_aliases alias ON alias.aircraft_make_id = make.id
        LEFT JOIN aircraft_markets market ON market.id = alias.aircraft_market_id
        ORDER BY make.id, alias.id
    "#;
    let labels = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, MakeLabelRow>(sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, MakeLabelRow>(sql)
                .fetch_all(pool)
                .await?
        }
    };
    let faa_key = normalize_aircraft_retrieval_text(faa_make);
    let mut matching_make_ids = BTreeSet::new();
    for label in labels {
        if normalize_aircraft_retrieval_text(&label.make_name) == faa_key {
            matching_make_ids.insert(label.make_id);
        }
        let (Some(alias), Some(normalized_alias)) = (label.alias, label.normalized_alias) else {
            continue;
        };
        let stored_key_is_deterministic =
            normalize_aircraft_retrieval_text(&alias) == normalized_alias;
        let year_is_applicable = label
            .valid_from_model_year
            .is_none_or(|from| from <= model_year)
            && label.valid_to_model_year.is_none_or(|to| to >= model_year);
        let market_is_applicable = label
            .market_code
            .as_deref()
            .is_none_or(|code| matches!(code, "GLOBAL" | "US"));
        if stored_key_is_deterministic
            && normalized_alias == faa_key
            && year_is_applicable
            && market_is_applicable
        {
            matching_make_ids.insert(label.make_id);
        }
    }
    Ok(matching_make_ids)
}

async fn matching_faa_lineage_make_ids(
    db: &AppDb,
    grounding: &AircraftGrounding,
    designation_id: i64,
) -> Result<BTreeSet<i64>, IdentityAssignmentError> {
    let Some(reference) = grounding.aircraft.as_ref() else {
        return Ok(BTreeSet::new());
    };
    let Some(faa_make) = nonblank(reference.manufacturer_name.as_deref()) else {
        return Ok(BTreeSet::new());
    };
    let Some(faa_model) = nonblank(reference.model_name.as_deref()) else {
        return Ok(BTreeSet::new());
    };
    let Some(serial_key) = grounding.manufacturer_serial_key.as_deref() else {
        return Ok(BTreeSet::new());
    };
    let sql = db.sql(
        r#"
        SELECT aircraft_make_id, aircraft_designation_id, serial_prefix,
               serial_digits_width, first_serial_number, last_serial_number
        FROM aircraft_tcds_make_lineage_bindings
        WHERE faa_snapshot_date = ?
          AND faa_archive_sha256 = ?
          AND faa_aircraft_code = ?
          AND faa_manufacturer_name = ?
          AND faa_model = ?
          AND aircraft_designation_id = ?
        ORDER BY id
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, MakeLineageRow>(&sql)
                .bind(&grounding.snapshot.snapshot_date)
                .bind(&grounding.snapshot.archive_sha256)
                .bind(&grounding.aircraft_code)
                .bind(faa_make)
                .bind(faa_model)
                .bind(designation_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, MakeLineageRow>(&sql)
                .bind(&grounding.snapshot.snapshot_date)
                .bind(&grounding.snapshot.archive_sha256)
                .bind(&grounding.aircraft_code)
                .bind(faa_make)
                .bind(faa_model)
                .bind(designation_id)
                .fetch_all(pool)
                .await?
        }
    };
    let mut make_ids = BTreeSet::new();
    for row in rows {
        if row.aircraft_designation_id != designation_id {
            continue;
        }
        let Some(number) =
            lineage_serial_number(serial_key, &row.serial_prefix, row.serial_digits_width)
        else {
            continue;
        };
        if number >= row.first_serial_number
            && row.last_serial_number.is_none_or(|last| number <= last)
        {
            make_ids.insert(row.aircraft_make_id);
        }
    }
    Ok(make_ids)
}

async fn make_has_unambiguous_faa_identity(
    db: &AppDb,
    make_id: i64,
    designation_id: i64,
    faa_make: &str,
    model_year: i64,
    grounding: &AircraftGrounding,
) -> Result<bool, IdentityAssignmentError> {
    let mut matching_make_ids = matching_faa_label_make_ids(db, faa_make, model_year).await?;
    matching_make_ids.extend(matching_faa_lineage_make_ids(db, grounding, designation_id).await?);
    Ok(matching_make_ids.len() == 1 && matching_make_ids.contains(&make_id))
}

async fn faa_code_binding(
    db: &AppDb,
    faa_snapshot_date: &str,
    faa_archive_sha256: &str,
    faa_aircraft_code: &str,
) -> Result<Option<i64>, IdentityAssignmentError> {
    let sql = db.sql(
        "SELECT aircraft_designation_id FROM aircraft_designation_faa_bindings WHERE faa_snapshot_date = ? AND faa_archive_sha256 = ? AND faa_aircraft_code = ?",
    );
    let id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(faa_snapshot_date)
                .bind(faa_archive_sha256)
                .bind(faa_aircraft_code)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(faa_snapshot_date)
                .bind(faa_archive_sha256)
                .bind(faa_aircraft_code)
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(id)
}

/// Return true when a curated dimension is omitted or when a selected
/// generation/package no longer applies to the listing's current model year.
/// The latter matters because assignments are immutable while model_year is a
/// corrected listing fact: changing the year must force a new assignment, not
/// silently retain an out-of-scope trim tier.
async fn invalid_or_unresolved_material_dimensions(
    db: &AppDb,
    designation_id: i64,
    model_year: i64,
    generation_id: Option<i64>,
    package_id: Option<i64>,
) -> Result<bool, IdentityAssignmentError> {
    let generation_sql = if generation_id.is_some() {
        db.sql(
            "SELECT EXISTS (SELECT 1 FROM aircraft_generation_designations WHERE aircraft_generation_id = ? AND aircraft_designation_id = ?)",
        )
    } else {
        db.sql(
            "SELECT EXISTS (SELECT 1 FROM aircraft_generation_designations WHERE aircraft_designation_id = ?)",
        )
    };
    let generation_valid = match (db.backend(), generation_id) {
        (DatabaseBackend::Sqlite(pool), Some(generation_id)) => {
            sqlx::query_scalar::<_, bool>(&generation_sql)
                .bind(generation_id)
                .bind(designation_id)
                .fetch_one(pool)
                .await?
        }
        (DatabaseBackend::Postgres(pool), Some(generation_id)) => {
            sqlx::query_scalar::<_, bool>(&generation_sql)
                .bind(generation_id)
                .bind(designation_id)
                .fetch_one(pool)
                .await?
        }
        (DatabaseBackend::Sqlite(pool), None) => {
            !sqlx::query_scalar::<_, bool>(&generation_sql)
                .bind(designation_id)
                .fetch_one(pool)
                .await?
        }
        (DatabaseBackend::Postgres(pool), None) => {
            !sqlx::query_scalar::<_, bool>(&generation_sql)
                .bind(designation_id)
                .fetch_one(pool)
                .await?
        }
    };
    if !generation_valid {
        return Ok(true);
    }

    let package_sql = if package_id.is_some() {
        db.sql(
            r#"
            SELECT EXISTS (
              SELECT 1 FROM aircraft_package_applicability applicability
              WHERE applicability.aircraft_factory_package_id = ?
                AND applicability.aircraft_designation_id = ?
                AND (applicability.aircraft_generation_id IS NULL
                  OR applicability.aircraft_generation_id = ?)
                AND (applicability.valid_from_model_year IS NULL
                  OR applicability.valid_from_model_year <= ?)
                AND (applicability.valid_to_model_year IS NULL
                  OR applicability.valid_to_model_year >= ?)
            )
            "#,
        )
    } else {
        db.sql(
            r#"
            SELECT EXISTS (
              SELECT 1
              FROM aircraft_package_applicability applicability
              JOIN aircraft_factory_packages package
                ON package.id = applicability.aircraft_factory_package_id
              WHERE applicability.aircraft_designation_id = ?
                AND package.package_kind = 'trim_tier'
                AND (applicability.aircraft_generation_id IS NULL
                  OR applicability.aircraft_generation_id = ?)
                AND (applicability.valid_from_model_year IS NULL
                  OR applicability.valid_from_model_year <= ?)
                AND (applicability.valid_to_model_year IS NULL
                  OR applicability.valid_to_model_year >= ?)
            )
            "#,
        )
    };
    let package_valid = match (db.backend(), package_id) {
        (DatabaseBackend::Sqlite(pool), Some(package_id)) => {
            sqlx::query_scalar::<_, bool>(&package_sql)
                .bind(package_id)
                .bind(designation_id)
                .bind(generation_id)
                .bind(model_year)
                .bind(model_year)
                .fetch_one(pool)
                .await?
        }
        (DatabaseBackend::Postgres(pool), Some(package_id)) => {
            sqlx::query_scalar::<_, bool>(&package_sql)
                .bind(package_id)
                .bind(designation_id)
                .bind(generation_id)
                .bind(model_year)
                .bind(model_year)
                .fetch_one(pool)
                .await?
        }
        (DatabaseBackend::Sqlite(pool), None) => {
            !sqlx::query_scalar::<_, bool>(&package_sql)
                .bind(designation_id)
                .bind(generation_id)
                .bind(model_year)
                .bind(model_year)
                .fetch_one(pool)
                .await?
        }
        (DatabaseBackend::Postgres(pool), None) => {
            !sqlx::query_scalar::<_, bool>(&package_sql)
                .bind(designation_id)
                .bind(generation_id)
                .bind(model_year)
                .bind(model_year)
                .fetch_one(pool)
                .await?
        }
    };
    Ok(!package_valid)
}

pub(crate) struct FaaIdentityEvidence {
    subject: String,
    predicate: &'static str,
    object: String,
    quote: String,
}

impl FaaIdentityEvidence {
    pub(crate) fn new(grounding: &AircraftGrounding, manufacturer: &str, model: &str) -> Self {
        Self {
            subject: grounding.n_number.clone(),
            predicate: "FAA registered aircraft identity",
            object: serde_json::json!({
                "aircraft_code": grounding.aircraft_code,
                "manufacturer": manufacturer,
                "model": model,
                "source_record_sha256": grounding.source_record_sha256,
            })
            .to_string(),
            quote: format!(
                "FAA ACFTREF {}: {} {}; MASTER {} record sha256 {}",
                grounding.aircraft_code,
                manufacturer,
                model,
                grounding.n_number,
                grounding.source_record_sha256
            ),
        }
    }
}

#[cfg(test)]
async fn persist_assignment(
    db: &AppDb,
    listing_id: i64,
    candidate: &PromotionCandidate,
    supersedes_assignment_id: Option<i64>,
    grounding: &AircraftGrounding,
    evidence: &FaaIdentityEvidence,
) -> Result<i64, IdentityAssignmentError> {
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            let assignment_id = persist_assignment_sqlite_in_transaction(
                &mut transaction,
                listing_id,
                candidate,
                supersedes_assignment_id,
                grounding,
                evidence,
            )
            .await?;
            transaction.commit().await?;
            Ok(assignment_id)
        }
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool.begin().await?;
            let assignment_id = persist_assignment_postgres_in_transaction(
                &mut transaction,
                listing_id,
                candidate,
                supersedes_assignment_id,
                grounding,
                evidence,
            )
            .await?;
            transaction.commit().await?;
            if !listing_has_exact_compatibility_projection(db, listing_id, assignment_id).await? {
                return Err(mismatch(
                    listing_id,
                    "committed Postgres assignment lacks its exact valuation compatibility projection",
                ));
            }
            Ok(assignment_id)
        }
    }
}

async fn persist_assignment_reusing_existing_binding(
    db: &AppDb,
    listing_id: i64,
    candidate: &PromotionCandidate,
    supersedes_assignment_id: Option<i64>,
    grounding: &AircraftGrounding,
    evidence: &FaaIdentityEvidence,
) -> Result<i64, IdentityAssignmentError> {
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            require_existing_faa_binding_sqlite(
                &mut transaction,
                listing_id,
                candidate.aircraft_designation_id,
                grounding,
            )
            .await?;
            let claim_id = find_or_insert_claim_sqlite(
                &mut transaction,
                grounding.snapshot.evidence_source_id,
                evidence,
            )
            .await?;
            let assignment_id = insert_assignment_sqlite(
                &mut transaction,
                listing_id,
                candidate,
                supersedes_assignment_id,
                grounding,
                claim_id,
            )
            .await?;
            execute_projection_transition_sqlite(
                &mut transaction,
                listing_id,
                assignment_id,
                ProjectionTransitionKind::for_new_assignment(supersedes_assignment_id),
            )
            .await?;
            transaction.commit().await?;
            Ok(assignment_id)
        }
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool.begin().await?;
            require_existing_faa_binding_postgres(
                &mut transaction,
                listing_id,
                candidate.aircraft_designation_id,
                grounding,
            )
            .await?;
            let claim_id = find_or_insert_claim_postgres(
                &mut transaction,
                grounding.snapshot.evidence_source_id,
                evidence,
            )
            .await?;
            let assignment_id = insert_assignment_postgres(
                &mut transaction,
                listing_id,
                candidate,
                supersedes_assignment_id,
                grounding,
                claim_id,
            )
            .await?;
            execute_projection_transition_postgres(
                &mut transaction,
                listing_id,
                assignment_id,
                ProjectionTransitionKind::for_new_assignment(supersedes_assignment_id),
            )
            .await?;
            transaction.commit().await?;
            if !listing_has_exact_compatibility_projection(db, listing_id, assignment_id).await? {
                return Err(mismatch(
                    listing_id,
                    "committed Postgres assignment lacks its exact valuation compatibility projection",
                ));
            }
            Ok(assignment_id)
        }
    }
}

async fn require_existing_faa_binding_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    listing_id: i64,
    aircraft_designation_id: i64,
    grounding: &AircraftGrounding,
) -> Result<(), IdentityAssignmentError> {
    let bound = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT aircraft_designation_id
        FROM aircraft_designation_faa_bindings
        WHERE faa_snapshot_date = ?
          AND faa_archive_sha256 = ?
          AND faa_aircraft_code = ?
        "#,
    )
    .bind(&grounding.snapshot.snapshot_date)
    .bind(&grounding.snapshot.archive_sha256)
    .bind(&grounding.aircraft_code)
    .fetch_optional(&mut **transaction)
    .await?;
    match bound {
        Some(bound) if bound == aircraft_designation_id => Ok(()),
        Some(_) => Err(mismatch(
            listing_id,
            "FAA aircraft code binding changed before approved-catalog assignment",
        )),
        None => Err(mismatch(
            listing_id,
            "approved-catalog assignment requires an existing FAA aircraft code binding",
        )),
    }
}

async fn require_existing_faa_binding_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    listing_id: i64,
    aircraft_designation_id: i64,
    grounding: &AircraftGrounding,
) -> Result<(), IdentityAssignmentError> {
    let bound = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT aircraft_designation_id
        FROM aircraft_designation_faa_bindings
        WHERE faa_snapshot_date = $1
          AND faa_archive_sha256 = $2
          AND faa_aircraft_code = $3
        "#,
    )
    .bind(&grounding.snapshot.snapshot_date)
    .bind(&grounding.snapshot.archive_sha256)
    .bind(&grounding.aircraft_code)
    .fetch_optional(&mut **transaction)
    .await?;
    match bound {
        Some(bound) if bound == aircraft_designation_id => Ok(()),
        Some(_) => Err(mismatch(
            listing_id,
            "FAA aircraft code binding changed before approved-catalog assignment",
        )),
        None => Err(mismatch(
            listing_id,
            "approved-catalog assignment requires an existing FAA aircraft code binding",
        )),
    }
}

/// Persist one exact, already-validated canonical candidate and its FAA-bound
/// listing assignment inside the caller's SQLite transaction.
///
/// Catalog curation uses this boundary so catalog rows, FAA bindings, the
/// immutable assignment, its current pointer, and the valuation projection are
/// committed or rolled back together. Candidate selection and hierarchy
/// validation deliberately remain the caller's responsibility.
pub(crate) async fn persist_assignment_sqlite_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    listing_id: i64,
    candidate: &PromotionCandidate,
    supersedes_assignment_id: Option<i64>,
    grounding: &AircraftGrounding,
    evidence: &FaaIdentityEvidence,
) -> Result<i64, IdentityAssignmentError> {
    let claim_id =
        find_or_insert_claim_sqlite(transaction, grounding.snapshot.evidence_source_id, evidence)
            .await?;
    sqlx::query(
        r#"
        INSERT INTO aircraft_designation_faa_bindings (
          faa_snapshot_date, faa_archive_sha256, faa_aircraft_code,
          aircraft_designation_id, representative_faa_registry_snapshot_id,
          identity_evidence_claim_id
        ) VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT (faa_snapshot_date, faa_archive_sha256, faa_aircraft_code) DO NOTHING
        "#,
    )
    .bind(&grounding.snapshot.snapshot_date)
    .bind(&grounding.snapshot.archive_sha256)
    .bind(&grounding.aircraft_code)
    .bind(candidate.aircraft_designation_id)
    .bind(grounding.snapshot.id)
    .bind(claim_id)
    .execute(&mut **transaction)
    .await?;
    let bound = sqlx::query_scalar::<_, i64>(
        "SELECT aircraft_designation_id FROM aircraft_designation_faa_bindings WHERE faa_snapshot_date = ? AND faa_archive_sha256 = ? AND faa_aircraft_code = ?",
    )
    .bind(&grounding.snapshot.snapshot_date)
    .bind(&grounding.snapshot.archive_sha256)
    .bind(&grounding.aircraft_code)
    .fetch_one(&mut **transaction)
    .await?;
    if bound != candidate.aircraft_designation_id {
        return Err(mismatch(listing_id, "FAA aircraft code binding collision"));
    }
    let assignment_id = insert_assignment_sqlite(
        transaction,
        listing_id,
        candidate,
        supersedes_assignment_id,
        grounding,
        claim_id,
    )
    .await?;
    execute_projection_transition_sqlite(
        transaction,
        listing_id,
        assignment_id,
        ProjectionTransitionKind::for_new_assignment(supersedes_assignment_id),
    )
    .await?;
    Ok(assignment_id)
}

/// PostgreSQL counterpart of
/// [`persist_assignment_sqlite_in_transaction`].
pub(crate) async fn persist_assignment_postgres_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    listing_id: i64,
    candidate: &PromotionCandidate,
    supersedes_assignment_id: Option<i64>,
    grounding: &AircraftGrounding,
    evidence: &FaaIdentityEvidence,
) -> Result<i64, IdentityAssignmentError> {
    let claim_id =
        find_or_insert_claim_postgres(transaction, grounding.snapshot.evidence_source_id, evidence)
            .await?;
    sqlx::query(
        r#"
        INSERT INTO aircraft_designation_faa_bindings (
          faa_snapshot_date, faa_archive_sha256, faa_aircraft_code,
          aircraft_designation_id, representative_faa_registry_snapshot_id,
          identity_evidence_claim_id
        ) VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (faa_snapshot_date, faa_archive_sha256, faa_aircraft_code) DO NOTHING
        "#,
    )
    .bind(&grounding.snapshot.snapshot_date)
    .bind(&grounding.snapshot.archive_sha256)
    .bind(&grounding.aircraft_code)
    .bind(candidate.aircraft_designation_id)
    .bind(grounding.snapshot.id)
    .bind(claim_id)
    .execute(&mut **transaction)
    .await?;
    let bound = sqlx::query_scalar::<_, i64>(
        "SELECT aircraft_designation_id FROM aircraft_designation_faa_bindings WHERE faa_snapshot_date = $1 AND faa_archive_sha256 = $2 AND faa_aircraft_code = $3",
    )
    .bind(&grounding.snapshot.snapshot_date)
    .bind(&grounding.snapshot.archive_sha256)
    .bind(&grounding.aircraft_code)
    .fetch_one(&mut **transaction)
    .await?;
    if bound != candidate.aircraft_designation_id {
        return Err(mismatch(listing_id, "FAA aircraft code binding collision"));
    }
    let assignment_id = insert_assignment_postgres(
        transaction,
        listing_id,
        candidate,
        supersedes_assignment_id,
        grounding,
        claim_id,
    )
    .await?;
    execute_projection_transition_postgres(
        transaction,
        listing_id,
        assignment_id,
        ProjectionTransitionKind::for_new_assignment(supersedes_assignment_id),
    )
    .await?;
    Ok(assignment_id)
}

async fn execute_projection_transition_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    listing_id: i64,
    assignment_id: i64,
    transition_kind: ProjectionTransitionKind,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO aircraft_valuation_projection_transitions (
          aircraft_sale_listing_id,
          identity_assignment_id,
          transition_kind,
          selected_at
        ) VALUES (?, ?, ?, ?)
        "#,
    )
    .bind(listing_id)
    .bind(assignment_id)
    .bind(transition_kind.as_str())
    .bind(pointer_timestamp())
    .execute(&mut **transaction)
    .await?;

    let remaining = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM aircraft_valuation_projection_transitions
        WHERE aircraft_sale_listing_id = ?
        "#,
    )
    .bind(listing_id)
    .fetch_one(&mut **transaction)
    .await?;
    if remaining != 0 {
        return Err(sqlx::Error::Protocol(
            "aircraft projection command did not consume itself".to_string(),
        ));
    }
    Ok(())
}

async fn execute_projection_transition_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    listing_id: i64,
    assignment_id: i64,
    transition_kind: ProjectionTransitionKind,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO aircraft_valuation_projection_transitions (
          aircraft_sale_listing_id,
          identity_assignment_id,
          transition_kind,
          selected_at
        ) VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(listing_id)
    .bind(assignment_id)
    .bind(transition_kind.as_str())
    .bind(pointer_timestamp())
    .execute(&mut **transaction)
    .await?;

    let remaining = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM aircraft_valuation_projection_transitions
        WHERE aircraft_sale_listing_id = $1
        "#,
    )
    .bind(listing_id)
    .fetch_one(&mut **transaction)
    .await?;
    if remaining != 0 {
        return Err(sqlx::Error::Protocol(
            "aircraft projection command did not consume itself".to_string(),
        ));
    }
    Ok(())
}

async fn find_or_insert_claim_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    evidence_source_id: i64,
    evidence: &FaaIdentityEvidence,
) -> Result<i64, sqlx::Error> {
    if let Some(id) = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM curation_evidence_claims
        WHERE evidence_source_id = ? AND claim_kind = 'identity'
          AND subject_text = ? AND predicate_text = ? AND object_text = ?
          AND quoted_evidence = ? AND validation_status = 'validated'
        ORDER BY id LIMIT 1
        "#,
    )
    .bind(evidence_source_id)
    .bind(&evidence.subject)
    .bind(evidence.predicate)
    .bind(&evidence.object)
    .bind(&evidence.quote)
    .fetch_optional(&mut **transaction)
    .await?
    {
        return Ok(id);
    }
    sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO curation_evidence_claims (
          evidence_source_id, claim_kind, subject_text, predicate_text,
          object_text, quoted_evidence, validation_status, validated_at
        ) VALUES (?, 'identity', ?, ?, ?, ?, 'validated', CURRENT_TIMESTAMP)
        RETURNING id
        "#,
    )
    .bind(evidence_source_id)
    .bind(&evidence.subject)
    .bind(evidence.predicate)
    .bind(&evidence.object)
    .bind(&evidence.quote)
    .fetch_one(&mut **transaction)
    .await
}

async fn find_or_insert_claim_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    evidence_source_id: i64,
    evidence: &FaaIdentityEvidence,
) -> Result<i64, sqlx::Error> {
    if let Some(id) = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM curation_evidence_claims
        WHERE evidence_source_id = $1 AND claim_kind = 'identity'
          AND subject_text = $2 AND predicate_text = $3 AND object_text = $4
          AND quoted_evidence = $5 AND validation_status = 'validated'
        ORDER BY id LIMIT 1
        "#,
    )
    .bind(evidence_source_id)
    .bind(&evidence.subject)
    .bind(evidence.predicate)
    .bind(&evidence.object)
    .bind(&evidence.quote)
    .fetch_optional(&mut **transaction)
    .await?
    {
        return Ok(id);
    }
    sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO curation_evidence_claims (
          evidence_source_id, claim_kind, subject_text, predicate_text,
          object_text, quoted_evidence, validation_status, validated_at
        ) VALUES ($1, 'identity', $2, $3, $4, $5, 'validated', CURRENT_TIMESTAMP)
        RETURNING id
        "#,
    )
    .bind(evidence_source_id)
    .bind(&evidence.subject)
    .bind(evidence.predicate)
    .bind(&evidence.object)
    .bind(&evidence.quote)
    .fetch_one(&mut **transaction)
    .await
}

async fn insert_assignment_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    listing_id: i64,
    candidate: &PromotionCandidate,
    supersedes_assignment_id: Option<i64>,
    grounding: &AircraftGrounding,
    claim_id: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO aircraft_sale_listing_identity_assignments (
          aircraft_sale_listing_id, supersedes_assignment_id,
          aircraft_make_id, aircraft_model_family_id, aircraft_designation_id,
          aircraft_generation_id, aircraft_factory_package_id,
          identity_decision_id, identity_evidence_claim_id,
          faa_registry_snapshot_id, faa_n_number, faa_source_record_sha256
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        RETURNING id
        "#,
    )
    .bind(listing_id)
    .bind(supersedes_assignment_id)
    .bind(candidate.aircraft_make_id)
    .bind(candidate.aircraft_model_family_id)
    .bind(candidate.aircraft_designation_id)
    .bind(candidate.aircraft_generation_id)
    .bind(candidate.aircraft_factory_package_id)
    .bind(candidate.identity_decision_id)
    .bind(claim_id)
    .bind(grounding.snapshot.id)
    .bind(&grounding.n_number)
    .bind(&grounding.source_record_sha256)
    .fetch_one(&mut **transaction)
    .await
}

async fn insert_assignment_postgres(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    listing_id: i64,
    candidate: &PromotionCandidate,
    supersedes_assignment_id: Option<i64>,
    grounding: &AircraftGrounding,
    claim_id: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO aircraft_sale_listing_identity_assignments (
          aircraft_sale_listing_id, supersedes_assignment_id,
          aircraft_make_id, aircraft_model_family_id, aircraft_designation_id,
          aircraft_generation_id, aircraft_factory_package_id,
          identity_decision_id, identity_evidence_claim_id,
          faa_registry_snapshot_id, faa_n_number, faa_source_record_sha256
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
        RETURNING id
        "#,
    )
    .bind(listing_id)
    .bind(supersedes_assignment_id)
    .bind(candidate.aircraft_make_id)
    .bind(candidate.aircraft_model_family_id)
    .bind(candidate.aircraft_designation_id)
    .bind(candidate.aircraft_generation_id)
    .bind(candidate.aircraft_factory_package_id)
    .bind(candidate.identity_decision_id)
    .bind(claim_id)
    .bind(grounding.snapshot.id)
    .bind(&grounding.n_number)
    .bind(&grounding.source_record_sha256)
    .fetch_one(&mut **transaction)
    .await
}

fn pointer_timestamp() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("z{:020}.{:09}", duration.as_secs(), duration.subsec_nanos())
}

fn nonblank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Build the smallest honest curated hierarchy needed by unit tests, then
/// exercise the same FAA binding/assignment path as production. The FAA claim
/// proves make and designation. The family grouping is explicitly labeled as
/// test-fixture context rather than regulator evidence.
#[cfg(test)]
pub(crate) async fn seed_test_curated_identity_assignment(
    db: &AppDb,
    listing_id: i64,
    grounding: &AircraftGrounding,
) -> Result<CanonicalAircraftIdentityAssignment, IdentityAssignmentError> {
    seed_test_curated_identity_assignment_with_make_alias(db, listing_id, grounding, None).await
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct TestMakeAlias<'a> {
    canonical_make: &'a str,
    valid_from_model_year: Option<i64>,
    valid_to_model_year: Option<i64>,
    market_code: &'a str,
}

#[cfg(test)]
async fn seed_test_curated_identity_assignment_with_make_alias(
    db: &AppDb,
    listing_id: i64,
    grounding: &AircraftGrounding,
    make_alias: Option<TestMakeAlias<'_>>,
) -> Result<CanonicalAircraftIdentityAssignment, IdentityAssignmentError> {
    use sha2::{Digest, Sha256};

    load_listing_identity(db, listing_id)
        .await?
        .ok_or(IdentityAssignmentError::ListingNotFound(listing_id))?;
    let reference = grounding
        .aircraft
        .as_ref()
        .ok_or_else(|| mismatch(listing_id, "test FAA grounding lacks aircraft identity"))?;
    let faa_make = nonblank(reference.manufacturer_name.as_deref())
        .ok_or_else(|| mismatch(listing_id, "test FAA grounding lacks manufacturer"))?;
    let faa_model = nonblank(reference.model_name.as_deref())
        .ok_or_else(|| mismatch(listing_id, "test FAA grounding lacks model"))?;
    let evidence = FaaIdentityEvidence::new(grounding, faa_make, faa_model);

    let DatabaseBackend::Sqlite(pool) = db.backend() else {
        return Err(IdentityAssignmentError::Database(
            "test curated identity seeding currently supports SQLite fixtures only".to_string(),
        ));
    };
    let family_name = match sqlx::query_scalar::<_, String>(
        r#"
        SELECT observed_family
        FROM aircraft_listing_identity_input_observations
        WHERE aircraft_sale_listing_id = ?
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(listing_id)
    .fetch_optional(pool)
    .await?
    {
        Some(family_name) => family_name,
        None => {
            sqlx::query_scalar::<_, String>(
                r#"
            SELECT model.name
            FROM aircraft_sale_listings listing
            JOIN aircraft_model_variants variant
              ON variant.id = listing.aircraft_model_variant_id
            JOIN aircraft_models model
              ON model.id = variant.aircraft_model_id
            WHERE listing.id = ?
            "#,
            )
            .bind(listing_id)
            .fetch_one(pool)
            .await?
        }
    };
    let family_normalized = normalize_aircraft_retrieval_text(&family_name);
    let mut transaction = pool.begin().await?;
    let claim_id = find_or_insert_claim_sqlite(
        &mut transaction,
        grounding.snapshot.evidence_source_id,
        &evidence,
    )
    .await?;
    let exact_source_evidence = format!(
        "TEST FIXTURE: FAA proves {} {}; listing context groups it under family {}",
        faa_make, faa_model, family_name
    );
    let observation_digest = format!(
        "{:x}",
        Sha256::digest(format!(
            "test-aircraft-identity:{listing_id}:{}:{}",
            grounding.snapshot.id, grounding.source_record_sha256
        ))
    );
    sqlx::query(
        r#"
        INSERT INTO aircraft_identity_observations (
          aircraft_sale_listing_id, observed_make, observed_family,
          observed_designation, exact_source_evidence, observation_sha256
        ) VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT (observation_sha256) DO NOTHING
        "#,
    )
    .bind(listing_id)
    .bind(faa_make)
    .bind(&family_name)
    .bind(faa_model)
    .bind(&exact_source_evidence)
    .bind(&observation_digest)
    .execute(&mut *transaction)
    .await?;
    let observation_id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM aircraft_identity_observations WHERE observation_sha256 = ?",
    )
    .bind(&observation_digest)
    .fetch_one(&mut *transaction)
    .await?;

    let catalog_make = make_alias
        .map(|alias| alias.canonical_make)
        .unwrap_or(faa_make);
    let make_normalized = normalize_aircraft_retrieval_text(catalog_make);
    let make_row = sqlx::query_as::<_, TestCatalogRow>(
        "SELECT id, approval_decision_id FROM aircraft_makes WHERE normalized_name = ?",
    )
    .bind(&make_normalized)
    .fetch_optional(&mut *transaction)
    .await?;
    let make_id = if let Some(row) = make_row {
        row.id
    } else {
        let decision_id = seed_test_approval_decision_sqlite(
            &mut transaction,
            observation_id,
            listing_id,
            grounding.snapshot.id,
            "make",
            "make",
            claim_id,
            "FAA manufacturer identity used only for a test fixture",
        )
        .await?;
        sqlx::query_scalar::<_, i64>(
            "INSERT INTO aircraft_makes (name, normalized_name, approval_decision_id) VALUES (?, ?, ?) RETURNING id",
        )
        .bind(catalog_make)
        .bind(&make_normalized)
        .bind(decision_id)
        .fetch_one(&mut *transaction)
        .await?
    };

    if let Some(alias) = make_alias {
        let alias_decision_id = seed_test_approval_decision_sqlite(
            &mut transaction,
            observation_id,
            listing_id,
            grounding.snapshot.id,
            "make",
            "alias",
            claim_id,
            "FAA manufacturer alias used only for a test fixture",
        )
        .await?;
        let market_id =
            sqlx::query_scalar::<_, i64>("SELECT id FROM aircraft_markets WHERE code = ?")
                .bind(alias.market_code)
                .fetch_one(&mut *transaction)
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
        .bind(faa_make)
        .bind(normalize_aircraft_retrieval_text(faa_make))
        .bind(alias.valid_from_model_year)
        .bind(alias.valid_to_model_year)
        .bind(market_id)
        .bind(alias_decision_id)
        .execute(&mut *transaction)
        .await?;
    }

    let family_row = sqlx::query_as::<_, TestCatalogRow>(
        "SELECT id, approval_decision_id FROM aircraft_model_families WHERE aircraft_make_id = ? AND normalized_name = ?",
    )
    .bind(make_id)
    .bind(&family_normalized)
    .fetch_optional(&mut *transaction)
    .await?;
    let family_id = if let Some(row) = family_row {
        row.id
    } else {
        let decision_id = seed_test_approval_decision_sqlite(
            &mut transaction,
            observation_id,
            listing_id,
            grounding.snapshot.id,
            "family",
            "family",
            claim_id,
            "TEST FIXTURE ONLY: listing family grouping; FAA does not independently prove this hierarchy",
        )
        .await?;
        sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_model_families (
              aircraft_make_id, name, normalized_name, approval_decision_id
            ) VALUES (?, ?, ?, ?) RETURNING id
            "#,
        )
        .bind(make_id)
        .bind(&family_name)
        .bind(&family_normalized)
        .bind(decision_id)
        .fetch_one(&mut *transaction)
        .await?
    };

    let designation_normalized = normalize_aircraft_designator_retrieval_key(faa_model);
    let designation_row = sqlx::query_as::<_, TestCatalogRow>(
        r#"
        SELECT id, approval_decision_id
        FROM aircraft_designations
        WHERE aircraft_model_family_id = ?
          AND normalized_official_designation = ?
        "#,
    )
    .bind(family_id)
    .bind(&designation_normalized)
    .fetch_optional(&mut *transaction)
    .await?;
    let (designation_id, designation_decision_id) = if let Some(row) = designation_row {
        (row.id, row.approval_decision_id)
    } else {
        let decision_id = seed_test_approval_decision_sqlite(
            &mut transaction,
            observation_id,
            listing_id,
            grounding.snapshot.id,
            "designation",
            "designation",
            claim_id,
            "FAA model designation identity used only for a test fixture",
        )
        .await?;
        let designation_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_designations (
              aircraft_model_family_id, official_designation,
              normalized_official_designation, display_name, approval_decision_id
            ) VALUES (?, ?, ?, ?, ?) RETURNING id
            "#,
        )
        .bind(family_id)
        .bind(faa_model)
        .bind(&designation_normalized)
        .bind(format!("{faa_make} {faa_model}"))
        .bind(decision_id)
        .fetch_one(&mut *transaction)
        .await?;
        (designation_id, decision_id)
    };
    transaction.commit().await?;

    let candidate = PromotionCandidate {
        aircraft_make_id: make_id,
        make_name: catalog_make.to_string(),
        aircraft_model_family_id: family_id,
        family_name,
        aircraft_designation_id: designation_id,
        official_designation: faa_model.to_string(),
        identity_decision_id: designation_decision_id,
        aircraft_generation_id: None,
        aircraft_factory_package_id: None,
    };
    let supersedes_assignment_id = load_current_assignment(db, listing_id)
        .await?
        .map(|assignment| assignment.assignment_id);
    let assignment_id = persist_assignment(
        db,
        listing_id,
        &candidate,
        supersedes_assignment_id,
        grounding,
        &evidence,
    )
    .await?;
    require_listing_identity_assignment(db, listing_id, grounding)
        .await
        .and_then(|assignment| {
            if assignment.assignment_id == assignment_id {
                Ok(assignment)
            } else {
                Err(mismatch(listing_id, "test assignment was not selected"))
            }
        })
}

#[cfg(test)]
#[derive(FromRow)]
struct TestCatalogRow {
    id: i64,
    approval_decision_id: i64,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn seed_test_approval_decision_sqlite(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    observation_id: i64,
    listing_id: i64,
    snapshot_id: i64,
    scope: &str,
    entity_kind: &str,
    claim_id: i64,
    rationale: &str,
) -> Result<i64, sqlx::Error> {
    let fingerprint = format!("test-aircraft-identity:{listing_id}:{snapshot_id}:{scope}");
    sqlx::query(
        r#"
        INSERT INTO aircraft_identity_resolution_cases (
          observation_id, resolution_scope, job_fingerprint,
          catalog_revision, case_status
        ) VALUES (?, ?, ?, 'test-fixture-v1', 'resolved')
        ON CONFLICT (job_fingerprint) DO NOTHING
        "#,
    )
    .bind(observation_id)
    .bind(scope)
    .bind(&fingerprint)
    .execute(&mut **transaction)
    .await?;
    let case_id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM aircraft_identity_resolution_cases WHERE job_fingerprint = ?",
    )
    .bind(&fingerprint)
    .fetch_one(&mut **transaction)
    .await?;
    if let Some(id) = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM aircraft_identity_decisions
        WHERE resolution_case_id = ? AND entity_kind = ?
          AND decision_action = 'approve_new' AND decision_status = 'approved'
        ORDER BY id LIMIT 1
        "#,
    )
    .bind(case_id)
    .bind(entity_kind)
    .fetch_optional(&mut **transaction)
    .await?
    {
        return Ok(id);
    }
    let decision_id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO aircraft_identity_decisions (
          resolution_case_id, entity_kind, decision_action, decision_status,
          decision_payload_json, deterministic_validation_json,
          deterministic_validation_passed, rationale, decided_at
        ) VALUES (
          ?, ?, 'approve_new', 'approved',
          '{"test_fixture":true}', '{"passed":true}', 1, ?, CURRENT_TIMESTAMP
        ) RETURNING id
        "#,
    )
    .bind(case_id)
    .bind(entity_kind)
    .bind(rationale)
    .fetch_one(&mut **transaction)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO aircraft_identity_decision_claims (
          decision_id, evidence_claim_id, evidence_role
        ) VALUES (?, ?, 'identity')
        "#,
    )
    .bind(decision_id)
    .bind(claim_id)
    .execute(&mut **transaction)
    .await?;
    Ok(decision_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::faa::{
        require_listing_admission, require_listing_faa_admission, store_release, AircraftRecord,
        AircraftReference, MemberProvenance, Release, ReleaseMetadata, TargetCoverage,
        AIRCRAFT_MEMBER_NAME, ENGINE_MEMBER_NAME, MASTER_MEMBER_NAME,
    };

    fn faa_release(snapshot_date: &str, digest: char, target_digest: char) -> Release {
        let digest_string = digest.to_string().repeat(64);
        let source_record = if digest == 'a' { '1' } else { '2' };
        Release {
            metadata: ReleaseMetadata::official(snapshot_date, digest_string),
            source_manifest_sha256: if digest == 'a' {
                "b".repeat(64)
            } else {
                "3".repeat(64)
            },
            target_set_sha256: target_digest.to_string().repeat(64),
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
                n_number: "N123AB".to_string(),
                matched: true,
            }],
            aircraft: vec![AircraftRecord {
                n_number: "N123AB".to_string(),
                manufacturer_serial_raw: Some("182-01234".to_string()),
                manufacturer_serial_key: Some("18201234".to_string()),
                aircraft_code: "2072738".to_string(),
                engine_code: None,
                year_manufactured: Some(2006),
                source_record_sha256: source_record.to_string().repeat(64),
            }],
            aircraft_references: vec![AircraftReference {
                aircraft_code: "2072738".to_string(),
                manufacturer_name: Some("CESSNA AIRCRAFT CO".to_string()),
                model_name: Some("182T".to_string()),
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

    async fn listing_and_faa(db: &AppDb) -> (i64, AircraftGrounding) {
        let user = db.current_user(None).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let make_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES ('Cessna', 'cessna') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let model_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name) VALUES (?, '182', '182') RETURNING id",
        )
        .bind(make_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let _raw_variant_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name) VALUES (?, '182T', '182t') RETURNING id",
        )
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let pending_variant_id = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT aircraft_model_variant_id
            FROM aircraft_sale_listing_pending_compatibility_placeholder
            WHERE singleton_id = 1
            "#,
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
            ) VALUES (?, ?, 'https://example.test/listing', 2006, 250000, 1000,
                      'N123AB', '182-01234', 'incomplete')
            RETURNING id
            "#,
        )
        .bind(pending_variant_id)
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_listing_identity_input_observations (
              aircraft_sale_listing_id, source_url,
              observed_make, observed_family, observed_designation,
              model_year, serial_number, registration_number,
              input_json, observation_sha256
            ) VALUES (
              ?, 'https://example.test/listing',
              'Cessna', '182', '182T',
              2006, '182-01234', 'N123AB',
              '{"observation_kind":"test_literal_listing_input"}', ?
            )
            "#,
        )
        .bind(listing_id)
        .bind(format!("{listing_id:064x}"))
        .execute(pool)
        .await
        .unwrap();
        let release = faa_release("2026-07-22", 'a', 'c');
        store_release(db, &release).await.unwrap();
        let grounding = require_listing_faa_admission(db, listing_id).await.unwrap();
        (listing_id, grounding)
    }

    async fn duplicate_listing(db: &AppDb, listing_id: i64, suffix: &str) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours,
              registration_number, serial_number, ingestion_state
            )
            SELECT aircraft_model_variant_id, created_by_user_id,
              'https://example.test/listing-' || ?, model_year,
              asking_price_usd, airframe_hours, registration_number,
              serial_number, 'incomplete'
            FROM aircraft_sale_listings
            WHERE id = ?
            RETURNING id
            "#,
        )
        .bind(suffix)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn canonical_catalog_and_relationship_counts(db: &AppDb) -> Vec<i64> {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let mut counts = Vec::new();
        for table in [
            "aircraft_makes",
            "aircraft_make_aliases",
            "aircraft_model_families",
            "aircraft_family_aliases",
            "aircraft_designations",
            "aircraft_designation_aliases",
            "aircraft_designation_identifiers",
            "aircraft_generations",
            "aircraft_generation_designations",
            "aircraft_factory_packages",
            "aircraft_package_applicability",
            "aircraft_tcds_make_lineage_bindings",
            "aircraft_designation_faa_bindings",
            "aircraft_identity_resolution_cases",
            "aircraft_identity_decisions",
            "aircraft_identity_decision_claims",
            "aircraft_manufacturers",
            "aircraft_models",
            "aircraft_model_variants",
            "aircraft_valuation_compatibility_projections",
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

    async fn approved_reuse_write_counts(db: &AppDb) -> Vec<i64> {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let mut counts = canonical_catalog_and_relationship_counts(db).await;
        for table in [
            "curation_evidence_claims",
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

    #[test]
    fn projection_transition_kinds_are_backend_neutral() {
        assert_eq!(
            ProjectionTransitionKind::for_new_assignment(None),
            ProjectionTransitionKind::Initial
        );
        assert_eq!(
            ProjectionTransitionKind::for_new_assignment(Some(41)),
            ProjectionTransitionKind::Successor
        );
        assert_eq!(ProjectionTransitionKind::Initial.as_str(), "initial");
        assert_eq!(
            ProjectionTransitionKind::CurrentRepair.as_str(),
            "current_repair"
        );
        assert_eq!(ProjectionTransitionKind::Successor.as_str(), "successor");
    }

    #[test]
    fn pointer_timestamps_sort_after_schema_defaults_and_are_fixed_width() {
        let first = pointer_timestamp();
        let second = pointer_timestamp();
        assert!(first.starts_with('z'));
        assert_eq!(first.len(), second.len());
        assert!(first.as_str() >= "9999-12-31 23:59:59");
    }

    #[tokio::test]
    async fn empty_catalog_stays_pending_then_test_seed_can_publish() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (listing_id, grounding) = listing_and_faa(&db).await;

        let pending =
            ensure_listing_identity_assignment_from_approved_catalog(&db, listing_id, &grounding)
                .await
                .unwrap();
        assert!(matches!(
            pending,
            EnsureIdentityAssignmentOutcome::PendingCuration {
                candidate_count: 0,
                ..
            }
        ));

        let assignment = seed_test_curated_identity_assignment(&db, listing_id, &grounding)
            .await
            .unwrap();
        assert_eq!(assignment.official_designation, "182T");
        assert_eq!(assignment.faa_aircraft_code, "2072738");
        require_listing_identity_assignment(&db, listing_id, &grounding)
            .await
            .unwrap();

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let projection: (String, String, i64, i64) = sqlx::query_as(
            r#"
            SELECT
              variant.name,
              variant.normalized_name,
              (SELECT COUNT(*) FROM aircraft_valuation_projection_transitions),
              (SELECT COUNT(*)
               FROM aircraft_model_variants raw_variant
               WHERE raw_variant.normalized_name = '182t')
            FROM aircraft_sale_listings listing
            JOIN aircraft_model_variants variant
              ON variant.id = listing.aircraft_model_variant_id
            JOIN aircraft_sale_listing_exact_compatibility_projections exact
              ON exact.listing_id = listing.id
            WHERE listing.id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(projection.0, "182T");
        assert_eq!(
            projection.1,
            format!(
                "__aircost_projection_identity_{}_0_0__",
                assignment.aircraft_designation_id
            )
        );
        assert_eq!(projection.2, 0, "transition command must self-delete");
        assert_eq!(
            projection.3, 1,
            "same-name or raw legacy rows are retained but never adopted"
        );
        sqlx::query(
            "UPDATE aircraft_sale_listings SET ingestion_state = 'ready', ingestion_completed_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .expect("FAA-backed curated assignment should satisfy ready trigger");
        let state = sqlx::query_scalar::<_, String>(
            "SELECT ingestion_state FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(state, "ready");

        store_release(&db, &faa_release("2026-07-22", 'a', '8'))
            .await
            .expect("same-release target expansion should store");
        let same_release_state = sqlx::query_scalar::<_, String>(
            "SELECT ingestion_state FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(same_release_state, "ready");
        require_listing_admission(&db, listing_id)
            .await
            .expect("another target projection of the same FAA release remains current");

        store_release(&db, &faa_release("2026-07-23", '9', 'c'))
            .await
            .expect("newer FAA projection should store atomically");
        let rollover = sqlx::query_as::<_, (String, Option<String>, bool)>(
            r#"
            SELECT ingestion_state, ingestion_error, is_verified
            FROM aircraft_sale_listings WHERE id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(rollover.0, "quarantined");
        assert!(rollover
            .1
            .as_deref()
            .is_some_and(|message| message.contains("FAA snapshot rollover")));
        assert!(!rollover.2);
        let stale = require_listing_admission(&db, listing_id)
            .await
            .expect_err("old assignment cannot admit against the new current snapshot");
        assert_eq!(
            stale.block_reason(),
            Some(&crate::aircraft::faa::BlockReason::CanonicalIdentityAssignmentMismatch)
        );

        let new_grounding = require_listing_faa_admission(&db, listing_id)
            .await
            .unwrap();
        let successor = seed_test_curated_identity_assignment(&db, listing_id, &new_grounding)
            .await
            .expect("explicitly re-grounded fixture should append a successor");
        assert_eq!(
            successor.supersedes_assignment_id,
            Some(assignment.assignment_id)
        );
        sqlx::query(
            "UPDATE aircraft_sale_listings SET ingestion_state = 'ready', ingestion_error = NULL, ingestion_completed_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .expect("successor assignment should restore readiness");

        let pointer_delete = sqlx::query(
            "DELETE FROM aircraft_sale_listing_current_identity_assignments WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .expect_err("current pointer cannot be removed from a live listing");
        assert!(pointer_delete
            .to_string()
            .contains("only with its parent listing"));
    }

    #[tokio::test]
    async fn approved_catalog_reuse_assigns_without_mutating_catalog_relationships() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (representative_id, representative_grounding) = listing_and_faa(&db).await;
        seed_test_curated_identity_assignment(&db, representative_id, &representative_grounding)
            .await
            .unwrap();
        let listing_id = duplicate_listing(&db, representative_id, "approved-reuse").await;
        let grounding = require_listing_faa_admission(&db, listing_id)
            .await
            .unwrap();
        let before = canonical_catalog_and_relationship_counts(&db).await;

        let outcome =
            ensure_listing_identity_assignment_from_approved_catalog(&db, listing_id, &grounding)
                .await
                .unwrap();

        assert!(matches!(
            outcome,
            EnsureIdentityAssignmentOutcome::Assigned { .. }
        ));
        assert_eq!(
            canonical_catalog_and_relationship_counts(&db).await,
            before,
            "approved local reuse may create listing state but no catalog or relationship row"
        );
    }

    #[tokio::test]
    async fn approved_reuse_compares_every_required_faa_projection_identity_field() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (_, expected) = listing_and_faa(&db).await;

        let mut changed = expected.clone();
        changed.snapshot.id += 1;
        assert_eq!(
            exact_faa_projection_mismatches(&expected, &changed),
            vec!["snapshot"]
        );

        changed = expected.clone();
        changed.n_number = "N999ZZ".to_string();
        assert_eq!(
            exact_faa_projection_mismatches(&expected, &changed),
            vec!["N-number"]
        );

        changed = expected.clone();
        changed.source_record_sha256 = "9".repeat(64);
        assert_eq!(
            exact_faa_projection_mismatches(&expected, &changed),
            vec!["source record"]
        );

        changed = expected.clone();
        changed.aircraft_code = "9999999".to_string();
        assert_eq!(
            exact_faa_projection_mismatches(&expected, &changed),
            vec!["aircraft code"]
        );

        changed = expected.clone();
        changed.manufacturer_serial_raw = Some("different".to_string());
        assert_eq!(
            exact_faa_projection_mismatches(&expected, &changed),
            vec!["manufacturer serial"]
        );

        changed = expected.clone();
        changed.manufacturer_serial_key = Some("DIFFERENT".to_string());
        assert_eq!(
            exact_faa_projection_mismatches(&expected, &changed),
            vec!["manufacturer serial"]
        );

        changed = expected.clone();
        changed
            .aircraft
            .as_mut()
            .expect("fixture FAA aircraft reference")
            .model_name = Some("T182T".to_string());
        assert_eq!(
            exact_faa_projection_mismatches(&expected, &changed),
            vec!["aircraft model"]
        );
    }

    #[tokio::test]
    async fn approved_catalog_reuse_rejects_a_stale_faa_projection_with_zero_writes() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (representative_id, stale_grounding) = listing_and_faa(&db).await;
        seed_test_curated_identity_assignment(&db, representative_id, &stale_grounding)
            .await
            .unwrap();
        let listing_id = duplicate_listing(&db, representative_id, "stale-faa").await;

        store_release(&db, &faa_release("2026-07-23", '9', 'c'))
            .await
            .expect("newer FAA projection should store");
        let current_grounding = require_listing_faa_admission(&db, listing_id)
            .await
            .unwrap();
        assert_ne!(current_grounding.snapshot, stale_grounding.snapshot);
        assert!(matches!(
            resolve_faa_backed_compatibility_identity(
                &db,
                Some(listing_id),
                2006,
                &current_grounding,
            )
            .await
            .unwrap(),
            ResolveCompatibilityIdentityOutcome::PendingCuration { reason, .. }
                if reason.contains("no existing approved designation binding")
        ));
        let missing_binding_before = approved_reuse_write_counts(&db).await;
        assert!(matches!(
            ensure_listing_identity_assignment_from_approved_catalog(
                &db,
                listing_id,
                &current_grounding,
            )
            .await
            .unwrap(),
            EnsureIdentityAssignmentOutcome::PendingCuration { reason, .. }
                if reason.contains("no existing approved designation binding")
        ));
        assert_eq!(
            approved_reuse_write_counts(&db).await,
            missing_binding_before,
            "local reuse must not install the missing current-release FAA binding"
        );
        let before = approved_reuse_write_counts(&db).await;

        let error = ensure_listing_identity_assignment_from_approved_catalog(
            &db,
            listing_id,
            &stale_grounding,
        )
        .await
        .expect_err("a report grounded to the prior FAA release must fail closed");

        assert!(error.to_string().contains("changed after curation"));
        assert_eq!(
            approved_reuse_write_counts(&db).await,
            before,
            "stale FAA detection must happen before evidence, assignment, projection, or catalog writes"
        );
    }

    #[tokio::test]
    async fn admission_rejects_null_generation_after_catalog_relation_is_added() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (listing_id, grounding) = listing_and_faa(&db).await;
        let assignment = seed_test_curated_identity_assignment(&db, listing_id, &grounding)
            .await
            .unwrap();
        assert_eq!(assignment.aircraft_generation_id, None);
        require_listing_admission(&db, listing_id)
            .await
            .expect("the NULL generation is valid while no generation relation exists");

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let observation_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM aircraft_identity_observations WHERE aircraft_sale_listing_id = ? ORDER BY id LIMIT 1",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let generation_decision_id = seed_test_approval_decision_sqlite(
            &mut transaction,
            observation_id,
            listing_id,
            grounding.snapshot.id,
            "generation",
            "generation",
            assignment.identity_evidence_claim_id,
            "test generation added after the NULL assignment",
        )
        .await
        .unwrap();
        let generation_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_generations (
              aircraft_model_family_id, name, normalized_name,
              ordinal, approval_decision_id
            ) VALUES (?, 'Test generation', 'test generation', 1, ?)
            RETURNING id
            "#,
        )
        .bind(assignment.aircraft_model_family_id)
        .bind(generation_decision_id)
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        let relation_decision_id = seed_test_approval_decision_sqlite(
            &mut transaction,
            observation_id,
            listing_id,
            grounding.snapshot.id,
            "generation",
            "generation_designation",
            assignment.identity_evidence_claim_id,
            "test generation/designation relation added after the NULL assignment",
        )
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_generation_designations (
              aircraft_generation_id, aircraft_designation_id,
              approval_decision_id
            ) VALUES (?, ?, ?)
            "#,
        )
        .bind(generation_id)
        .bind(assignment.aircraft_designation_id)
        .bind(relation_decision_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let error = require_listing_admission(&db, listing_id)
            .await
            .expect_err("a newly available generation must invalidate the NULL assignment");
        assert_eq!(
            error.block_reason(),
            Some(&crate::aircraft::faa::BlockReason::CanonicalIdentityAssignmentMismatch)
        );
    }

    #[tokio::test]
    async fn admission_rejects_null_package_after_applicable_trim_tier_is_added() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (listing_id, grounding) = listing_and_faa(&db).await;
        let assignment = seed_test_curated_identity_assignment(&db, listing_id, &grounding)
            .await
            .unwrap();
        assert_eq!(assignment.aircraft_factory_package_id, None);
        require_listing_admission(&db, listing_id)
            .await
            .expect("the NULL package is valid while no applicable trim tier exists");

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let observation_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM aircraft_identity_observations WHERE aircraft_sale_listing_id = ? ORDER BY id LIMIT 1",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let package_decision_id = seed_test_approval_decision_sqlite(
            &mut transaction,
            observation_id,
            listing_id,
            grounding.snapshot.id,
            "package",
            "package",
            assignment.identity_evidence_claim_id,
            "test trim tier added after the NULL assignment",
        )
        .await
        .unwrap();
        let package_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_factory_packages (
              aircraft_model_family_id, name, normalized_name, package_kind,
              exclusivity_group, approval_decision_id
            ) VALUES (?, 'Test trim tier', 'test trim tier', 'trim_tier',
                      'test-tier', ?)
            RETURNING id
            "#,
        )
        .bind(assignment.aircraft_model_family_id)
        .bind(package_decision_id)
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        let applicability_decision_id = seed_test_approval_decision_sqlite(
            &mut transaction,
            observation_id,
            listing_id,
            grounding.snapshot.id,
            "package",
            "package_applicability",
            assignment.identity_evidence_claim_id,
            "test model-year applicability added after the NULL assignment",
        )
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_package_applicability (
              aircraft_factory_package_id, aircraft_designation_id,
              valid_from_model_year, valid_to_model_year,
              approval_decision_id
            ) VALUES (?, ?, 2006, 2006, ?)
            "#,
        )
        .bind(package_id)
        .bind(assignment.aircraft_designation_id)
        .bind(applicability_decision_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let error = require_listing_admission(&db, listing_id)
            .await
            .expect_err("a newly applicable trim tier must invalidate the NULL assignment");
        assert_eq!(
            error.block_reason(),
            Some(&crate::aircraft::faa::BlockReason::CanonicalIdentityAssignmentMismatch)
        );
    }

    #[tokio::test]
    async fn model_year_change_invalidates_selected_package() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (listing_id, grounding) = listing_and_faa(&db).await;
        let root = seed_test_curated_identity_assignment(&db, listing_id, &grounding)
            .await
            .unwrap();
        let current = load_current_assignment(&db, listing_id)
            .await
            .unwrap()
            .unwrap();

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let observation_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM aircraft_identity_observations WHERE aircraft_sale_listing_id = ? ORDER BY id LIMIT 1",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let package_decision_id = seed_test_approval_decision_sqlite(
            &mut transaction,
            observation_id,
            listing_id,
            grounding.snapshot.id,
            "package",
            "package",
            root.identity_evidence_claim_id,
            "test package valid only for the original model year",
        )
        .await
        .unwrap();
        let package_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_factory_packages (
              aircraft_model_family_id, name, normalized_name, package_kind,
              exclusivity_group, approval_decision_id
            ) VALUES (?, 'Test 2006 tier', 'test 2006 tier', 'trim_tier',
                      'test-tier', ?) RETURNING id
            "#,
        )
        .bind(root.aircraft_model_family_id)
        .bind(package_decision_id)
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        let applicability_decision_id = seed_test_approval_decision_sqlite(
            &mut transaction,
            observation_id,
            listing_id,
            grounding.snapshot.id,
            "package",
            "package_applicability",
            root.identity_evidence_claim_id,
            "test package applicability limited to model year 2006",
        )
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_package_applicability (
              aircraft_factory_package_id, aircraft_designation_id,
              valid_from_model_year, valid_to_model_year, approval_decision_id
            ) VALUES (?, ?, 2006, 2006, ?)
            "#,
        )
        .bind(package_id)
        .bind(root.aircraft_designation_id)
        .bind(applicability_decision_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let mut package_candidate = PromotionCandidate::from_current(&current);
        package_candidate.aircraft_factory_package_id = Some(package_id);
        let reference = grounding.aircraft.as_ref().unwrap();
        let evidence = FaaIdentityEvidence::new(
            &grounding,
            reference.manufacturer_name.as_deref().unwrap(),
            reference.model_name.as_deref().unwrap(),
        );
        persist_assignment(
            &db,
            listing_id,
            &package_candidate,
            Some(root.assignment_id),
            &grounding,
            &evidence,
        )
        .await
        .unwrap();
        require_listing_identity_assignment(&db, listing_id, &grounding)
            .await
            .expect("the package applies in model year 2006");
        sqlx::query(
            "UPDATE aircraft_sale_listings SET ingestion_state = 'ready', ingestion_error = NULL, ingestion_completed_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
            .bind(listing_id)
            .execute(pool)
            .await
            .unwrap();

        let rejected =
            sqlx::query("UPDATE aircraft_sale_listings SET model_year = 2007 WHERE id = ?")
                .bind(listing_id)
                .execute(pool)
                .await
                .expect_err(
                    "a ready listing cannot retain a package outside its year applicability",
                );
        assert!(rejected
            .to_string()
            .contains("current canonical aircraft assignment"));

        sqlx::query(
            "UPDATE aircraft_sale_listings SET ingestion_state = 'incomplete', model_year = 2007 WHERE id = ?",
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();
        let stale = require_listing_identity_assignment(&db, listing_id, &grounding)
            .await
            .expect_err("runtime admission must also reject the stale year-specific package");
        assert!(stale.to_string().contains("invalid or unresolved"));
        assert!(matches!(
            ensure_listing_identity_assignment_from_approved_catalog(&db, listing_id, &grounding)
                .await
                .unwrap(),
            EnsureIdentityAssignmentOutcome::PendingCuration { .. }
        ));
    }

    #[tokio::test]
    async fn faa_make_alias_is_unambiguous_and_year_market_scoped() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (listing_id, grounding) = listing_and_faa(&db).await;
        let assignment = seed_test_curated_identity_assignment_with_make_alias(
            &db,
            listing_id,
            &grounding,
            Some(TestMakeAlias {
                canonical_make: "Cessna",
                valid_from_model_year: Some(2000),
                valid_to_model_year: Some(2010),
                market_code: "US",
            }),
        )
        .await
        .expect("an exact curated US/year alias should authorize the FAA manufacturer label");
        assert_eq!(assignment.make_name, "Cessna");

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let second_listing_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours,
              registration_number, serial_number, ingestion_state
            )
            SELECT aircraft_model_variant_id, created_by_user_id,
              'https://example.test/listing-alias', 2006, 260000, 900,
              'N123AB', '182-01234', 'incomplete'
            FROM aircraft_sale_listings WHERE id = ?
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let second_grounding = require_listing_faa_admission(&db, second_listing_id)
            .await
            .unwrap();
        assert!(matches!(
            ensure_listing_identity_assignment_from_approved_catalog(
                &db,
                second_listing_id,
                &second_grounding,
            )
            .await
            .unwrap(),
            EnsureIdentityAssignmentOutcome::Assigned { .. }
        ));

        sqlx::query(
            "UPDATE aircraft_sale_listings SET ingestion_state = 'ready', ingestion_error = NULL, ingestion_completed_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE aircraft_sale_listings SET model_year = 2011 WHERE id = ?")
            .bind(listing_id)
            .execute(pool)
            .await
            .expect_err("the ready gate must reject an alias outside its approved year range");

        sqlx::query(
            "UPDATE aircraft_sale_listings SET ingestion_state = 'incomplete', model_year = 2011 WHERE id = ?",
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();
        require_listing_identity_assignment(&db, listing_id, &grounding)
            .await
            .expect_err("runtime admission must reject an out-of-range FAA make alias");
        assert!(matches!(
            ensure_listing_identity_assignment_from_approved_catalog(&db, listing_id, &grounding)
                .await
                .unwrap(),
            EnsureIdentityAssignmentOutcome::PendingCuration { .. }
        ));
    }
}
