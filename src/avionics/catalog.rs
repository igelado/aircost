use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use super::manufacturer::{
    admit_manufacturer_product_scope_postgres, admit_manufacturer_product_scope_sqlite,
    authorize_manufacturer_source_urls, canonical_exact_https_origin,
    require_source_urls_not_revoked, ManufacturerIdentityError, ManufacturerIdentityEvidence,
    ManufacturerProductAdmission, ManufacturerProductAdmissionOutcome,
    ManufacturerSourceOriginAdmission,
};
use super::reuse::{
    current_reuse_attested_product_ids, refresh_grounded_evidence_and_reuse_attestation_postgres,
    refresh_grounded_evidence_and_reuse_attestation_sqlite, refresh_reuse_attestation_postgres,
    refresh_reuse_attestation_sqlite,
};
use super::source::{exact_oem_product_identity_row, OemProductIdentity};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{
    AvionicsApprovedCandidateAdjudicationContext, AvionicsApprovedCatalogCandidate,
    AvionicsCatalogCandidate, AvionicsCatalogCollisionReviewContext, AvionicsProposedIdentity,
    AvionicsUnitResolutionCandidate, AvionicsUnitResolutionContext,
    AvionicsUnitResolutionCorrectionContext, GeminiGroundingSource, GeminiGroundingSupport,
    GeminiListingExtractor, GroundedJsonResponse, AVIONICS_APPROVED_CANDIDATE_ADJUDICATION_LIMIT,
    CURATED_AVIONICS_TYPES,
};
use crate::gemini::curation::workflow::{
    direct_source_product_identity_signal_is_present, SourceEvidenceProof,
};
use crate::gemini::interactions::FetchedSourceDocument;
use crate::normalize::{
    is_generic_avionics_manufacturer_name, is_usable_avionics_label, normalize_avionics_identifier,
    normalize_avionics_manufacturer_name, normalize_avionics_model_name, normalize_name,
};

const CANDIDATE_LIMIT: usize = 16;
const COLLISION_CANDIDATE_LIMIT: usize = 32;
const COLLISION_STRUCTURE_CALL_BUDGET: usize = 2;
const MINIMUM_PREFIX_MODEL_KEY_LENGTH: usize = 4;
const EXACT_CATALOG_PRODUCT_IDENTIFIER_SCOPE: &str = "exact_catalog_product";
const NO_IDENTIFIER_SCOPE: &str = "none";
const NO_REJECTION_BASIS: &str = "none";
const REJECTION_BASES: &[&str] = &[
    "generic_or_class_only",
    "feature_only",
    "not_installed_equipment",
    "demonstrably_nonexistent",
];
const MANUFACTURER_IDENTIFIER_SCOPES: &[&str] = &[
    EXACT_CATALOG_PRODUCT_IDENTIFIER_SCOPE,
    "component_of_catalog_product",
    "approval_or_article_scope",
    "family_or_series",
    "unknown",
    NO_IDENTIFIER_SCOPE,
];
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CollisionCorrectionPlan {
    CorrectOnce,
    BudgetExhausted,
}

fn collision_correction_plan(
    initial_structure_calls: usize,
) -> CatalogResult<CollisionCorrectionPlan> {
    match initial_structure_calls {
        1 => Ok(CollisionCorrectionPlan::CorrectOnce),
        COLLISION_STRUCTURE_CALL_BUDGET => Ok(CollisionCorrectionPlan::BudgetExhausted),
        calls => Err(CatalogError::Validation(format!(
            "Gemini avionics collision review reported {calls} structure calls; expected 1..={COLLISION_STRUCTURE_CALL_BUDGET}"
        ))),
    }
}

fn validate_collision_correction_call_budget(
    initial_structure_calls: usize,
    correction_structure_calls: usize,
) -> CatalogResult<()> {
    if correction_structure_calls != 1
        || initial_structure_calls + correction_structure_calls > COLLISION_STRUCTURE_CALL_BUDGET
    {
        return Err(CatalogError::Validation(format!(
            "Gemini avionics collision review and correction used {initial_structure_calls} + {correction_structure_calls} structure calls; the shared budget is {COLLISION_STRUCTURE_CALL_BUDGET}"
        )));
    }
    Ok(())
}

const CATALOG_SELECT_SQL: &str = r#"
    SELECT
      model.id,
      mfr.name AS manufacturer,
      model.name AS model,
      capability_type.name AS capability_type,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier,
      model.catalog_status,
      effective_manufacturer.avionics_manufacturer_identity_id
    FROM avionics_models model
    JOIN avionics_manufacturers mfr
      ON mfr.id = model.avionics_manufacturer_id
    LEFT JOIN avionics_manufacturer_effective_memberships effective_manufacturer
      ON effective_manufacturer.avionics_manufacturer_id
        = model.avionics_manufacturer_id
    JOIN avionics_model_types model_type
      ON model_type.avionics_model_id = model.id
    JOIN avionics_types capability_type
      ON capability_type.id = model_type.avionics_type_id
    WHERE model.catalog_status IN ('approved', 'unreviewed')
    ORDER BY model.id, capability_type.normalized_name, capability_type.id
"#;
const KNOWN_APPROVED_SELECT_SQL: &str = r#"
    SELECT
      model.id,
      mfr.name AS manufacturer,
      model.name AS model,
      capability_type.name AS capability_type,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier,
      model.catalog_status,
      model.identity_source_url,
      model.identity_source_title,
      model.identity_evidence_text AS identity_evidence,
      approved_identity.avionics_manufacturer_identity_id,
      approved_identity.canonical_product_key,
      approved_identity.canonical_identifier_key
    FROM avionics_models model
    JOIN avionics_approved_product_identities approved_identity
      ON approved_identity.avionics_model_id = model.id
    JOIN avionics_manufacturers mfr
      ON mfr.id = model.avionics_manufacturer_id
    JOIN avionics_manufacturer_effective_memberships effective_manufacturer
      ON effective_manufacturer.avionics_manufacturer_id
        = model.avionics_manufacturer_id
     AND effective_manufacturer.avionics_manufacturer_identity_id
        = approved_identity.avionics_manufacturer_identity_id
    JOIN avionics_model_types model_type
      ON model_type.avionics_model_id = model.id
    JOIN avionics_types capability_type
      ON capability_type.id = model_type.avionics_type_id
    WHERE model.catalog_status = 'approved'
    ORDER BY model.id, capability_type.normalized_name, capability_type.id
"#;

#[derive(Debug)]
pub enum CatalogError {
    Validation(String),
    Database(String),
    Gemini(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Database(message) | Self::Gemini(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<sqlx::Error> for CatalogError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

pub type CatalogResult<T> = Result<T, CatalogError>;

#[derive(Clone, Debug)]
pub struct AvionicsIdentityRequest {
    pub aircraft_manufacturer: String,
    pub aircraft_model: String,
    pub aircraft_variant: String,
    pub model_year: i64,
    pub source_url: String,
    pub listing_context: String,
    pub requires_listing_evidence: bool,
    /// Explicit caller-selected OEM/regulator sources for this exact observed
    /// product. Empty selects normal Search discovery.
    pub authoritative_direct_source_urls: Vec<String>,
    /// Immutable product labels/part numbers that every accepted fresh direct
    /// source must match. These are never parsed from `listing_context`.
    pub authoritative_identity_anchors: Vec<String>,
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub quantity: i64,
}

/// Immutable approved-product identity bound to one guarded OEM fetch.
///
/// This request deliberately contains no listing or aircraft context. Product
/// attestation is global catalog work; listing occurrence is corroborated by a
/// separate source-free local resolver.
#[derive(Clone, Debug)]
pub(crate) struct ApprovedAvionicsProductSourceRequest {
    pub source_url: String,
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub manufacturer_identifier_kind: String,
    pub manufacturer_identifier: String,
}

/// Source-free occurrence claim for one listing and one immutable approved
/// product. It contains only facts the local matcher consumes.
#[derive(Clone, Debug)]
pub(crate) struct ApprovedProductAssociationRequest {
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub listing_evidence_text: String,
}

/// Immutable catalog projection used by source-free listing association checks.
///
/// Loading this once lets a review page evaluate every returned association
/// against one coherent local catalog view without repeating the full catalog
/// and manufacturer-identity queries for each row.
pub(crate) struct ApprovedProductAssociationResolver {
    manufacturer_identity_memberships: Vec<(String, i64)>,
    catalog: Vec<AvionicsCatalogCandidate>,
    approved_candidates: Vec<KnownApprovedAvionicsCandidate>,
}

impl ApprovedProductAssociationResolver {
    pub(crate) async fn load_with_reuse_attested_product_ids(
        db: &AppDb,
        reuse_attested_ids: &HashSet<i64>,
    ) -> CatalogResult<Self> {
        Ok(Self {
            manufacturer_identity_memberships: load_manufacturer_identity_memberships(db).await?,
            catalog: load_catalog_candidates(db).await?,
            approved_candidates: load_known_approved_candidates_for_ids(db, reuse_attested_ids)
                .await?,
        })
    }

    pub(crate) fn resolve(
        &self,
        request: &ApprovedProductAssociationRequest,
    ) -> Option<ApprovedAvionicsIdentity> {
        if request.listing_evidence_text.trim().is_empty()
            || request.manufacturer.trim().is_empty()
            || request.model.trim().is_empty()
            || request.avionics_types.is_empty()
            || request.avionics_types.iter().any(|capability| {
                let capability = capability.trim();
                capability.is_empty() || !CURATED_AVIONICS_TYPES.contains(&capability)
            })
        {
            return None;
        }
        let observed_types = canonicalize_avionics_types(&request.avionics_types);
        let manufacturer_identity_id = resolve_input_manufacturer_identity_from_memberships(
            &request.manufacturer,
            &self.manufacturer_identity_memberships,
        )?;
        known_approved_local_match_core(
            &request.model,
            &request.listing_evidence_text,
            &observed_types,
            manufacturer_identity_id,
            &self.approved_candidates,
            &self.catalog,
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PendingProductAttestationCommitGuard {
    pub owner_user_id: i64,
    pub listing_id: i64,
    pub review_payload_sha256: String,
    pub aspect_id: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct ApprovedProductSourceVerification {
    pub approved: ApprovedAvionicsIdentity,
    pub manufacturer_collision_snapshot_sha256: String,
}

#[derive(Clone, Debug)]
pub(crate) enum ApprovedProductSourceVerificationOutcome {
    Verified(ApprovedProductSourceVerification),
    Unresolved { reason: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct ApprovedAvionicsIdentity {
    pub id: i64,
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub manufacturer_identifier_kind: String,
    pub manufacturer_identifier: String,
    pub evidence_url: String,
    pub evidence_title: String,
    pub evidence: String,
    pub reason: String,
    /// Server-owned URLs for every accepted structured claim that supported
    /// this new grounded decision. Never expose these as client-controlled
    /// review input.
    #[serde(skip)]
    pub(crate) grounded_claim_source_urls: Vec<String>,
}

/// One unique exact-model catalog candidate exposed to a human review.
///
/// This is retrieval metadata only. An approved row still requires an
/// explicit `use_verified_product` decision, while an unreviewed row remains
/// only a promotion target for the independently grounded create path.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AvionicsReviewCatalogCandidate {
    pub id: i64,
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub manufacturer_identifier_kind: String,
    pub manufacturer_identifier: String,
    pub catalog_status: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AvionicsIdentityOutcome {
    Approved(ApprovedAvionicsIdentity),
    Rejected { reason: String },
    Unresolved { reason: String },
}

#[derive(Clone, Debug, FromRow)]
struct CatalogRow {
    id: i64,
    manufacturer: String,
    model: String,
    capability_type: String,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    catalog_status: String,
    avionics_manufacturer_identity_id: Option<i64>,
}

#[derive(Clone, Debug)]
struct ReviewCatalogCandidate {
    candidate: AvionicsCatalogCandidate,
    manufacturer_identity_id: Option<i64>,
}

#[derive(Clone, Debug, FromRow)]
struct AuthoritativeSourceHintRow {
    evidence_source_url: String,
    evidence_source_title: String,
    evidence_text: String,
}

#[derive(Clone, Debug, FromRow)]
struct KnownApprovedRow {
    id: i64,
    manufacturer: String,
    model: String,
    capability_type: String,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    catalog_status: String,
    identity_source_url: Option<String>,
    identity_source_title: Option<String>,
    identity_evidence: Option<String>,
    avionics_manufacturer_identity_id: i64,
    canonical_product_key: String,
    canonical_identifier_key: String,
}

#[derive(Clone, Debug)]
struct KnownApprovedAvionicsCandidate {
    id: i64,
    manufacturer: String,
    model: String,
    avionics_types: Vec<String>,
    manufacturer_identifier_kind: String,
    manufacturer_identifier: String,
    identity_source_url: String,
    identity_source_title: String,
    identity_evidence: String,
    avionics_manufacturer_identity_id: i64,
    canonical_product_key: String,
    canonical_identifier_key: String,
}

#[derive(Clone, Debug)]
struct ApprovedCandidateAdjudicationPlan {
    context: AvionicsApprovedCandidateAdjudicationContext,
    selectable_catalog_ids: HashSet<i64>,
    manufacturer_identity_id: i64,
}

#[derive(Clone, Debug)]
struct AuthoritativeDirectSourcePlan {
    source_urls: Vec<String>,
    identity_anchors: Vec<String>,
    admission: ManufacturerSourceOriginAdmission,
    requirement: DirectSourceRequirement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectSourceRequirement {
    Explicit,
    Opportunistic,
}

#[derive(Clone, Debug)]
enum IdentityGroundingPlan {
    Search,
    Direct(AuthoritativeDirectSourcePlan),
}

#[derive(Clone, Debug)]
struct VerifiedIdentity {
    canonical_manufacturer: String,
    canonical_model: String,
    canonical_types: Vec<String>,
    manufacturer_identifier_kind: String,
    manufacturer_identifier: String,
    manufacturer_identifier_scope: String,
    identity_source_url: String,
    identity_source_title: String,
    identity_evidence: String,
    reason: String,
    grounded_claim_source_urls: Vec<String>,
}

#[derive(Clone, Debug)]
struct CollisionReview {
    catalog_id: i64,
    decision: String,
    candidate_source_url: String,
    candidate_source_title: String,
    candidate_evidence: String,
    reason: String,
}

#[derive(Clone, Debug)]
struct ProposalAttestation {
    confirmed: bool,
    source_url: String,
    source_title: String,
    evidence: String,
    reason: String,
}

macro_rules! query_as_all {
    ($db:expr, $row:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_all(pool).await
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_all(pool).await
            }
        }
    }};
}

/// Resolve one raw listing avionics label against the curated catalog.
///
/// Similarity only determines which existing identities Gemini must compare. It
/// never determines the outcome. A new or legacy-unreviewed identity is written
/// only after a separate Gemini call reviews every shortlisted collision.
pub async fn resolve_avionics_identity(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    request: &AvionicsIdentityRequest,
) -> CatalogResult<AvionicsIdentityOutcome> {
    resolve_avionics_identity_with_write_mode(db, extractor, request, true).await
}

/// Run the same grounded classification and independent collision review
/// without changing the catalog. New identities are returned with id `0`;
/// legacy identities that would be promoted retain their existing id.
pub async fn preview_avionics_identity(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    request: &AvionicsIdentityRequest,
) -> CatalogResult<AvionicsIdentityOutcome> {
    resolve_avionics_identity_with_write_mode(db, extractor, request, false).await
}

/// Persist a current-policy reuse attestation for an already-approved product
/// after a fresh preview completed the full grounded identity and collision
/// review.
///
/// This is a dedicated mutation boundary: preview itself stays read-only. The
/// transaction re-reads and exactly compares the immutable catalog identity
/// and capability set, refreshes only its grounded evidence fields, then binds
/// that exact refreshed row to the active source origin.
pub(crate) async fn attest_grounded_existing_avionics_identity(
    db: &AppDb,
    grounded: &ApprovedAvionicsIdentity,
) -> CatalogResult<bool> {
    attest_grounded_existing_avionics_identity_with_guard(db, grounded, None).await
}

pub(crate) async fn attest_pending_review_product_identity(
    db: &AppDb,
    verification: &ApprovedProductSourceVerification,
    guard: &PendingProductAttestationCommitGuard,
) -> CatalogResult<bool> {
    attest_grounded_existing_avionics_identity_with_guard(
        db,
        &verification.approved,
        Some((
            verification.manufacturer_collision_snapshot_sha256.as_str(),
            guard,
        )),
    )
    .await
}

fn pending_review_payload_contains_attestation_target(
    payload_json: &str,
    aspect_id: &Value,
    product_id: i64,
) -> bool {
    serde_json::from_str::<Value>(payload_json)
        .ok()
        .and_then(|payload| payload.get("aspects").and_then(Value::as_array).cloned())
        .is_some_and(|aspects| {
            aspects.iter().any(|aspect| {
                aspect.get("id") == Some(aspect_id)
                    && aspect
                        .get("reuse_attestation_target_id")
                        .and_then(Value::as_i64)
                        == Some(product_id)
            })
        })
}

async fn attest_grounded_existing_avionics_identity_with_guard(
    db: &AppDb,
    grounded: &ApprovedAvionicsIdentity,
    guarded_review: Option<(&str, &PendingProductAttestationCommitGuard)>,
) -> CatalogResult<bool> {
    if grounded.id <= 0
        || grounded.evidence_url.trim().is_empty()
        || grounded.evidence_title.trim().is_empty()
        || grounded.evidence.trim().is_empty()
    {
        return Err(CatalogError::Validation(
            "existing-product reuse attestation requires a grounded approved catalog identity and complete source evidence".to_string(),
        ));
    }
    let expected_types = canonicalize_avionics_types(&grounded.avionics_types);
    if expected_types.is_empty() || expected_types.len() != grounded.avionics_types.len() {
        return Err(CatalogError::Validation(
            "existing-product reuse attestation requires exact canonical capabilities".to_string(),
        ));
    }
    let lock_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            "LOCK TABLE avionics_models, avionics_model_types, avionics_types, avionics_manufacturers, avionics_approved_product_identities, avionics_product_reuse_attestations, avionics_authoritative_source_origins, avionics_authoritative_source_origin_revocations, aircraft_sale_listings, aircraft_sale_listing_pending_reviews IN SHARE ROW EXCLUSIVE MODE",
        ),
    };
    macro_rules! attest {
        ($pool:expr, $refresh_grounded:path) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&lock_sql).execute(&mut *transaction).await?;
            let select_sql = format!("{CATALOG_SELECT_SQL}");
            let select_sql = db.sql(&select_sql);
            let rows = sqlx::query_as::<_, CatalogRow>(&select_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let review_catalog = review_catalog_candidates_from_rows(rows);
            let catalog = review_catalog
                .iter()
                .map(|candidate| candidate.candidate.clone())
                .collect::<Vec<_>>();
            let current = catalog
                .iter()
                .find(|candidate| candidate.id == grounded.id)
                .ok_or_else(|| {
                    CatalogError::Validation(format!(
                        "approved catalog id {} disappeared before reuse attestation",
                        grounded.id
                    ))
                })?;
            if current.catalog_status != "approved"
                || current.manufacturer != grounded.manufacturer
                || current.model != grounded.model
                || current.avionics_types != expected_types
                || current.manufacturer_identifier_kind
                    != grounded.manufacturer_identifier_kind
                || current.manufacturer_identifier != grounded.manufacturer_identifier
            {
                return Err(CatalogError::Validation(format!(
                    "approved catalog id {} changed after grounded review; retry against the current product",
                    grounded.id
                )));
            }
            if let Some((expected_collision_snapshot, guard)) = guarded_review {
                let current_manufacturer_identity_id = review_catalog
                    .iter()
                    .find(|candidate| candidate.candidate.id == grounded.id)
                    .and_then(|candidate| candidate.manufacturer_identity_id);
                let current_collision_snapshot = manufacturer_collision_snapshot_sha256(
                    &grounded.manufacturer,
                    current_manufacturer_identity_id,
                    &review_catalog,
                );
                if current_collision_snapshot != expected_collision_snapshot {
                    return Err(CatalogError::Validation(format!(
                        "manufacturer-scoped collision catalog changed after source verification for catalog id {}; retry against the current product family",
                        grounded.id
                    )));
                }
                let pending_sql = db.sql(
                    r#"
                    SELECT review.review_payload_json
                    FROM aircraft_sale_listing_pending_reviews review
                    JOIN aircraft_sale_listings listing
                      ON listing.id = review.listing_id
                    WHERE review.listing_id = ?
                      AND listing.created_by_user_id = ?
                      AND review.review_payload_sha256 = ?
                    "#,
                );
                let pending_payload: Option<String> =
                    sqlx::query_scalar(&pending_sql)
                        .bind(guard.listing_id)
                        .bind(guard.owner_user_id)
                        .bind(guard.review_payload_sha256.as_str())
                        .fetch_optional(&mut *transaction)
                        .await?;
                if !pending_payload.as_deref().is_some_and(|payload| {
                    pending_review_payload_contains_attestation_target(
                        payload,
                        &guard.aspect_id,
                        grounded.id,
                    )
                }) {
                    return Err(CatalogError::Validation(format!(
                        "the reviewer no longer owns the hash-bound pending association for catalog id {}; reload the product review queue",
                        grounded.id
                    )));
                }
            }
            let attested = $refresh_grounded(
                db,
                &mut transaction,
                grounded.id,
                grounded.evidence_url.as_str(),
                grounded.evidence_title.as_str(),
                grounded.evidence.as_str(),
            )
            .await?;
            if !attested {
                // Dropping the transaction rolls the evidence refresh back as
                // well; catalog evidence and its positive attestation are one
                // atomic conclusion.
                return Ok(false);
            }
            transaction.commit().await?;
            true
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(attest!(
            pool,
            refresh_grounded_evidence_and_reuse_attestation_sqlite
        )),
        DatabaseBackend::Postgres(pool) => Ok(attest!(
            pool,
            refresh_grounded_evidence_and_reuse_attestation_postgres
        )),
    }
}

/// Re-attest one hash-bound graph-approved product from a freshly fetched OEM
/// document without asking Gemini to restate an identity the catalog already
/// knows.
///
/// This path is deliberately narrower than ordinary identity resolution. It
/// cannot select a different product, infer capabilities, create catalog
/// state, or resolve listing occurrence. The caller must separately
/// corroborate the listing association after the returned identity is written
/// through `attest_grounded_existing_avionics_identity`.
pub(crate) async fn verify_approved_avionics_product_source_without_gemini(
    db: &AppDb,
    request: &ApprovedAvionicsProductSourceRequest,
    target_id: i64,
    source_title: &str,
    fetched: &FetchedSourceDocument,
) -> CatalogResult<ApprovedProductSourceVerificationOutcome> {
    canonical_exact_https_origin(&request.source_url).map_err(|error| {
        CatalogError::Validation(format!(
            "invalid authoritative avionics direct-source URL: {error}"
        ))
    })?;
    let admission = authorize_manufacturer_source_urls(
        db,
        &request.manufacturer,
        std::slice::from_ref(&request.source_url),
    )
    .await
    .map_err(|error| match error {
        ManufacturerIdentityError::Validation(message)
        | ManufacturerIdentityError::Conflict(message) => CatalogError::Validation(format!(
            "authoritative avionics direct-source admission failed: {message}"
        )),
        ManufacturerIdentityError::Database(message) => CatalogError::Database(message),
    })?;
    let requested_source_url = request.source_url.as_str();
    let final_url = fetched.final_url.as_str();
    if let Err(error) = admission.require_authorized_final_url(requested_source_url, final_url) {
        return Ok(ApprovedProductSourceVerificationOutcome::Unresolved {
            reason: format!(
                "the freshly fetched publisher document did not retain its admitted exact origin: {error}"
            ),
        });
    }

    let graph_candidates = load_graph_approved_candidates(db).await?;
    let review_catalog = load_review_catalog_candidates(db).await?;
    match deterministic_graph_approved_identity_from_source(
        request,
        target_id,
        source_title,
        fetched,
        &admission,
        &graph_candidates,
        &review_catalog,
    ) {
        Ok(approved) => Ok(ApprovedProductSourceVerificationOutcome::Verified(
            ApprovedProductSourceVerification {
                manufacturer_collision_snapshot_sha256: manufacturer_collision_snapshot_sha256(
                    &request.manufacturer,
                    Some(admission.effective_manufacturer_identity_id),
                    &review_catalog,
                ),
                approved,
            },
        )),
        Err(reason) => Ok(ApprovedProductSourceVerificationOutcome::Unresolved { reason }),
    }
}

#[allow(clippy::too_many_arguments)]
fn deterministic_graph_approved_identity_from_source(
    request: &ApprovedAvionicsProductSourceRequest,
    target_id: i64,
    source_title: &str,
    fetched: &FetchedSourceDocument,
    admission: &ManufacturerSourceOriginAdmission,
    graph_candidates: &[KnownApprovedAvionicsCandidate],
    review_catalog: &[ReviewCatalogCandidate],
) -> Result<ApprovedAvionicsIdentity, String> {
    if target_id <= 0 {
        return Err("the hash-bound approved catalog id is invalid".to_string());
    }
    if source_title.trim().is_empty() {
        return Err("the publisher source title is missing".to_string());
    }
    if fetched.publisher_text.trim().is_empty()
        || fetched.content_sha256.len() != 64
        || !fetched
            .content_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(
            "the guarded publisher fetch did not produce a complete source document".to_string(),
        );
    }

    let Some(target) = graph_candidates
        .iter()
        .find(|candidate| candidate.id == target_id)
    else {
        return Err(format!(
            "catalog id {target_id} is not a current graph-approved product identity"
        ));
    };
    if target.avionics_manufacturer_identity_id != admission.effective_manufacturer_identity_id {
        return Err(format!(
            "catalog id {target_id} is not owned by the manufacturer identity admitted for this publisher origin"
        ));
    }

    let requested_types = canonicalize_avionics_types(&request.avionics_types);
    if request.manufacturer != target.manufacturer
        || request.model != target.model
        || requested_types != target.avionics_types
        || requested_types.len() != request.avionics_types.len()
        || request.manufacturer_identifier_kind != target.manufacturer_identifier_kind
        || request.manufacturer_identifier != target.manufacturer_identifier
    {
        return Err(format!(
            "catalog id {target_id} changed manufacturer, model, capabilities, or stable identifier after the hash-bound review was staged"
        ));
    }
    if !matches!(
        target.manufacturer_identifier_kind.as_str(),
        "manufacturer_part_number" | "manufacturer_model_number" | "sku"
    ) || target.manufacturer_identifier.trim().is_empty()
    {
        return Err(format!(
            "catalog id {target_id} has no stable manufacturer identifier eligible for deterministic proof"
        ));
    }
    if stable_oem_identifier_has_placeholder(
        &target.manufacturer_identifier_kind,
        &target.manufacturer_identifier,
    ) {
        return Err(format!(
            "catalog id {target_id} uses a wildcard or placeholder manufacturer part number/SKU and cannot receive exact deterministic proof"
        ));
    }

    let product_key = normalize_avionics_identifier(&target.model);
    let identifier_key = normalize_avionics_identifier(&target.manufacturer_identifier);
    if product_key.is_empty()
        || identifier_key.is_empty()
        || product_key != target.canonical_product_key
        || identifier_key != target.canonical_identifier_key
    {
        return Err(format!(
            "catalog id {target_id} has inconsistent graph identity keys"
        ));
    }
    let manufacturer_catalog = manufacturer_scoped_catalog_candidates(
        &target.manufacturer,
        Some(target.avionics_manufacturer_identity_id),
        review_catalog,
    );
    let Some(current) = manufacturer_catalog
        .iter()
        .find(|candidate| candidate.id == target_id)
    else {
        return Err(format!(
            "catalog id {target_id} disappeared from its effective manufacturer scope"
        ));
    };
    if current.catalog_status != "approved"
        || current.manufacturer != target.manufacturer
        || current.model != target.model
        || current.avionics_types != target.avionics_types
        || current.manufacturer_identifier_kind != target.manufacturer_identifier_kind
        || current.manufacturer_identifier != target.manufacturer_identifier
    {
        return Err(format!(
            "catalog id {target_id} no longer matches its graph-approved identity and capability projection"
        ));
    }

    for other in manufacturer_catalog
        .iter()
        .filter(|candidate| candidate.id != target_id)
    {
        let other_product_key = normalize_avionics_identifier(&other.model);
        if other_product_key == product_key {
            return Err(format!(
                "catalog id {target_id} has an exact-model duplicate at catalog id {}",
                other.id
            ));
        }
        let other_identifier_key = normalize_avionics_identifier(&other.manufacturer_identifier);
        if !other_identifier_key.is_empty() && other_identifier_key == identifier_key {
            return Err(format!(
                "catalog id {target_id} has an exact manufacturer-identifier duplicate at catalog id {}",
                other.id
            ));
        }
    }

    let target_identity = OemProductIdentity {
        catalog_id: target.id,
        model: &target.model,
        manufacturer_identifier: &target.manufacturer_identifier,
    };
    let scoped_identities = manufacturer_catalog
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.manufacturer_identifier_kind.as_str(),
                "manufacturer_part_number" | "manufacturer_model_number" | "sku"
            ) && !candidate.manufacturer_identifier.trim().is_empty()
                && !stable_oem_identifier_has_placeholder(
                    &candidate.manufacturer_identifier_kind,
                    &candidate.manufacturer_identifier,
                )
        })
        .map(|candidate| OemProductIdentity {
            catalog_id: candidate.id,
            model: &candidate.model,
            manufacturer_identifier: &candidate.manufacturer_identifier,
        })
        .collect::<Vec<_>>();
    let evidence = exact_oem_product_identity_row(
        &fetched.source_text_rows,
        fetched.source_text_rows_complete,
        target_identity,
        &scoped_identities,
    )
    .map_err(|reason| format!("{reason} for catalog id {target_id}"))?;
    if manufacturer_catalog
        .iter()
        .filter(|candidate| candidate.id != target_id)
        .map(|candidate| normalize_avionics_identifier(&candidate.model))
        .any(|other_product_key| {
            !other_product_key.is_empty()
                && other_product_key != product_key
                && other_product_key.starts_with(&product_key)
                && exact_compact_identity_is_present(&evidence, &other_product_key)
        })
    {
        return Err(format!(
            "the exact OEM source row names a longer manufacturer-scoped prefix-neighbor instead of catalog id {target_id}"
        ));
    }

    let has_longer_product_neighbor = manufacturer_catalog
        .iter()
        .filter(|candidate| candidate.id != target_id)
        .map(|candidate| normalize_avionics_identifier(&candidate.model))
        .any(|other_product_key| {
            !other_product_key.is_empty()
                && other_product_key != product_key
                && other_product_key.starts_with(&product_key)
        });
    if has_longer_product_neighbor
        && !has_distinct_exact_oem_part_or_sku(
            &target.manufacturer_identifier_kind,
            &target.manufacturer_identifier,
            &product_key,
        )
    {
        return Err(format!(
            "catalog id {target_id} is a prefix of another manufacturer product and lacks a distinct OEM part number or SKU proof"
        ));
    }

    Ok(ApprovedAvionicsIdentity {
        id: target.id,
        manufacturer: target.manufacturer.clone(),
        model: target.model.clone(),
        avionics_types: target.avionics_types.clone(),
        manufacturer_identifier_kind: target.manufacturer_identifier_kind.clone(),
        manufacturer_identifier: target.manufacturer_identifier.clone(),
        evidence_url: fetched.final_url.to_string(),
        evidence_title: source_title.trim().to_string(),
        evidence,
        reason: "A fresh guarded fetch from the currently admitted OEM origin carries the complete graph-approved model and stable identifier in one bounded visible structural row; the manufacturer-scoped catalog has no exact identity duplicate.".to_string(),
        grounded_claim_source_urls: vec![fetched.final_url.to_string()],
    })
}

/// Resolve an observed listing unit against the graph-approved catalog without
/// invoking Gemini.
///
/// Product existence and listing occurrence are separate claims. A current
/// reuse attestation establishes the former. The latter may be established by
/// either an exact retained OEM part number/SKU or one complete exact
/// manufacturer-scoped catalog model label. Normalization handles typography
/// only; prefix/suffix similarity remains candidate retrieval and never
/// authorizes a product.
pub async fn resolve_verified_local_avionics_identity(
    db: &AppDb,
    request: &AvionicsIdentityRequest,
) -> CatalogResult<Option<ApprovedAvionicsIdentity>> {
    if !request.requires_listing_evidence
        || request.listing_context.trim().is_empty()
        || request.manufacturer.trim().is_empty()
        || request.model.trim().is_empty()
    {
        return Ok(None);
    }

    if request.avionics_types.is_empty()
        || request.avionics_types.iter().any(|capability| {
            let capability = capability.trim();
            capability.is_empty() || !CURATED_AVIONICS_TYPES.contains(&capability)
        })
    {
        return Ok(None);
    }
    let observed_types = canonicalize_avionics_types(&request.avionics_types);

    let Some(manufacturer_identity_id) =
        resolve_input_manufacturer_identity(db, &request.manufacturer).await?
    else {
        return Ok(None);
    };
    let catalog = load_catalog_candidates(db).await?;
    let approved_candidates = load_known_approved_candidates(db).await?;
    Ok(known_approved_local_match_core(
        &request.model,
        &request.listing_context,
        &observed_types,
        manufacturer_identity_id,
        &approved_candidates,
        &catalog,
    ))
}

async fn explicit_authoritative_direct_source_plan(
    db: &AppDb,
    request: &AvionicsIdentityRequest,
) -> CatalogResult<Option<AuthoritativeDirectSourcePlan>> {
    match (
        request.authoritative_direct_source_urls.is_empty(),
        request.authoritative_identity_anchors.is_empty(),
    ) {
        (true, true) => return Ok(None),
        (true, false) | (false, true) => {
            return Err(CatalogError::Validation(
                "authoritative avionics direct-source URLs and identity anchors must be supplied together"
                    .to_string(),
            ));
        }
        (false, false) => {}
    }

    // A caller that explicitly selected a direct source must either use that
    // exact admitted manufacturer origin or fail before Gemini. Silently
    // converting a rejected direct-source request into ordinary Search would
    // both weaken provenance and open an unexpected paid-call path.
    for source_url in &request.authoritative_direct_source_urls {
        canonical_exact_https_origin(source_url).map_err(|error| {
            CatalogError::Validation(format!(
                "invalid authoritative avionics direct-source URL: {error}"
            ))
        })?;
    }

    match authorize_manufacturer_source_urls(
        db,
        &request.manufacturer,
        &request.authoritative_direct_source_urls,
    )
    .await
    {
        Ok(admission) => Ok(Some(AuthoritativeDirectSourcePlan {
            source_urls: request.authoritative_direct_source_urls.clone(),
            identity_anchors: request.authoritative_identity_anchors.clone(),
            admission,
            requirement: DirectSourceRequirement::Explicit,
        })),
        Err(ManufacturerIdentityError::Validation(message)) => {
            Err(CatalogError::Validation(format!(
                "authoritative avionics direct-source admission failed: {message}"
            )))
        }
        Err(ManufacturerIdentityError::Conflict(message)) => {
            Err(CatalogError::Validation(format!(
                "authoritative avionics direct-source admission conflicted with the curated manufacturer authority: {message}"
            )))
        }
        Err(ManufacturerIdentityError::Database(message)) => Err(CatalogError::Database(message)),
    }
}

async fn opportunistic_authoritative_direct_source_plan(
    db: &AppDb,
    request: &AvionicsIdentityRequest,
    input_types: &[String],
    manufacturer_identity_id: Option<i64>,
    review_catalog: &[ReviewCatalogCandidate],
) -> CatalogResult<Option<AuthoritativeDirectSourcePlan>> {
    let Some(manufacturer_identity_id) = manufacturer_identity_id else {
        return Ok(None);
    };
    let source_sql = db.sql(
        r#"
        SELECT
          source_origin.evidence_source_url,
          source_origin.evidence_source_title,
          source_origin.evidence_text
        FROM avionics_active_authoritative_source_origins source_origin
        JOIN avionics_manufacturer_effective_identities effective
          ON effective.identity_id =
             source_origin.avionics_manufacturer_identity_id
        WHERE source_origin.authority_kind = 'manufacturer_primary'
          AND effective.avionics_manufacturer_identity_id = ?
        ORDER BY source_origin.id
        "#,
    );
    let source_hints = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, AuthoritativeSourceHintRow>(&source_sql)
                .bind(manufacturer_identity_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, AuthoritativeSourceHintRow>(&source_sql)
                .bind(manufacturer_identity_id)
                .fetch_all(pool)
                .await?
        }
    };
    let source_urls = select_opportunistic_authoritative_source_urls(
        request,
        input_types,
        manufacturer_identity_id,
        review_catalog,
        &source_hints,
    );
    if source_urls.is_empty() {
        return Ok(None);
    }

    let admission =
        match authorize_manufacturer_source_urls(db, &request.manufacturer, &source_urls).await {
            Ok(admission) => admission,
            Err(ManufacturerIdentityError::Validation(_))
            | Err(ManufacturerIdentityError::Conflict(_)) => return Ok(None),
            Err(ManufacturerIdentityError::Database(message)) => {
                return Err(CatalogError::Database(message));
            }
        };
    Ok(Some(AuthoritativeDirectSourcePlan {
        source_urls,
        identity_anchors: vec![request.manufacturer.clone(), request.model.clone()],
        admission,
        requirement: DirectSourceRequirement::Opportunistic,
    }))
}

fn select_opportunistic_authoritative_source_urls(
    request: &AvionicsIdentityRequest,
    input_types: &[String],
    manufacturer_identity_id: i64,
    review_catalog: &[ReviewCatalogCandidate],
    source_hints: &[AuthoritativeSourceHintRow],
) -> Vec<String> {
    let Some(exact_candidate) = select_unique_exact_review_candidate(
        &request.manufacturer,
        &request.model,
        input_types,
        Some(manufacturer_identity_id),
        review_catalog,
    ) else {
        return Vec::new();
    };
    if exact_candidate.catalog_status == "rejected" {
        return Vec::new();
    }
    let observed_model_key = normalize_avionics_identifier(&request.model);
    let observed_manufacturer_key = normalize_avionics_identifier(&request.manufacturer);
    if observed_model_key.is_empty() || observed_manufacturer_key.is_empty() {
        return Vec::new();
    }
    let manufacturer_catalog = manufacturer_scoped_catalog_candidates(
        &request.manufacturer,
        Some(manufacturer_identity_id),
        review_catalog,
    );
    let longer_product_keys = manufacturer_catalog
        .iter()
        .filter_map(|candidate| {
            let candidate_key = normalize_avionics_identifier(&candidate.model);
            (candidate_key != observed_model_key && candidate_key.starts_with(&observed_model_key))
                .then_some(candidate_key)
        })
        .collect::<BTreeSet<_>>();

    let mut seen_urls = BTreeSet::new();
    source_hints
        .iter()
        .filter_map(|source| {
            canonical_exact_https_origin(&source.evidence_source_url).ok()?;
            let stored_evidence_identity =
                format!("{} {}", source.evidence_source_title, source.evidence_text);
            let conflict_identity = format!(
                "{} {}",
                source.evidence_source_url, stored_evidence_identity
            );
            if !exact_compact_identity_is_present(
                &stored_evidence_identity,
                &observed_manufacturer_key,
            ) || !exact_compact_identity_is_present(
                &stored_evidence_identity,
                &observed_model_key,
            ) || longer_product_keys
                .iter()
                .any(|longer_key| exact_compact_identity_is_present(&conflict_identity, longer_key))
            {
                return None;
            }
            seen_urls
                .insert(source.evidence_source_url.clone())
                .then_some(source.evidence_source_url.clone())
        })
        .take(2)
        .collect()
}

fn should_run_listing_only_approved_candidate_adjudication(_plan: &IdentityGroundingPlan) -> bool {
    // A bounded list of currently approved products is not a closed product
    // family: an OEM sibling may be absent or still unreviewed (for example,
    // GDL 69A versus GDL 69A SXM). Until the adjudicator consumes a complete,
    // revision-bound OEM family set containing both selectable and blocking
    // members, it must never authorize a listing association.
    false
}

fn validate_authorized_direct_source_response(
    plan: &AuthoritativeDirectSourcePlan,
    response: &GroundedJsonResponse,
) -> CatalogResult<()> {
    if !response.authoritative_direct_source_verified {
        return Err(CatalogError::Validation(
            "authoritative direct-source response did not retain verified direct-source provenance"
                .to_string(),
        ));
    }

    // Authorized direct-fetch passes deliberately have no Gemini grounding
    // sources. A safe nonpositive structure response may also use no publisher
    // excerpt, so it correctly has no source-evidence proof. The verified
    // server-fetch window URLs retain the origin provenance in that case;
    // output-used proofs remain a separate requirement for positive claims.
    let final_urls = response
        .authoritative_direct_source_final_urls
        .iter()
        .map(String::as_str)
        .chain(
            response
                .grounding_sources
                .iter()
                .map(|source| source.url.as_str()),
        )
        .chain(
            response
                .source_evidence_proofs
                .iter()
                .map(|proof| proof.final_url.as_str()),
        )
        .filter(|url| !url.trim().is_empty())
        .collect::<HashSet<_>>();
    if final_urls.is_empty() {
        return Err(CatalogError::Validation(
            "authoritative direct-source response did not expose any final source URL".to_string(),
        ));
    }

    for final_url in final_urls {
        if !plan.source_urls.iter().any(|requested_url| {
            plan.admission
                .require_authorized_final_url(requested_url, final_url)
                .is_ok()
        }) {
            return Err(CatalogError::Validation(format!(
                "authoritative direct-source response returned unbound final URL {final_url:?}"
            )));
        }
    }
    Ok(())
}

fn response_evidence_source_urls(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if (key == "source_url" || key.ends_with("_source_url"))
                    && value.as_str().is_some_and(|url| !url.trim().is_empty())
                {
                    output.push(value.as_str().unwrap_or_default().trim().to_string());
                }
                response_evidence_source_urls(value, output);
            }
        }
        Value::Array(values) => {
            for value in values {
                response_evidence_source_urls(value, output);
            }
        }
        _ => {}
    }
}

fn manufacturer_origin_error(error: ManufacturerIdentityError) -> CatalogError {
    match error {
        ManufacturerIdentityError::Database(message) => CatalogError::Database(message),
        ManufacturerIdentityError::Validation(message)
        | ManufacturerIdentityError::Conflict(message) => CatalogError::Validation(message),
    }
}

async fn revalidate_direct_source_admission_state(
    db: &AppDb,
    manufacturer: &str,
    plan: &IdentityGroundingPlan,
    response: &GroundedJsonResponse,
) -> CatalogResult<()> {
    let IdentityGroundingPlan::Direct(plan) = plan else {
        return Ok(());
    };
    let current_admission =
        authorize_manufacturer_source_urls(db, manufacturer, &plan.source_urls)
            .await
            .map_err(|error| match manufacturer_origin_error(error) {
                CatalogError::Database(message) => CatalogError::Database(message),
                error => CatalogError::Validation(format!(
                    "authoritative direct-source admission changed or was revoked while grounding: {error}"
                )),
            })?;
    if current_admission != plan.admission {
        return Err(CatalogError::Validation(
            "authoritative direct-source manufacturer binding changed while grounding".to_string(),
        ));
    }
    validate_authorized_direct_source_response(plan, response)?;
    Ok(())
}

async fn require_response_evidence_source_urls_not_revoked(
    db: &AppDb,
    response: &GroundedJsonResponse,
) -> CatalogResult<()> {
    let mut source_urls = Vec::new();
    response_evidence_source_urls(&response.value, &mut source_urls);
    source_urls.sort();
    source_urls.dedup();
    require_source_urls_not_revoked(db, &source_urls)
        .await
        .map_err(manufacturer_origin_error)
}

async fn resolve_avionics_identity_with_write_mode(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    request: &AvionicsIdentityRequest,
    persist: bool,
) -> CatalogResult<AvionicsIdentityOutcome> {
    if request.manufacturer.trim().is_empty() || request.model.trim().is_empty() {
        return Ok(AvionicsIdentityOutcome::Unresolved {
            reason: "candidate is missing a manufacturer or model label".to_string(),
        });
    }
    let mut seen_types = HashSet::new();
    let input_types = request
        .avionics_types
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .filter(|value| seen_types.insert(normalize_name(value)))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if input_types.is_empty() {
        return Ok(AvionicsIdentityOutcome::Unresolved {
            reason: "candidate is missing an avionics capability observation".to_string(),
        });
    }
    let explicit_direct_source_plan =
        explicit_authoritative_direct_source_plan(db, request).await?;

    if let Some(approved) = resolve_verified_local_avionics_identity(db, request).await? {
        return Ok(AvionicsIdentityOutcome::Approved(approved));
    }

    let review_catalog = load_review_catalog_candidates(db).await?;
    let catalog = review_catalog
        .iter()
        .map(|candidate| candidate.candidate.clone())
        .collect::<Vec<_>>();
    let input_manufacturer_identity_id = match explicit_direct_source_plan.as_ref() {
        Some(plan) => Some(plan.admission.effective_manufacturer_identity_id),
        None => resolve_input_manufacturer_identity(db, &request.manufacturer).await?,
    };
    let manufacturer_catalog = manufacturer_scoped_catalog_candidates(
        &request.manufacturer,
        input_manufacturer_identity_id,
        &review_catalog,
    );
    let mut grounding_plan = if let Some(plan) = explicit_direct_source_plan {
        IdentityGroundingPlan::Direct(plan)
    } else if let Some(plan) = opportunistic_authoritative_direct_source_plan(
        db,
        request,
        &input_types,
        input_manufacturer_identity_id,
        &review_catalog,
    )
    .await?
    {
        IdentityGroundingPlan::Direct(plan)
    } else {
        IdentityGroundingPlan::Search
    };
    if should_run_listing_only_approved_candidate_adjudication(&grounding_plan) {
        let approved_candidates = load_known_approved_candidates(db).await?;
        if let Some(approved) = resolve_approved_catalog_candidate_with_gemini(
            db,
            extractor,
            request,
            &input_types,
            &approved_candidates,
            &catalog,
        )
        .await?
        {
            return Ok(AvionicsIdentityOutcome::Approved(approved));
        }
    }

    let catalog_snapshot = catalog_fingerprint(&catalog);
    let shortlist = shortlist_avionics_candidates(
        &request.manufacturer,
        &request.model,
        &input_types,
        None,
        &manufacturer_catalog,
    );
    let (direct_source_urls, direct_source_anchors) = match &grounding_plan {
        IdentityGroundingPlan::Search => (Vec::new(), Vec::new()),
        IdentityGroundingPlan::Direct(plan) => {
            (plan.source_urls.clone(), plan.identity_anchors.clone())
        }
    };
    let mut context = AvionicsUnitResolutionContext {
        aircraft_manufacturer: request.aircraft_manufacturer.clone(),
        aircraft_model: request.aircraft_model.clone(),
        aircraft_variant: request.aircraft_variant.clone(),
        model_year: request.model_year,
        source_url: request.source_url.clone(),
        listing_context: request.listing_context.clone(),
        requires_listing_evidence: request.requires_listing_evidence,
        authoritative_direct_source_urls: direct_source_urls,
        authoritative_identity_anchors: direct_source_anchors,
        candidate: AvionicsUnitResolutionCandidate {
            manufacturer: request.manufacturer.clone(),
            model: request.model.clone(),
            avionics_types: input_types,
            quantity: request.quantity.max(1),
        },
        catalog_candidates: shortlist.clone(),
    };

    let initial_response = match &grounding_plan {
        IdentityGroundingPlan::Direct(plan)
            if plan.requirement == DirectSourceRequirement::Opportunistic =>
        {
            extractor
                .resolve_avionics_unit_opportunistically(&context)
                .await
        }
        _ => extractor.resolve_avionics_unit(&context).await,
    };
    let mut grounded_response = match initial_response {
        Ok(response) => response,
        Err(direct_error)
            if matches!(
                &grounding_plan,
                IdentityGroundingPlan::Direct(plan)
                    if plan.requirement == DirectSourceRequirement::Opportunistic
            ) && crate::gemini::curation::workflow::is_opportunistic_direct_source_unavailable(
                &direct_error,
            ) =>
        {
            grounding_plan = IdentityGroundingPlan::Search;
            context.authoritative_direct_source_urls.clear();
            context.authoritative_identity_anchors.clear();
            extractor
                .resolve_avionics_unit(&context)
                .await
                .map_err(|search_error| {
                    CatalogError::Gemini(format!(
                        "Gemini avionics Search fallback failed after the opportunistic local source preflight could not use its retrieval hint ({direct_error:#}): {search_error:#}"
                    ))
                })?
        }
        Err(error) => {
            return Err(CatalogError::Gemini(format!(
                "Gemini avionics identity resolution failed: {error:#}"
            )));
        }
    };
    // Recheck only server-owned direct-source admission here. Model-owned
    // source URL defects belong to the bounded domain-correction pass below.
    revalidate_direct_source_admission_state(
        db,
        &request.manufacturer,
        &grounding_plan,
        &grounded_response,
    )
    .await?;
    let verified_evidence = grounded_response.verified_evidence.clone();
    let mut response = grounded_response.value.clone();
    let mut issues = resolution_issues_with_direct_source_proofs(
        &context,
        &response,
        grounded_response_has_verified_evidence(&grounded_response),
        &grounded_response.grounding_sources,
        &grounded_response.grounding_supports,
        &grounded_response.source_evidence_proofs,
    );
    let invalid_rejection_seen =
        string_field(&response, "status") == "reject" && !issues.is_empty();
    if !issues.is_empty() {
        let correction_context = AvionicsUnitResolutionCorrectionContext {
            issues,
            secondary_check: None,
        };
        let corrected_response = match verified_evidence.as_ref() {
            Some(evidence) => {
                extractor
                    .correct_avionics_unit_resolution_reusing(
                        &context,
                        &response,
                        &correction_context,
                        &evidence.dossier,
                    )
                    .await
            }
            None => {
                extractor
                    .correct_avionics_unit_resolution(&context, &response, &correction_context)
                    .await
            }
        };
        grounded_response = match corrected_response {
            Ok(response) => response,
            Err(error) if invalid_rejection_seen => {
                return Ok(AvionicsIdentityOutcome::Unresolved {
                    reason: format!(
                        "Gemini's reject decision could not be accepted safely because its correction failed: {error:#}"
                    ),
                });
            }
            Err(error) => {
                return Err(CatalogError::Gemini(format!(
                    "Gemini avionics identity correction failed: {error:#}"
                )));
            }
        };
        // A correction is a new model call, so recheck server-owned direct
        // source admission without preempting corrected response validation.
        revalidate_direct_source_admission_state(
            db,
            &request.manufacturer,
            &grounding_plan,
            &grounded_response,
        )
        .await?;
        response = grounded_response.value.clone();
        issues = resolution_issues_with_direct_source_proofs(
            &context,
            &response,
            grounded_response_has_verified_evidence(&grounded_response),
            &grounded_response.grounding_sources,
            &grounded_response.grounding_supports,
            &grounded_response.source_evidence_proofs,
        );
    }
    if !issues.is_empty() {
        if invalid_rejection_seen || string_field(&response, "status") == "reject" {
            return Ok(AvionicsIdentityOutcome::Unresolved {
                reason: format!(
                    "Gemini's reject decision remained unsafe after correction: {}",
                    issues.join("; ")
                ),
            });
        }
        return Err(CatalogError::Validation(format!(
            "Gemini avionics identity response remained invalid after correction: {}",
            issues.join("; ")
        )));
    }

    if let Some(outcome) = nonpositive_identity_outcome(
        &context,
        &response,
        grounded_response_has_verified_evidence(&grounded_response),
        &grounded_response.grounding_sources,
        &grounded_response.grounding_supports,
    ) {
        return Ok(outcome);
    }
    // Only a positive, domain-valid response may reach origin
    // canonicalization and revocation checks.
    require_response_evidence_source_urls_not_revoked(db, &grounded_response).await?;
    let status = response["status"].as_str().unwrap_or_default();
    match status {
        "existing_match" => {
            let catalog_id = response["catalog_id"].as_i64().unwrap_or_default();
            let _selected = shortlist
                .iter()
                .find(|candidate| candidate.id == catalog_id)
                .ok_or_else(|| {
                    CatalogError::Validation(format!(
                        "Gemini selected unknown catalog id {catalog_id}"
                    ))
                })?;
            let proposed = verified_identity_from_response(&response)?;
            let collision_context =
                expanded_collision_context(&context, &proposed, &manufacturer_catalog);
            resolve_verified_identity(
                db,
                extractor,
                &context,
                &collision_context,
                proposed,
                Some(catalog_id),
                grounded_response.verified_evidence.as_ref(),
                &grounding_plan,
                &catalog_snapshot,
                persist,
            )
            .await
        }
        "propose_new" => {
            let proposed = verified_identity_from_response(&response)?;
            let collision_context =
                expanded_collision_context(&context, &proposed, &manufacturer_catalog);
            resolve_verified_identity(
                db,
                extractor,
                &context,
                &collision_context,
                proposed,
                None,
                grounded_response.verified_evidence.as_ref(),
                &grounding_plan,
                &catalog_snapshot,
                persist,
            )
            .await
        }
        _ => Err(CatalogError::Validation(format!(
            "unexpected Gemini avionics identity status: {status}"
        ))),
    }
}

async fn resolve_verified_identity(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    source_context: &AvionicsUnitResolutionContext,
    context: &AvionicsUnitResolutionContext,
    mut proposed: VerifiedIdentity,
    selected_existing_id: Option<i64>,
    source_evidence: Option<&crate::extract::GroundedAvionicsEvidence>,
    grounding_plan: &IdentityGroundingPlan,
    reviewed_catalog_fingerprint: &str,
    persist: bool,
) -> CatalogResult<AvionicsIdentityOutcome> {
    let review_context = AvionicsCatalogCollisionReviewContext {
        classification_context: context.clone(),
        proposed_identity: AvionicsProposedIdentity {
            canonical_manufacturer: proposed.canonical_manufacturer.clone(),
            canonical_model: proposed.canonical_model.clone(),
            canonical_types: proposed.canonical_types.clone(),
            manufacturer_identifier_kind: proposed.manufacturer_identifier_kind.clone(),
            manufacturer_identifier: proposed.manufacturer_identifier.clone(),
        },
    };
    let review_result = match source_evidence {
        Some(evidence) if evidence.audit.verified_direct_source => {
            extractor
                .review_avionics_catalog_collisions_reusing_direct_source(
                    source_context,
                    &review_context,
                    &evidence.dossier,
                )
                .await
        }
        _ => {
            extractor
                .review_avionics_catalog_collisions(&review_context)
                .await
        }
    };
    let mut review_response = match review_result {
        Ok(response) => response,
        Err(error) => {
            return Ok(AvionicsIdentityOutcome::Unresolved {
                reason: format!(
                    "independent grounded collision review could not establish a safe catalog decision: {error:#}"
                ),
            });
        }
    };
    // Preserve fail-closed direct-source admission without parsing
    // model-owned proposal or candidate URLs before domain correction.
    revalidate_direct_source_admission_state(
        db,
        &source_context.candidate.manufacturer,
        grounding_plan,
        &review_response,
    )
    .await?;
    let mut domain_issues = collision_response_issues(
        context,
        &proposed,
        &review_response.value,
        &review_response.source_evidence_proofs,
    );
    if !domain_issues.is_empty() {
        let Some(evidence) = review_response.verified_evidence.as_ref() else {
            return Err(CatalogError::Validation(domain_issues.join("; ")));
        };
        let initial_structure_calls = evidence.audit.current_structure_calls;
        if collision_correction_plan(initial_structure_calls)?
            == CollisionCorrectionPlan::BudgetExhausted
        {
            return Err(CatalogError::Validation(format!(
                "Gemini avionics collision response remained domain-invalid after exhausting the shared {COLLISION_STRUCTURE_CALL_BUDGET}-call structure budget; correction was skipped: {}",
                domain_issues.join("; ")
            )));
        }
        review_response = extractor
            .correct_avionics_catalog_collision_review_reusing(
                &review_context,
                &review_response.value,
                &domain_issues,
                &evidence.dossier,
            )
            .await
            .map_err(|error| {
                CatalogError::Gemini(format!(
                    "Gemini avionics collision correction failed: {error:#}"
                ))
            })?;
        let correction_structure_calls = review_response
            .verified_evidence
            .as_ref()
            .ok_or_else(|| {
                CatalogError::Validation(
                    "Gemini avionics collision correction returned no verified evidence audit"
                        .to_string(),
                )
            })?
            .audit
            .current_structure_calls;
        validate_collision_correction_call_budget(
            initial_structure_calls,
            correction_structure_calls,
        )?;
        revalidate_direct_source_admission_state(
            db,
            &source_context.candidate.manufacturer,
            grounding_plan,
            &review_response,
        )
        .await?;
        domain_issues = collision_response_issues(
            context,
            &proposed,
            &review_response.value,
            &review_response.source_evidence_proofs,
        );
        if !domain_issues.is_empty() {
            return Err(CatalogError::Validation(format!(
                "Gemini avionics collision response remained invalid after structure-only correction: {}",
                domain_issues.join("; ")
            )));
        }
    }
    let attestation = proposal_attestation_with_direct_source_proofs(
        context,
        &proposed,
        &review_response.value,
        &review_response.source_evidence_proofs,
    )?;
    if !attestation.confirmed {
        return Ok(AvionicsIdentityOutcome::Unresolved {
            reason: attestation.reason,
        });
    }
    // The response is confirmed and domain-valid; its final proposal and
    // candidate URLs can now be canonicalized and checked for revocation.
    require_response_evidence_source_urls_not_revoked(db, &review_response).await?;
    if !grounded_response_has_verified_evidence(&review_response) {
        return Err(CatalogError::Validation(
            "confirmed Gemini proposal review did not use verified Search + URL Context or authoritative direct-source + URL Context evidence"
                .to_string(),
        ));
    }
    proposed.identity_source_url = attestation.source_url;
    proposed.identity_source_title = attestation.source_title;
    proposed.identity_evidence = attestation.evidence;
    proposed.reason = attestation.reason;
    let reviews = collision_reviews_with_direct_source_proofs(
        context,
        &review_response.value,
        &review_response.source_evidence_proofs,
    )?;
    let mut grounded_claim_source_urls = Vec::new();
    response_evidence_source_urls(&review_response.value, &mut grounded_claim_source_urls);
    grounded_claim_source_urls.sort();
    grounded_claim_source_urls.dedup();
    proposed.grounded_claim_source_urls = grounded_claim_source_urls;
    let same_ids = reviews
        .iter()
        .filter(|review| review.decision == "same_product")
        .map(|review| review.catalog_id)
        .collect::<Vec<_>>();

    // A first-stage existing_match is still only a proposal. Require the
    // independent collision pass to confirm the selected row, regardless of
    // whether that row is already approved or remains legacy-unreviewed.
    if let Some(selected_id) = selected_existing_id {
        if !same_ids.contains(&selected_id) {
            return Ok(AvionicsIdentityOutcome::Unresolved {
                reason: format!(
                    "independent collision review did not confirm selected catalog id {selected_id} as the same product"
                ),
            });
        }
    }

    let approved_same = context
        .catalog_candidates
        .iter()
        .filter(|candidate| {
            same_ids.contains(&candidate.id) && candidate.catalog_status == "approved"
        })
        .collect::<Vec<_>>();
    if approved_same.len() > 1 {
        return Ok(AvionicsIdentityOutcome::Unresolved {
            reason: format!(
                "collision review found multiple approved identities for the same product: {:?}",
                approved_same
                    .iter()
                    .map(|candidate| candidate.id)
                    .collect::<Vec<_>>()
            ),
        });
    }
    if let Some(existing) = approved_same.first() {
        let review = reviews
            .iter()
            .find(|review| review.catalog_id == existing.id)
            .expect("approved same-product id came from collision reviews");
        let additions = match approved_capability_additions(existing, &proposed) {
            Ok(additions) => additions,
            Err(error) => {
                return Ok(AvionicsIdentityOutcome::Unresolved {
                    reason: error.to_string(),
                });
            }
        };
        if !additions.is_empty() {
            if !persist {
                return Ok(AvionicsIdentityOutcome::Approved(
                    approved_identity_from_verified(existing.id, &proposed),
                ));
            }
            let stored = persist_approved_capability_enrichment(
                db,
                existing,
                &proposed,
                reviewed_catalog_fingerprint,
            )
            .await?;
            return Ok(AvionicsIdentityOutcome::Approved(stored));
        }
        if persist {
            persist_existing_reuse_attestation(
                db,
                existing,
                &proposed,
                reviewed_catalog_fingerprint,
            )
            .await?;
        }
        return Ok(AvionicsIdentityOutcome::Approved(
            ApprovedAvionicsIdentity {
                id: existing.id,
                manufacturer: existing.manufacturer.clone(),
                model: existing.model.clone(),
                avionics_types: existing.avionics_types.clone(),
                manufacturer_identifier_kind: existing.manufacturer_identifier_kind.clone(),
                manufacturer_identifier: existing.manufacturer_identifier.clone(),
                evidence_url: review.candidate_source_url.clone(),
                evidence_title: review.candidate_source_title.clone(),
                evidence: review.candidate_evidence.clone(),
                reason: review.reason.clone(),
                grounded_claim_source_urls: proposed.grounded_claim_source_urls.clone(),
            },
        ));
    }

    if same_ids.len() > 1 {
        let mut duplicate_ids = same_ids.clone();
        duplicate_ids.sort_unstable();
        duplicate_ids.dedup();
        if duplicate_ids.len() > 1 {
            return Ok(AvionicsIdentityOutcome::Unresolved {
                reason: format!(
                    "independent collision review confirmed multiple legacy catalog rows as the same product ({duplicate_ids:?}); explicitly consolidate them before approving the identity"
                ),
            });
        }
    }

    let target_id = selected_existing_id.or_else(|| same_ids.iter().copied().min());
    if !persist {
        return Ok(AvionicsIdentityOutcome::Approved(
            approved_identity_from_verified(target_id.unwrap_or(0), &proposed),
        ));
    }
    let stored = persist_approved_identity(
        db,
        target_id,
        &same_ids,
        &proposed,
        reviewed_catalog_fingerprint,
    )
    .await?;
    Ok(AvionicsIdentityOutcome::Approved(stored))
}

async fn load_catalog_candidates(db: &AppDb) -> CatalogResult<Vec<AvionicsCatalogCandidate>> {
    let rows = query_as_all!(db, CatalogRow, CATALOG_SELECT_SQL)?;
    Ok(catalog_candidates_from_rows(rows))
}

async fn load_review_catalog_candidates(db: &AppDb) -> CatalogResult<Vec<ReviewCatalogCandidate>> {
    let rows = query_as_all!(db, CatalogRow, CATALOG_SELECT_SQL)?;
    Ok(review_catalog_candidates_from_rows(rows))
}

/// Retrieve, but never authorize, one exact-model candidate for review.
///
/// An evidence-backed effective manufacturer identity may safely scope an
/// exact-model lookup. Without one, a raw manufacturer name may only scope a
/// lookup when no other raw manufacturer scope collides. Capabilities validate
/// the one remaining identity; they must never make split catalog rows appear
/// unique. Ambiguous matches are deliberately hidden.
pub(crate) async fn unique_exact_avionics_review_candidate(
    db: &AppDb,
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
) -> CatalogResult<Option<AvionicsReviewCatalogCandidate>> {
    let manufacturer_identity_id = resolve_input_manufacturer_identity(db, manufacturer).await?;
    let catalog = load_review_catalog_candidates(db).await?;
    let selected = select_unique_exact_review_candidate(
        manufacturer,
        model,
        avionics_types,
        manufacturer_identity_id,
        &catalog,
    );
    let Some(selected) = selected else {
        return Ok(None);
    };
    if selected.catalog_status == "approved"
        && !current_reuse_attested_product_ids(db)
            .await?
            .contains(&selected.id)
    {
        return Ok(None);
    }
    Ok(Some(selected))
}

fn select_unique_exact_review_candidate(
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
    manufacturer_identity_id: Option<i64>,
    catalog: &[ReviewCatalogCandidate],
) -> Option<AvionicsReviewCatalogCandidate> {
    if !is_usable_avionics_label(manufacturer, model) {
        return None;
    }
    let model_key = normalize_avionics_identifier(model);
    let observed_types = canonicalize_avionics_types(avionics_types);
    if model_key.is_empty()
        || observed_types.is_empty()
        || observed_types.len() != avionics_types.len()
    {
        return None;
    }

    // Resolve every exact identity collision before looking at capabilities.
    // Otherwise two rows for one physical product can each appear unique merely
    // because legacy ingestion assigned different capabilities to each row.
    let model_matches = catalog
        .iter()
        .filter(|candidate| normalize_avionics_identifier(&candidate.candidate.model) == model_key)
        .collect::<Vec<_>>();
    let manufacturer_key = exact_manufacturer_name_key(manufacturer);
    let candidates = if let Some(manufacturer_identity_id) = manufacturer_identity_id {
        // A manufacturer identity is authoritative enough to distinguish the
        // same model label sold by genuinely different manufacturers. Any
        // unscoped exact-model row remains an unresolved collision, however:
        // treating it as a different maker would amount to alias inference.
        if model_matches
            .iter()
            .any(|candidate| candidate.manufacturer_identity_id.is_none())
        {
            return None;
        }
        model_matches
            .into_iter()
            .filter(|candidate| {
                candidate.manufacturer_identity_id == Some(manufacturer_identity_id)
            })
            .collect::<Vec<_>>()
    } else {
        let raw_manufacturer_matches = model_matches
            .iter()
            .copied()
            .filter(|candidate| {
                exact_manufacturer_name_key(&candidate.candidate.manufacturer) == manufacturer_key
            })
            .collect::<Vec<_>>();
        if raw_manufacturer_matches.is_empty() {
            model_matches
        } else if raw_manufacturer_matches.len() == model_matches.len() {
            raw_manufacturer_matches
        } else {
            // Multiple raw manufacturer scopes may be historical aliases.
            // Without evidence-backed identities, fail closed.
            return None;
        }
    };
    let [candidate] = candidates.as_slice() else {
        return None;
    };
    let candidate = &candidate.candidate;
    if !observed_types
        .iter()
        .all(|capability| candidate.avionics_types.contains(capability))
    {
        return None;
    }
    Some(AvionicsReviewCatalogCandidate {
        id: candidate.id,
        manufacturer: candidate.manufacturer.clone(),
        model: candidate.model.clone(),
        avionics_types: candidate.avionics_types.clone(),
        manufacturer_identifier_kind: candidate.manufacturer_identifier_kind.clone(),
        manufacturer_identifier: candidate.manufacturer_identifier.clone(),
        catalog_status: candidate.catalog_status.clone(),
    })
}

async fn resolve_input_manufacturer_identity(
    db: &AppDb,
    manufacturer: &str,
) -> CatalogResult<Option<i64>> {
    let rows = load_manufacturer_identity_memberships(db).await?;
    Ok(resolve_input_manufacturer_identity_from_memberships(
        manufacturer,
        &rows,
    ))
}

async fn load_manufacturer_identity_memberships(db: &AppDb) -> CatalogResult<Vec<(String, i64)>> {
    let sql = db.sql(
        r#"
        SELECT manufacturer.name,
               effective.avionics_manufacturer_identity_id
        FROM avionics_manufacturers manufacturer
        JOIN avionics_manufacturer_effective_memberships effective
          ON effective.avionics_manufacturer_id = manufacturer.id
        ORDER BY manufacturer.id
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_as(&sql).fetch_all(pool).await?,
        DatabaseBackend::Postgres(pool) => sqlx::query_as(&sql).fetch_all(pool).await?,
    };
    Ok(rows)
}

fn resolve_input_manufacturer_identity_from_memberships(
    manufacturer: &str,
    rows: &[(String, i64)],
) -> Option<i64> {
    if is_generic_avionics_manufacturer_name(manufacturer) {
        return None;
    }
    let input_key = exact_manufacturer_name_key(manufacturer);
    let identities = rows
        .iter()
        .filter(|(stored_name, _)| exact_manufacturer_name_key(stored_name) == input_key)
        .map(|(_, identity_id)| *identity_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let [identity_id] = identities.as_slice() else {
        return None;
    };
    Some(*identity_id)
}

async fn load_known_approved_candidates(
    db: &AppDb,
) -> CatalogResult<Vec<KnownApprovedAvionicsCandidate>> {
    let reuse_attested_ids = current_reuse_attested_product_ids(db).await?;
    load_known_approved_candidates_for_ids(db, &reuse_attested_ids).await
}

async fn load_known_approved_candidates_for_ids(
    db: &AppDb,
    reuse_attested_ids: &HashSet<i64>,
) -> CatalogResult<Vec<KnownApprovedAvionicsCandidate>> {
    let mut candidates = load_graph_approved_candidates(db)
        .await?
        .into_iter()
        .filter(|candidate| reuse_attested_ids.contains(&candidate.id))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| candidate.id);
    Ok(candidates)
}

/// Load every graph-approved identity, including products that have not yet
/// received a current-policy reuse attestation.
///
/// The ordinary local fast path deliberately uses only
/// `load_known_approved_candidates`; deterministic OEM re-attestation needs to
/// inspect the unattested hash-bound target without weakening that rule.
async fn load_graph_approved_candidates(
    db: &AppDb,
) -> CatalogResult<Vec<KnownApprovedAvionicsCandidate>> {
    let rows = query_as_all!(db, KnownApprovedRow, KNOWN_APPROVED_SELECT_SQL)?;
    Ok(graph_approved_candidates_from_rows(rows))
}

fn graph_approved_candidates_from_rows(
    rows: Vec<KnownApprovedRow>,
) -> Vec<KnownApprovedAvionicsCandidate> {
    let mut by_id = HashMap::<i64, KnownApprovedAvionicsCandidate>::new();
    for row in rows.into_iter().filter(|row| {
        row.catalog_status == "approved" && is_usable_avionics_label(&row.manufacturer, &row.model)
    }) {
        let capabilities = canonical_avionics_types_for_label(&row.capability_type);
        let candidate = by_id
            .entry(row.id)
            .or_insert_with(|| KnownApprovedAvionicsCandidate {
                id: row.id,
                manufacturer: row.manufacturer,
                model: row.model,
                avionics_types: Vec::new(),
                manufacturer_identifier_kind: row.manufacturer_identifier_kind.unwrap_or_default(),
                manufacturer_identifier: row.manufacturer_identifier.unwrap_or_default(),
                identity_source_url: row.identity_source_url.unwrap_or_default(),
                identity_source_title: row.identity_source_title.unwrap_or_default(),
                identity_evidence: row.identity_evidence.unwrap_or_default(),
                avionics_manufacturer_identity_id: row.avionics_manufacturer_identity_id,
                canonical_product_key: row.canonical_product_key,
                canonical_identifier_key: row.canonical_identifier_key,
            });
        candidate
            .avionics_types
            .extend(capabilities.into_iter().map(str::to_string));
    }
    let mut candidates = by_id.into_values().collect::<Vec<_>>();
    for candidate in &mut candidates {
        candidate.avionics_types = canonicalize_avionics_types(&candidate.avionics_types);
    }
    candidates.sort_by_key(|candidate| candidate.id);
    candidates
}

async fn resolve_approved_catalog_candidate_with_gemini(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    request: &AvionicsIdentityRequest,
    input_types: &[String],
    approved_candidates: &[KnownApprovedAvionicsCandidate],
    catalog: &[AvionicsCatalogCandidate],
) -> CatalogResult<Option<ApprovedAvionicsIdentity>> {
    if !request.requires_listing_evidence
        || request.listing_context.trim().is_empty()
        || !is_usable_avionics_label(&request.manufacturer, &request.model)
        || input_types.is_empty()
        || input_types
            .iter()
            .any(|capability| !CURATED_AVIONICS_TYPES.contains(&capability.as_str()))
    {
        return Ok(None);
    }

    let observed_product_key = normalize_avionics_identifier(&request.model);
    if observed_product_key.is_empty()
        || !exact_compact_identity_is_present(&request.listing_context, &observed_product_key)
        || !exact_token_phrase_is_present(&request.listing_context, &request.manufacturer)
    {
        return Ok(None);
    }

    let Some(manufacturer_identity_id) =
        resolve_input_manufacturer_identity(db, &request.manufacturer).await?
    else {
        return Ok(None);
    };
    let Some(plan) = approved_candidate_adjudication_plan(
        request,
        input_types,
        manufacturer_identity_id,
        approved_candidates,
        catalog,
    ) else {
        return Ok(None);
    };

    // This is an optimization only. A model, transport, parsing, or local
    // validation failure must preserve the existing grounded resolver's
    // behavior rather than becoming a new rejection or pending state.
    let response = match extractor
        .adjudicate_approved_avionics_candidates(&plan.context)
        .await
    {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    let Some(selected_id) = approved_candidate_adjudication_selection(request, &plan, &response)
    else {
        return Ok(None);
    };

    // The model compared a snapshot. Re-read the graph and rebuild the plan
    // before accepting so concurrent catalog curation cannot make its answer
    // refer to stale identity or capability data.
    let refreshed_catalog = load_catalog_candidates(db).await?;
    let refreshed_candidates = load_known_approved_candidates(db).await?;
    let Some(refreshed_manufacturer_identity_id) =
        resolve_input_manufacturer_identity(db, &request.manufacturer).await?
    else {
        return Ok(None);
    };
    let Some(refreshed_plan) = approved_candidate_adjudication_plan(
        request,
        input_types,
        refreshed_manufacturer_identity_id,
        &refreshed_candidates,
        &refreshed_catalog,
    ) else {
        return Ok(None);
    };
    if !approved_candidate_adjudication_plan_is_unchanged(&plan, &refreshed_plan)
        || approved_candidate_adjudication_selection(request, &refreshed_plan, &response)
            != Some(selected_id)
    {
        return Ok(None);
    }
    let Some(selected) = refreshed_candidates
        .iter()
        .find(|candidate| candidate.id == selected_id)
    else {
        return Ok(None);
    };

    Ok(Some(ApprovedAvionicsIdentity {
        id: selected.id,
        manufacturer: selected.manufacturer.clone(),
        model: selected.model.clone(),
        avionics_types: selected.avionics_types.clone(),
        manufacturer_identifier_kind: selected.manufacturer_identifier_kind.clone(),
        manufacturer_identifier: selected.manufacturer_identifier.clone(),
        evidence_url: selected.identity_source_url.clone(),
        evidence_title: selected.identity_source_title.clone(),
        evidence: selected.identity_evidence.clone(),
        reason:
            "Matched one unchanged graph-approved catalog product through bounded Gemini listing-evidence adjudication without Search, URL Context, collision review, or catalog mutation"
                .to_string(),
        grounded_claim_source_urls: Vec::new(),
    }))
}

fn approved_candidate_adjudication_plan(
    request: &AvionicsIdentityRequest,
    input_types: &[String],
    manufacturer_identity_id: i64,
    approved_candidates: &[KnownApprovedAvionicsCandidate],
    catalog: &[AvionicsCatalogCandidate],
) -> Option<ApprovedCandidateAdjudicationPlan> {
    let observed_types = canonicalize_avionics_types(input_types);
    if observed_types.is_empty() || observed_types.len() != input_types.len() {
        return None;
    }
    let observed_product_key = normalize_avionics_identifier(&request.model);
    let exact_matches = approved_candidates
        .iter()
        .filter(|candidate| {
            if candidate.avionics_manufacturer_identity_id != manufacturer_identity_id
                || !observed_types
                    .iter()
                    .all(|capability| candidate.avionics_types.contains(capability))
            {
                return false;
            }
            let product_match = observed_product_key == candidate.canonical_product_key;
            let identifier_match = observed_product_key == candidate.canonical_identifier_key
                && exact_stable_identifier_is_present(
                    &request.listing_context,
                    &candidate.manufacturer_identifier_kind,
                    &candidate.canonical_identifier_key,
                );
            product_match || identifier_match
        })
        .collect::<Vec<_>>();
    let [selected] = exact_matches.as_slice() else {
        return None;
    };
    if approved_candidate_has_identity_collision(selected, catalog) {
        return None;
    }

    // If the retained listing contains a longer/shorter colliding catalog
    // label, the extracted observation may have lost a discriminating suffix.
    // That case belongs in the grounded pipeline.
    if catalog.iter().any(|candidate| {
        candidate.id != selected.id
            && strict_product_prefix_collision(
                &selected.canonical_product_key,
                &normalize_avionics_identifier(&candidate.model),
            )
            && exact_compact_identity_is_present(
                &request.listing_context,
                &normalize_avionics_identifier(&candidate.model),
            )
    }) {
        return None;
    }

    // Prefix/suffix neighbors are identity-critical collision alternatives.
    // Supply the complete graph-approved closure or skip this bounded route;
    // never silently truncate it.
    let mut prompt_candidates = vec![(*selected).clone()];
    let mut seen_ids = HashSet::from([selected.id]);
    for candidate in approved_candidates.iter().filter(|candidate| {
        candidate.avionics_manufacturer_identity_id == manufacturer_identity_id
            && candidate.id != selected.id
            && strict_product_prefix_collision(
                &selected.canonical_product_key,
                &candidate.canonical_product_key,
            )
    }) {
        if seen_ids.insert(candidate.id) {
            prompt_candidates.push(candidate.clone());
        }
    }
    if prompt_candidates.len() > AVIONICS_APPROVED_CANDIDATE_ADJUDICATION_LIMIT {
        return None;
    }

    // Fill the remaining bounded context with graph-approved local similarity
    // candidates. These help the model distinguish nearby products, but only
    // the exact server-scoped identity above is eligible for acceptance.
    let approved_catalog = approved_candidates
        .iter()
        .filter(|candidate| {
            candidate.avionics_manufacturer_identity_id == manufacturer_identity_id
                && observed_types
                    .iter()
                    .all(|capability| candidate.avionics_types.contains(capability))
        })
        .map(known_approved_catalog_candidate)
        .collect::<Vec<_>>();
    for candidate in shortlist_avionics_candidates(
        &request.manufacturer,
        &request.model,
        &observed_types,
        None,
        &approved_catalog,
    ) {
        if prompt_candidates.len() >= AVIONICS_APPROVED_CANDIDATE_ADJUDICATION_LIMIT {
            break;
        }
        if seen_ids.insert(candidate.id) {
            let known = approved_candidates
                .iter()
                .find(|known| known.id == candidate.id)?;
            prompt_candidates.push(known.clone());
        }
    }

    let context = AvionicsApprovedCandidateAdjudicationContext {
        observed_candidate: AvionicsUnitResolutionCandidate {
            manufacturer: request.manufacturer.clone(),
            model: request.model.clone(),
            avionics_types: observed_types,
            quantity: request.quantity.max(1),
        },
        listing_evidence_text: request.listing_context.clone(),
        approved_candidates: prompt_candidates
            .iter()
            .map(known_approved_prompt_candidate)
            .collect(),
    };
    Some(ApprovedCandidateAdjudicationPlan {
        context,
        selectable_catalog_ids: HashSet::from([selected.id]),
        manufacturer_identity_id,
    })
}

fn known_approved_catalog_candidate(
    candidate: &KnownApprovedAvionicsCandidate,
) -> AvionicsCatalogCandidate {
    AvionicsCatalogCandidate {
        id: candidate.id,
        manufacturer: candidate.manufacturer.clone(),
        model: candidate.model.clone(),
        avionics_types: candidate.avionics_types.clone(),
        manufacturer_identifier_kind: candidate.manufacturer_identifier_kind.clone(),
        manufacturer_identifier: candidate.manufacturer_identifier.clone(),
        catalog_status: "approved".to_string(),
    }
}

fn known_approved_prompt_candidate(
    candidate: &KnownApprovedAvionicsCandidate,
) -> AvionicsApprovedCatalogCandidate {
    AvionicsApprovedCatalogCandidate {
        id: candidate.id,
        manufacturer: candidate.manufacturer.clone(),
        model: candidate.model.clone(),
        avionics_types: candidate.avionics_types.clone(),
        manufacturer_identifier_kind: candidate.manufacturer_identifier_kind.clone(),
        manufacturer_identifier: candidate.manufacturer_identifier.clone(),
    }
}

fn approved_candidate_has_identity_collision(
    selected: &KnownApprovedAvionicsCandidate,
    catalog: &[AvionicsCatalogCandidate],
) -> bool {
    catalog.iter().any(|candidate| {
        if candidate.id == selected.id {
            return false;
        }
        let product_collision =
            normalize_avionics_identifier(&candidate.model) == selected.canonical_product_key;
        let identifier_collision = !selected.canonical_identifier_key.is_empty()
            && candidate.manufacturer_identifier_kind == selected.manufacturer_identifier_kind
            && normalize_avionics_identifier(&candidate.manufacturer_identifier)
                == selected.canonical_identifier_key;
        product_collision || identifier_collision
    })
}

fn approved_candidate_adjudication_selection(
    request: &AvionicsIdentityRequest,
    plan: &ApprovedCandidateAdjudicationPlan,
    response: &Value,
) -> Option<i64> {
    let object = response.as_object()?;
    if object.len() != 5
        || string_field(response, "decision") != "same"
        || string_field(response, "confidence") != "very_high"
        || string_field(response, "reason").trim().is_empty()
    {
        return None;
    }
    let selected_id = object.get("selected_catalog_id")?.as_i64()?;
    if !plan.selectable_catalog_ids.contains(&selected_id)
        || !plan
            .context
            .approved_candidates
            .iter()
            .any(|candidate| candidate.id == selected_id)
    {
        return None;
    }
    let evidence = string_field(response, "evidence_text");
    let observed_product_key = normalize_avionics_identifier(&request.model);
    if evidence.is_empty()
        || evidence != evidence.trim()
        || !request.listing_context.contains(evidence)
        || !exact_compact_identity_is_present(evidence, &observed_product_key)
    {
        return None;
    }
    Some(selected_id)
}

fn approved_candidate_adjudication_plan_is_unchanged(
    before: &ApprovedCandidateAdjudicationPlan,
    after: &ApprovedCandidateAdjudicationPlan,
) -> bool {
    before.manufacturer_identity_id == after.manufacturer_identity_id
        && before.selectable_catalog_ids == after.selectable_catalog_ids
        && before.context.observed_candidate.manufacturer
            == after.context.observed_candidate.manufacturer
        && before.context.observed_candidate.model == after.context.observed_candidate.model
        && before.context.observed_candidate.avionics_types
            == after.context.observed_candidate.avionics_types
        && before.context.observed_candidate.quantity == after.context.observed_candidate.quantity
        && before.context.listing_evidence_text == after.context.listing_evidence_text
        && before.context.approved_candidates.len() == after.context.approved_candidates.len()
        && before
            .context
            .approved_candidates
            .iter()
            .zip(&after.context.approved_candidates)
            .all(|(left, right)| {
                left.id == right.id
                    && left.manufacturer == right.manufacturer
                    && left.model == right.model
                    && left.avionics_types == right.avionics_types
                    && left.manufacturer_identifier_kind == right.manufacturer_identifier_kind
                    && left.manufacturer_identifier == right.manufacturer_identifier
            })
}

#[cfg(test)]
fn known_approved_local_match(
    request: &AvionicsIdentityRequest,
    observed_types: &[String],
    manufacturer_identity_id: i64,
    candidates: &[KnownApprovedAvionicsCandidate],
    catalog: &[AvionicsCatalogCandidate],
) -> Option<ApprovedAvionicsIdentity> {
    known_approved_local_match_core(
        &request.model,
        &request.listing_context,
        observed_types,
        manufacturer_identity_id,
        candidates,
        catalog,
    )
}

fn known_approved_local_match_core(
    observed_model: &str,
    listing_evidence_text: &str,
    observed_types: &[String],
    manufacturer_identity_id: i64,
    candidates: &[KnownApprovedAvionicsCandidate],
    catalog: &[AvionicsCatalogCandidate],
) -> Option<ApprovedAvionicsIdentity> {
    let observed_product_key = normalize_avionics_identifier(observed_model);
    if observed_product_key.is_empty() {
        return None;
    }
    let eligible_candidates = candidates
        .iter()
        .filter(|candidate| {
            if candidate.avionics_manufacturer_identity_id != manufacturer_identity_id
                || observed_types.is_empty()
                || !observed_types
                    .iter()
                    .all(|capability| candidate.avionics_types.contains(capability))
            {
                return false;
            }
            true
        })
        .collect::<Vec<_>>();

    let identifier_matches = eligible_candidates
        .iter()
        .filter_map(|candidate| {
            let request_names_candidate = observed_product_key == candidate.canonical_product_key
                || observed_product_key == candidate.canonical_identifier_key;
            let identifier_match = request_names_candidate
                && has_distinct_exact_oem_part_or_sku(
                    &candidate.manufacturer_identifier_kind,
                    &candidate.manufacturer_identifier,
                    &candidate.canonical_product_key,
                )
                && exact_stable_identifier_is_present(
                    listing_evidence_text,
                    &candidate.manufacturer_identifier_kind,
                    &candidate.canonical_identifier_key,
                );
            identifier_match.then_some(candidate)
        })
        .collect::<Vec<_>>();
    let (selected, match_reason) = match identifier_matches.as_slice() {
        [selected] => (
            **selected,
            "Matched one unchanged graph-approved catalog product from an exact retained OEM part number or SKU without Gemini",
        ),
        [] => {
            let exact_model_matches = eligible_candidates
                .iter()
                .filter_map(|candidate| {
                    (observed_product_key == candidate.canonical_product_key
                        && exact_compact_identity_is_present(
                            listing_evidence_text,
                            &candidate.canonical_product_key,
                        ))
                    .then_some(*candidate)
                })
                .collect::<Vec<_>>();
            let [selected] = exact_model_matches.as_slice() else {
                return None;
            };
            if listing_names_longer_catalog_variant(selected, catalog, listing_evidence_text) {
                return None;
            }
            (
                *selected,
                "Matched one current graph-approved product from an exact retained manufacturer/model label without requiring an OEM part number in the listing",
            )
        }
        _ => return None,
    };

    if catalog_identity_is_duplicated(selected, catalog) {
        return None;
    }

    let selected_manufacturer = normalize_avionics_manufacturer_name(&selected.manufacturer);
    let selected_model = normalize_avionics_model_name(&selected.model);
    let selected_identifier = normalize_avionics_identifier(&selected.manufacturer_identifier);
    if candidates.iter().any(|candidate| {
        candidate.id != selected.id
            && ((normalize_avionics_manufacturer_name(&candidate.manufacturer)
                == selected_manufacturer
                && normalize_avionics_model_name(&candidate.model) == selected_model)
                || (!selected_identifier.is_empty()
                    && normalize_avionics_identifier(&candidate.manufacturer_identifier)
                        == selected_identifier))
    }) {
        return None;
    }

    Some(ApprovedAvionicsIdentity {
        id: selected.id,
        manufacturer: selected.manufacturer.clone(),
        model: selected.model.clone(),
        avionics_types: selected.avionics_types.clone(),
        manufacturer_identifier_kind: selected.manufacturer_identifier_kind.clone(),
        manufacturer_identifier: selected.manufacturer_identifier.clone(),
        evidence_url: selected.identity_source_url.clone(),
        evidence_title: selected.identity_source_title.clone(),
        evidence: selected.identity_evidence.clone(),
        reason: match_reason.to_string(),
        grounded_claim_source_urls: Vec::new(),
    })
}

/// An exact base label must not hide a longer catalog member that the retained
/// listing actually names. A merely possible omitted suffix does not defeat a
/// literal exact match; non-exact prefix/suffix observations continue to the
/// bounded variant path.
fn listing_names_longer_catalog_variant(
    selected: &KnownApprovedAvionicsCandidate,
    catalog: &[AvionicsCatalogCandidate],
    listing_context: &str,
) -> bool {
    let selected_manufacturer = normalize_avionics_manufacturer_name(&selected.manufacturer);
    catalog.iter().any(|candidate| {
        if candidate.id == selected.id
            || normalize_avionics_manufacturer_name(&candidate.manufacturer)
                != selected_manufacturer
        {
            return false;
        }
        let candidate_key = normalize_avionics_identifier(&candidate.model);
        candidate_key.len() > selected.canonical_product_key.len()
            && candidate_key.starts_with(&selected.canonical_product_key)
            && model_numeric_runs(&candidate_key)
                == model_numeric_runs(&selected.canonical_product_key)
            && exact_compact_identity_is_present(listing_context, &candidate_key)
    })
}

fn catalog_identity_is_duplicated(
    candidate: &KnownApprovedAvionicsCandidate,
    catalog: &[AvionicsCatalogCandidate],
) -> bool {
    let selected_keys = [
        candidate.canonical_product_key.as_str(),
        candidate.canonical_identifier_key.as_str(),
    ];
    catalog.iter().any(|other| {
        if other.id == candidate.id {
            return false;
        }
        [
            normalize_avionics_identifier(&other.model),
            normalize_avionics_identifier(&other.manufacturer_identifier),
        ]
        .into_iter()
        .filter(|key| !key.is_empty())
        .any(|key| {
            selected_keys
                .iter()
                .any(|selected_key| !selected_key.is_empty() && key == *selected_key)
        })
    })
}

fn stable_oem_identifier_has_placeholder(kind: &str, identifier: &str) -> bool {
    if !matches!(kind, "manufacturer_part_number" | "sku") {
        return false;
    }
    if identifier
        .chars()
        .any(|character| matches!(character, '*' | '?' | '#'))
    {
        return true;
    }
    identifier
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|segment| !segment.is_empty())
        .any(|segment| {
            let segment = segment.to_ascii_lowercase();
            (segment.len() >= 2 && segment.bytes().all(|byte| byte == b'x'))
                || matches!(
                    segment.as_str(),
                    "placeholder" | "tbd" | "unknown" | "varies" | "various"
                )
        })
}

fn has_distinct_exact_oem_part_or_sku(kind: &str, identifier: &str, product_key: &str) -> bool {
    if !matches!(kind, "manufacturer_part_number" | "sku")
        || stable_oem_identifier_has_placeholder(kind, identifier)
    {
        return false;
    }
    let identifier_key = normalize_avionics_identifier(identifier);
    !identifier_key.is_empty() && identifier_key != product_key
}

fn strict_product_prefix_collision(left: &str, right: &str) -> bool {
    left != right && (left.starts_with(right) || right.starts_with(left))
}

fn exact_token_phrase_is_present(text: &str, phrase: &str) -> bool {
    let text_tokens = exact_identity_tokens(text);
    let phrase_tokens = exact_identity_tokens(phrase);
    !phrase_tokens.is_empty()
        && text_tokens
            .windows(phrase_tokens.len())
            .any(|window| window == phrase_tokens)
}

fn exact_stable_identifier_is_present(text: &str, kind: &str, identifier: &str) -> bool {
    if !matches!(
        kind,
        "manufacturer_part_number" | "manufacturer_model_number" | "sku"
    ) {
        return false;
    }
    if identifier.is_empty()
        || identifier
            .chars()
            .any(|character| !character.is_ascii_lowercase() && !character.is_ascii_digit())
    {
        return false;
    }
    exact_compact_identity_is_present(text, identifier)
}

fn exact_compact_identity_is_present(text: &str, identity_key: &str) -> bool {
    !exact_compact_identity_occurrence_ranges(text, identity_key).is_empty()
}

fn exact_compact_identity_occurrence_ranges(text: &str, identity_key: &str) -> Vec<(usize, usize)> {
    if identity_key.is_empty() {
        return Vec::new();
    }
    let text_tokens = exact_identity_tokens(text);
    text_tokens
        .iter()
        .enumerate()
        .filter_map(|(start, _)| {
            let mut joined = String::new();
            for (offset, token) in text_tokens[start..].iter().enumerate() {
                joined.push_str(token);
                if joined == identity_key {
                    return Some((start, start + offset + 1));
                }
                if joined.len() >= identity_key.len() {
                    return None;
                }
            }
            None
        })
        .collect()
}

/// Require one exact excerpt to carry the complete product identity instead
/// of accepting model and identifier anchors found elsewhere on a
/// multi-product publisher page.
///
/// Punctuation and spacing may vary, but alphanumeric boundaries remain
/// significant: `GIA 63` cannot authorize `GIA 63W`.
pub(crate) fn exact_product_identity_signal_is_present(
    evidence: &str,
    canonical_model: &str,
    manufacturer_identifier: &str,
) -> bool {
    !normalize_avionics_identifier(manufacturer_identifier).is_empty()
        && direct_source_product_identity_signal_is_present(
            evidence,
            canonical_model,
            manufacturer_identifier,
        )
}

fn exact_identity_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            token.push(character);
        } else if !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

fn exact_manufacturer_name_key(value: &str) -> String {
    exact_identity_tokens(value)
        .into_iter()
        .filter(|token| {
            !matches!(
                token.as_str(),
                "co" | "company"
                    | "corp"
                    | "corporation"
                    | "inc"
                    | "incorporated"
                    | "llc"
                    | "ltd"
                    | "limited"
            )
        })
        .collect::<Vec<_>>()
        .concat()
}

fn catalog_candidates_from_rows(rows: Vec<CatalogRow>) -> Vec<AvionicsCatalogCandidate> {
    review_catalog_candidates_from_rows(rows)
        .into_iter()
        .map(|candidate| candidate.candidate)
        .collect()
}

/// Retain only products belonging to the observed manufacturer's curated
/// identity. Model numbers and part-number namespaces are manufacturer-scoped;
/// neither is globally unique.
///
/// Evidence-backed effective identities take precedence. A normalized-name
/// fallback is allowed only when either side has not yet entered the identity
/// graph, and therefore cannot bridge two different known identities.
fn manufacturer_scoped_catalog_candidates(
    manufacturer: &str,
    manufacturer_identity_id: Option<i64>,
    catalog: &[ReviewCatalogCandidate],
) -> Vec<AvionicsCatalogCandidate> {
    let manufacturer_key = normalize_avionics_manufacturer_name(manufacturer);
    catalog
        .iter()
        .filter(
            |candidate| match (manufacturer_identity_id, candidate.manufacturer_identity_id) {
                (Some(expected), Some(actual)) => expected == actual,
                _ => {
                    !manufacturer_key.is_empty()
                        && normalize_avionics_manufacturer_name(&candidate.candidate.manufacturer)
                            == manufacturer_key
                }
            },
        )
        .map(|candidate| candidate.candidate.clone())
        .collect()
}

fn manufacturer_collision_snapshot_sha256(
    manufacturer: &str,
    manufacturer_identity_id: Option<i64>,
    catalog: &[ReviewCatalogCandidate],
) -> String {
    let manufacturer_key = normalize_avionics_manufacturer_name(manufacturer);
    let mut rows = catalog
        .iter()
        .filter(
            |candidate| match (manufacturer_identity_id, candidate.manufacturer_identity_id) {
                (Some(expected), Some(actual)) => expected == actual,
                _ => {
                    !manufacturer_key.is_empty()
                        && normalize_avionics_manufacturer_name(&candidate.candidate.manufacturer)
                            == manufacturer_key
                }
            },
        )
        .map(|candidate| {
            let product = &candidate.candidate;
            json!({
                "id": product.id,
                "manufacturer": product.manufacturer,
                "model": product.model,
                "avionics_types": product.avionics_types,
                "manufacturer_identifier_kind": product.manufacturer_identifier_kind,
                "manufacturer_identifier": product.manufacturer_identifier,
                "catalog_status": product.catalog_status,
                "manufacturer_identity_id": candidate.manufacturer_identity_id,
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row["id"].as_i64().unwrap_or_default());
    let mut hasher = Sha256::new();
    hasher.update(b"aircost:avionics-manufacturer-collision-snapshot:v1");
    hasher.update(
        serde_json::to_vec(&rows).expect("manufacturer collision snapshot is serializable"),
    );
    format!("{:x}", hasher.finalize())
}

fn review_catalog_candidates_from_rows(rows: Vec<CatalogRow>) -> Vec<ReviewCatalogCandidate> {
    let mut by_id = HashMap::<i64, ReviewCatalogCandidate>::new();
    for row in rows
        .into_iter()
        .filter(|row| is_usable_avionics_label(&row.manufacturer, &row.model))
    {
        let capabilities = canonical_avionics_types_for_label(&row.capability_type);
        let candidate = by_id
            .entry(row.id)
            .or_insert_with(|| ReviewCatalogCandidate {
                candidate: AvionicsCatalogCandidate {
                    id: row.id,
                    manufacturer: row.manufacturer,
                    model: row.model,
                    avionics_types: Vec::new(),
                    manufacturer_identifier_kind: row
                        .manufacturer_identifier_kind
                        .unwrap_or_default(),
                    manufacturer_identifier: row.manufacturer_identifier.unwrap_or_default(),
                    catalog_status: row.catalog_status,
                },
                manufacturer_identity_id: row.avionics_manufacturer_identity_id,
            });
        candidate
            .candidate
            .avionics_types
            .extend(capabilities.into_iter().map(str::to_string));
    }
    let mut catalog = by_id.into_values().collect::<Vec<_>>();
    for candidate in &mut catalog {
        candidate.candidate.avionics_types =
            canonicalize_avionics_types(&candidate.candidate.avionics_types);
    }
    catalog.sort_by_key(|candidate| candidate.candidate.id);
    catalog
}

/// Map common capability labels to the server-owned taxonomy. This mapping is
/// used only for catalog loading and similarity retrieval. It must never be
/// used to turn a raw listing label into a product identity.
fn canonical_avionics_types_for_label(value: &str) -> Vec<&'static str> {
    let key = normalize_name(value);
    if let Some(canonical) = CURATED_AVIONICS_TYPES
        .iter()
        .copied()
        .find(|canonical| normalize_name(canonical) == key)
    {
        return vec![canonical];
    }
    match key.as_str() {
        "comm" | "communications radio" | "communication radio" => vec!["COM"],
        "navigation receiver" | "nav receiver" => vec!["NAV"],
        "nav com"
        | "nav comm"
        | "navcom"
        | "navigation communication"
        | "navigation communications" => {
            vec!["NAV", "COM"]
        }
        "automatic direction finder" => vec!["ADF"],
        "distance measuring equipment" | "distance measurement equipment" => vec!["DME"],
        "attitude heading reference system" | "attitude and heading reference system" => {
            vec!["AHRS"]
        }
        "adc" | "air data unit" => vec!["Air Data Computer"],
        "emergency locator transmitter" | "emergency locator beacon" => vec!["ELT"],
        "cdi"
        | "hsi"
        | "cdi hsi"
        | "course deviation indicator"
        | "horizontal situation indicator" => vec!["Navigation Indicator"],
        "stormscope" | "lightning detector" | "lightning detection system" => {
            vec!["Lightning Detection"]
        }
        "radio altimeter" => vec!["Radar Altimeter"],
        "clock" | "timer" | "chronometer" => vec!["Clock/Timer"],
        "taws" | "egpws" | "terrain awareness and warning system" => {
            vec!["Terrain Awareness"]
        }
        _ => Vec::new(),
    }
}

fn retrieval_avionics_type_keys(value: &str) -> Vec<String> {
    let canonical = canonical_avionics_types_for_label(value);
    if canonical.is_empty() {
        vec![normalize_name(value)]
    } else {
        canonical.into_iter().map(normalize_name).collect()
    }
}

fn canonicalize_avionics_types(values: &[String]) -> Vec<String> {
    let present = values
        .iter()
        .flat_map(|value| canonical_avionics_types_for_label(value))
        .collect::<HashSet<_>>();
    CURATED_AVIONICS_TYPES
        .iter()
        .copied()
        .filter(|value| present.contains(value))
        .map(str::to_string)
        .collect()
}

fn approved_capability_additions(
    existing: &AvionicsCatalogCandidate,
    proposed: &VerifiedIdentity,
) -> CatalogResult<Vec<String>> {
    for (field, actual, expected) in [
        (
            "canonical_manufacturer",
            proposed.canonical_manufacturer.as_str(),
            existing.manufacturer.as_str(),
        ),
        (
            "canonical_model",
            proposed.canonical_model.as_str(),
            existing.model.as_str(),
        ),
        (
            "manufacturer_identifier_kind",
            proposed.manufacturer_identifier_kind.as_str(),
            existing.manufacturer_identifier_kind.as_str(),
        ),
        (
            "manufacturer_identifier",
            proposed.manufacturer_identifier.as_str(),
            existing.manufacturer_identifier.as_str(),
        ),
    ] {
        if actual != expected {
            return Err(CatalogError::Validation(format!(
                "approved capability enrichment must preserve {field} exactly"
            )));
        }
    }

    let proposed_types = proposed
        .canonical_types
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if let Some(missing) = existing
        .avionics_types
        .iter()
        .find(|capability| !proposed_types.contains(capability.as_str()))
    {
        return Err(CatalogError::Validation(format!(
            "approved capability enrichment cannot remove stored capability {missing:?}"
        )));
    }
    let existing_types = existing
        .avionics_types
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    Ok(proposed
        .canonical_types
        .iter()
        .filter(|capability| !existing_types.contains(capability.as_str()))
        .cloned()
        .collect())
}

fn canonical_types_from_response(response: &Value, field: &str) -> CatalogResult<Vec<String>> {
    let values = response
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CatalogError::Validation(format!(
                "Gemini avionics identity response requires {field} to be an array"
            ))
        })?;
    let mut present = HashSet::new();
    for value in values {
        let capability = value.as_str().map(str::trim).ok_or_else(|| {
            CatalogError::Validation(format!(
                "Gemini avionics identity response {field} must contain only strings"
            ))
        })?;
        if !CURATED_AVIONICS_TYPES.contains(&capability) {
            return Err(CatalogError::Validation(format!(
                "unsupported canonical avionics capability {capability:?}"
            )));
        }
        present.insert(capability);
    }
    Ok(CURATED_AVIONICS_TYPES
        .iter()
        .copied()
        .filter(|capability| present.contains(capability))
        .map(str::to_string)
        .collect())
}

fn catalog_fingerprint(catalog: &[AvionicsCatalogCandidate]) -> String {
    let mut hasher = Sha256::new();
    for row in catalog {
        for value in [
            row.id.to_string(),
            row.manufacturer.clone(),
            row.model.clone(),
            row.avionics_types.join("\u{1f}"),
            row.manufacturer_identifier_kind.clone(),
            row.manufacturer_identifier.clone(),
            row.catalog_status.clone(),
        ] {
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

/// Return a model-only retrieval score when two labels carry the same product
/// identity signal.
///
/// Manufacturer and capability overlap must not admit a catalog row: those
/// broad signals made unrelated products from a large manufacturer look like
/// collision candidates. Typography-only equality is exact identity. A
/// family/variant relation is admitted only when the compact labels share a
/// real code prefix and every numeric run is identical. This retains `GIA 63`
/// beside `GIA 63W`, while keeping `GIA 64W` out of a `GIA 63W` review unless
/// an exact stable identifier independently connects it.
fn model_identity_relation_score(observed_model: &str, catalog_model: &str) -> Option<f64> {
    let observed_model = normalize_avionics_model_name(observed_model);
    let catalog_model = normalize_avionics_model_name(catalog_model);
    if observed_model.is_empty() || catalog_model.is_empty() {
        return None;
    }

    let observed_key = normalize_avionics_identifier(&observed_model);
    let catalog_key = normalize_avionics_identifier(&catalog_model);
    if observed_key == catalog_key {
        return Some(1.0);
    }

    let observed_numbers = model_numeric_runs(&observed_key);
    let catalog_numbers = model_numeric_runs(&catalog_key);
    if observed_numbers != catalog_numbers {
        return None;
    }

    let shorter_key_length = observed_key.len().min(catalog_key.len());
    let prefix_relation = shorter_key_length >= MINIMUM_PREFIX_MODEL_KEY_LENGTH
        && (observed_key.starts_with(&catalog_key) || catalog_key.starts_with(&observed_key));
    prefix_relation.then(|| string_similarity(&observed_model, &catalog_model))
}

fn model_numeric_runs(model_key: &str) -> Vec<&str> {
    let mut runs = Vec::new();
    let mut start = None;
    for (index, character) in model_key.char_indices() {
        if character.is_ascii_digit() {
            start.get_or_insert(index);
        } else if let Some(start) = start.take() {
            runs.push(&model_key[start..index]);
        }
    }
    if let Some(start) = start {
        runs.push(&model_key[start..]);
    }
    runs
}

fn shortlist_avionics_candidates(
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
    manufacturer_identifier: Option<&str>,
    catalog: &[AvionicsCatalogCandidate],
) -> Vec<AvionicsCatalogCandidate> {
    shortlist_avionics_candidates_with_limit(
        manufacturer,
        model,
        avionics_types,
        manufacturer_identifier,
        catalog,
        CANDIDATE_LIMIT,
    )
}

fn shortlist_avionics_candidates_with_limit(
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
    manufacturer_identifier: Option<&str>,
    catalog: &[AvionicsCatalogCandidate],
    limit: usize,
) -> Vec<AvionicsCatalogCandidate> {
    let observed_identifier =
        normalize_avionics_identifier(manufacturer_identifier.unwrap_or_default());
    let manufacturer_key = normalize_avionics_manufacturer_name(manufacturer);
    let type_keys = avionics_types
        .iter()
        .flat_map(|value| retrieval_avionics_type_keys(value))
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    let mut scored = catalog
        .iter()
        .filter_map(|candidate| {
            let identifier = normalize_avionics_identifier(&candidate.manufacturer_identifier);
            let identifier_match = !observed_identifier.is_empty()
                && matches!(
                    candidate.manufacturer_identifier_kind.as_str(),
                    "manufacturer_part_number" | "manufacturer_model_number" | "sku"
                )
                && observed_identifier == identifier;
            let model_score = model_identity_relation_score(model, &candidate.model);
            if model_score.is_none() && !identifier_match {
                return None;
            }

            let manufacturer_match =
                normalize_avionics_manufacturer_name(&candidate.manufacturer) == manufacturer_key;
            // Capability similarity affects retrieval rank only. It is never
            // a product identity key or evidence for a same-product decision.
            let type_match = candidate
                .avionics_types
                .iter()
                .flat_map(|value| retrieval_avionics_type_keys(value))
                .any(|value| type_keys.contains(&value));
            let score = model_score.unwrap_or(0.0)
                + if manufacturer_match { 0.18 } else { 0.0 }
                + if type_match { 0.08 } else { 0.0 }
                + if identifier_match { 0.75 } else { 0.0 };
            Some((score, candidate.id, candidate.clone()))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, candidate)| candidate)
        .collect()
}

fn expanded_collision_context(
    classification_context: &AvionicsUnitResolutionContext,
    proposed: &VerifiedIdentity,
    catalog: &[AvionicsCatalogCandidate],
) -> AvionicsUnitResolutionContext {
    // Grounding often supplies a clean canonical label and official part
    // number that were absent from the listing. Re-run retrieval with that new
    // evidence before the independent collision decision.
    let mut candidates = shortlist_avionics_candidates_with_limit(
        &proposed.canonical_manufacturer,
        &proposed.canonical_model,
        &proposed.canonical_types,
        Some(&proposed.manufacturer_identifier),
        catalog,
        COLLISION_CANDIDATE_LIMIT,
    );
    for candidate in &classification_context.catalog_candidates {
        if candidates.iter().all(|item| item.id != candidate.id) {
            candidates.push(candidate.clone());
        }
    }
    candidates.truncate(COLLISION_CANDIDATE_LIMIT);
    let mut context = classification_context.clone();
    context.catalog_candidates = candidates;
    context
}

fn grounded_response_has_verified_evidence(response: &GroundedJsonResponse) -> bool {
    response
        .verified_evidence
        .as_ref()
        .is_some_and(|evidence| evidence.audit.used_verified_evidence)
}

fn string_similarity(left: &str, right: &str) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    if left == right {
        return 1.0;
    }
    let left_tokens = left.split_whitespace().collect::<HashSet<_>>();
    let right_tokens = right.split_whitespace().collect::<HashSet<_>>();
    let intersection = left_tokens.intersection(&right_tokens).count() as f64;
    let union = left_tokens.union(&right_tokens).count() as f64;
    let token_score = if union > 0.0 {
        intersection / union
    } else {
        0.0
    };
    token_score.max(bigram_dice(left, right))
}

fn bigram_dice(left: &str, right: &str) -> f64 {
    fn bigrams(value: &str) -> HashMap<(char, char), usize> {
        let characters = value.chars().collect::<Vec<_>>();
        let mut output = HashMap::new();
        for window in characters.windows(2) {
            *output.entry((window[0], window[1])).or_insert(0) += 1;
        }
        output
    }
    if left.len() < 2 || right.len() < 2 {
        return f64::from(left == right);
    }
    let left_bigrams = bigrams(left);
    let right_bigrams = bigrams(right);
    let overlap = left_bigrams
        .iter()
        .map(|(bigram, count)| count.min(right_bigrams.get(bigram).unwrap_or(&0)))
        .sum::<usize>();
    let total = left_bigrams.values().sum::<usize>() + right_bigrams.values().sum::<usize>();
    2.0 * overlap as f64 / total as f64
}

fn resolution_issues(
    context: &AvionicsUnitResolutionContext,
    response: &Value,
    verified_grounding_used: bool,
    grounding_sources: &[GeminiGroundingSource],
    grounding_supports: &[GeminiGroundingSupport],
) -> Vec<String> {
    resolution_issues_with_direct_source_proofs(
        context,
        response,
        verified_grounding_used,
        grounding_sources,
        grounding_supports,
        &[],
    )
}

fn resolution_issues_with_direct_source_proofs(
    context: &AvionicsUnitResolutionContext,
    response: &Value,
    verified_grounding_used: bool,
    grounding_sources: &[GeminiGroundingSource],
    grounding_supports: &[GeminiGroundingSupport],
    source_evidence_proofs: &[SourceEvidenceProof],
) -> Vec<String> {
    let mut issues = Vec::new();
    let status = string_field(response, "status");
    let catalog_id = response
        .get("catalog_id")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let confidence = string_field(response, "confidence");
    let reason = string_field(response, "reason");
    let rejection_basis = string_field(response, "rejection_basis");
    if !matches!(confidence, "very_high" | "high" | "medium" | "low") {
        issues.push("confidence must be very_high, high, medium, or low".to_string());
    }
    if reason.is_empty() {
        issues.push("reason must be non-empty".to_string());
    }
    if status != "reject" && rejection_basis != NO_REJECTION_BASIS {
        issues.push(format!("{status} must use rejection_basis=none"));
    }
    match status {
        "existing_match" => {
            if string_field(response, "manufacturer_identifier_scope")
                != EXACT_CATALOG_PRODUCT_IDENTIFIER_SCOPE
            {
                issues.push(
                    "existing_match requires manufacturer_identifier_scope=exact_catalog_product"
                        .to_string(),
                );
            }
            if !verified_grounding_used {
                issues.push(
                    "existing_match requires verified Search + URL Context or authoritative direct-source + URL Context evidence"
                        .to_string(),
                );
            }
            let selected = context
                .catalog_candidates
                .iter()
                .find(|candidate| candidate.id == catalog_id);
            let Some(selected) = selected else {
                issues.push("existing_match must select one supplied catalog id".to_string());
                return issues;
            };
            if !matches!(confidence, "very_high" | "high") {
                issues.push("existing_match requires high or very_high confidence".to_string());
            }
            if selected.catalog_status == "unreviewed" && confidence != "very_high" {
                issues
                    .push("an unreviewed existing_match requires very_high confidence".to_string());
            }
            if selected.catalog_status == "approved" {
                for (field, expected) in [
                    ("canonical_manufacturer", selected.manufacturer.as_str()),
                    ("canonical_model", selected.model.as_str()),
                    (
                        "manufacturer_identifier_kind",
                        selected.manufacturer_identifier_kind.as_str(),
                    ),
                    (
                        "manufacturer_identifier",
                        selected.manufacturer_identifier.as_str(),
                    ),
                ] {
                    if string_field(response, field) != expected {
                        issues.push(format!(
                            "approved existing_match must repeat {field} exactly"
                        ));
                    }
                }
                match canonical_types_from_response(response, "canonical_types") {
                    Ok(types) => {
                        let returned = types.iter().map(String::as_str).collect::<HashSet<_>>();
                        if let Some(missing) = selected
                            .avionics_types
                            .iter()
                            .find(|capability| !returned.contains(capability.as_str()))
                        {
                            issues.push(format!(
                                "approved existing_match cannot remove stored capability {missing:?}"
                            ));
                        }
                        let stored = selected
                            .avionics_types
                            .iter()
                            .map(String::as_str)
                            .collect::<HashSet<_>>();
                        for observed in
                            canonicalize_avionics_types(&context.candidate.avionics_types)
                                .into_iter()
                                .filter(|capability| !stored.contains(capability.as_str()))
                        {
                            if !returned.contains(observed.as_str()) {
                                issues.push(format!(
                                    "approved existing_match must include the verified newly observed capability {observed:?} or return unresolved"
                                ));
                            }
                        }
                    }
                    Err(error) => issues.push(error.to_string()),
                }
            } else {
                if let Err(error) = verified_identity_from_response(response) {
                    issues.push(error.to_string());
                }
                let old_identifier =
                    normalize_avionics_identifier(&selected.manufacturer_identifier);
                let proposed_identifier = normalize_avionics_identifier(string_field(
                    response,
                    "manufacturer_identifier",
                ));
                if !old_identifier.is_empty() && old_identifier != proposed_identifier {
                    issues.push(
                        "unreviewed existing_match cannot overwrite a conflicting legacy manufacturer identifier"
                            .to_string(),
                    );
                }
                if !selected.manufacturer_identifier_kind.is_empty()
                    && selected.manufacturer_identifier_kind != "none"
                    && string_field(response, "manufacturer_identifier_kind")
                        != selected.manufacturer_identifier_kind
                {
                    issues.push(
                        "unreviewed existing_match cannot change the kind of a non-empty legacy manufacturer identifier"
                            .to_string(),
                    );
                }
            }
            validate_authoritative_evidence(
                response,
                &context.source_url,
                source_evidence_proofs,
                &mut issues,
            );
        }
        "propose_new" => {
            if string_field(response, "manufacturer_identifier_scope")
                != EXACT_CATALOG_PRODUCT_IDENTIFIER_SCOPE
            {
                issues.push(
                    "propose_new requires manufacturer_identifier_scope=exact_catalog_product"
                        .to_string(),
                );
            }
            if !verified_grounding_used {
                issues.push(
                    "propose_new requires verified Search + URL Context or authoritative direct-source + URL Context evidence"
                        .to_string(),
                );
            }
            if catalog_id != 0 {
                issues.push("propose_new must use catalog_id=0".to_string());
            }
            if confidence != "very_high" {
                issues.push("propose_new requires very_high confidence".to_string());
            }
            if let Err(error) = verified_identity_from_response(response) {
                issues.push(error.to_string());
            }
            validate_authoritative_evidence(
                response,
                &context.source_url,
                source_evidence_proofs,
                &mut issues,
            );
        }
        "reject" | "unresolved" => {
            if catalog_id != 0 {
                issues.push(format!("{status} must use catalog_id=0"));
            }
            if status == "reject" && !matches!(confidence, "very_high" | "high") {
                issues.push("reject requires high or very_high confidence".to_string());
            }
            if status == "reject" && !REJECTION_BASES.contains(&rejection_basis) {
                issues.push(format!(
                    "reject requires rejection_basis to be one of {}",
                    REJECTION_BASES.join(", ")
                ));
            }
            if status == "reject" && !rejection_reason_matches_basis(reason, rejection_basis) {
                issues.push(
                    "reject reason must state a negative claim consistent with rejection_basis"
                        .to_string(),
                );
            }
            if status == "reject" && !candidate_labels_are_present_in_claim(context, reason) {
                issues.push(
                    "reject reason must explicitly name the observed model and its usable manufacturer"
                        .to_string(),
                );
            }
            if status == "reject"
                && !has_claim_bound_verified_grounding(
                    context,
                    response,
                    verified_grounding_used,
                    grounding_sources,
                    grounding_supports,
                )
            {
                issues.push(
                    "reject requires verified grounding with the candidate-specific negative reason contained in one linked cited support span"
                        .to_string(),
                );
            }
            for field in [
                "canonical_manufacturer",
                "canonical_model",
                "manufacturer_identifier",
                "identity_source_url",
                "identity_source_title",
                "identity_evidence",
            ] {
                if !string_field(response, field).is_empty() {
                    issues.push(format!("{status} must leave {field} empty"));
                }
            }
            match canonical_types_from_response(response, "canonical_types") {
                Ok(types) if types.is_empty() => {}
                Ok(_) => issues.push(format!("{status} must leave canonical_types empty")),
                Err(error) => issues.push(error.to_string()),
            }
            if string_field(response, "manufacturer_identifier_kind") != "none" {
                issues.push(format!(
                    "{status} must use manufacturer_identifier_kind=none"
                ));
            }
            if string_field(response, "manufacturer_identifier_scope") != NO_IDENTIFIER_SCOPE {
                issues.push(format!(
                    "{status} must use manufacturer_identifier_scope=none"
                ));
            }
        }
        _ => issues
            .push("status must be existing_match, propose_new, reject, or unresolved".to_string()),
    }
    issues
}

fn has_claim_bound_verified_grounding(
    context: &AvionicsUnitResolutionContext,
    response: &Value,
    verified_grounding_used: bool,
    grounding_sources: &[GeminiGroundingSource],
    grounding_supports: &[GeminiGroundingSupport],
) -> bool {
    let rejection_basis = string_field(response, "rejection_basis");
    let reason = string_field(response, "reason");
    if !REJECTION_BASES.contains(&rejection_basis)
        || !rejection_reason_matches_basis(reason, rejection_basis)
        || !candidate_labels_are_present_in_claim(context, reason)
    {
        return false;
    }
    verified_grounding_used
        && grounding_supports.iter().any(|support| {
            !support.text.trim().is_empty()
                && normalized_word_phrase_is_present(&support.text, reason)
                && support.source_indices.iter().any(|source_index| {
                    grounding_sources.iter().any(|source| {
                        source.chunk_index == *source_index && !source.url.trim().is_empty()
                    })
                })
        })
}

fn candidate_labels_are_present_in_claim(
    context: &AvionicsUnitResolutionContext,
    claim: &str,
) -> bool {
    normalized_model_phrase_is_present(claim, &context.candidate.model)
        && (is_generic_avionics_manufacturer_name(&context.candidate.manufacturer)
            || normalized_manufacturer_phrase_is_present(claim, &context.candidate.manufacturer))
}

fn rejection_reason_matches_basis(reason: &str, rejection_basis: &str) -> bool {
    let required_phrases: &[&str] = match rejection_basis {
        "generic_or_class_only" => &[
            "is a generic",
            "generic or class only",
            "is class only",
            "is an equipment class",
            "not a concrete product",
            "does not identify a concrete product",
        ],
        "feature_only" => &[
            "feature only",
            "capability only",
            "is a feature",
            "is a capability",
            "does not identify hardware",
        ],
        "not_installed_equipment" => &[
            "not installed equipment",
            "not an installed unit",
            "is not installed",
            "was not installed",
            "does not identify installed equipment",
        ],
        "demonstrably_nonexistent" => &[
            "demonstrably nonexistent",
            "does not exist",
            "no such product exists",
            "not a real product",
            "nonexistent product",
        ],
        _ => return false,
    };
    required_phrases
        .iter()
        .any(|phrase| normalized_word_phrase_is_present(reason, phrase))
}

fn normalized_word_phrase_is_present(text: &str, phrase: &str) -> bool {
    let normalized_text = normalize_name(text);
    let normalized_phrase = normalize_name(phrase);
    let text_tokens = normalized_text.split_whitespace().collect::<Vec<_>>();
    let phrase_tokens = normalized_phrase.split_whitespace().collect::<Vec<_>>();
    !phrase_tokens.is_empty()
        && text_tokens
            .windows(phrase_tokens.len())
            .any(|window| window == phrase_tokens)
}

fn normalized_model_phrase_is_present(text: &str, model: &str) -> bool {
    let normalized_text = normalize_avionics_model_name(text);
    let normalized_model = normalize_avionics_model_name(model);
    let text_tokens = normalized_text.split_whitespace().collect::<Vec<_>>();
    let model_tokens = normalized_model.split_whitespace().collect::<Vec<_>>();
    !model_tokens.is_empty()
        && text_tokens
            .windows(model_tokens.len())
            .any(|window| window == model_tokens)
}

fn normalized_manufacturer_phrase_is_present(text: &str, manufacturer: &str) -> bool {
    let expected = normalize_avionics_manufacturer_name(manufacturer);
    if expected.is_empty() {
        return false;
    }
    let normalized_text = normalize_name(text);
    let text_tokens = normalized_text.split_whitespace().collect::<Vec<_>>();
    (1..=text_tokens.len().min(4)).any(|window_size| {
        text_tokens
            .windows(window_size)
            .any(|window| window.concat() == expected)
    })
}

/// Map a non-positive Gemini response while keeping rejection fail-closed.
///
/// `resolution_issues` normally prevents an ungrounded reject from reaching
/// this point. Rechecking here ensures future callers cannot accidentally turn
/// an ungrounded model response into an automatic listing discard.
fn nonpositive_identity_outcome(
    context: &AvionicsUnitResolutionContext,
    response: &Value,
    verified_grounding_used: bool,
    grounding_sources: &[GeminiGroundingSource],
    grounding_supports: &[GeminiGroundingSupport],
) -> Option<AvionicsIdentityOutcome> {
    let reason = string_field(response, "reason").to_string();
    match string_field(response, "status") {
        "reject"
            if resolution_issues(
                context,
                response,
                verified_grounding_used,
                grounding_sources,
                grounding_supports,
            )
            .is_empty() =>
        {
            Some(AvionicsIdentityOutcome::Rejected { reason })
        }
        "reject" => Some(AvionicsIdentityOutcome::Unresolved {
            reason: "Gemini's reject decision was not accepted because its structured negative claim was not bound to candidate-specific cited grounding evidence"
                .to_string(),
        }),
        "unresolved" => Some(AvionicsIdentityOutcome::Unresolved { reason }),
        _ => None,
    }
}

fn verified_identity_from_response(response: &Value) -> CatalogResult<VerifiedIdentity> {
    let identity = VerifiedIdentity {
        canonical_manufacturer: required_field(response, "canonical_manufacturer")?,
        canonical_model: required_field(response, "canonical_model")?,
        canonical_types: canonical_types_from_response(response, "canonical_types")?,
        manufacturer_identifier_kind: required_field(response, "manufacturer_identifier_kind")?,
        manufacturer_identifier: required_field(response, "manufacturer_identifier")?,
        manufacturer_identifier_scope: required_field(response, "manufacturer_identifier_scope")?,
        identity_source_url: required_field(response, "identity_source_url")?,
        identity_source_title: required_field(response, "identity_source_title")?,
        identity_evidence: required_field(response, "identity_evidence")?,
        reason: required_field(response, "reason")?,
        grounded_claim_source_urls: vec![required_field(response, "identity_source_url")?],
    };
    if !matches!(
        identity.manufacturer_identifier_kind.as_str(),
        "manufacturer_part_number" | "manufacturer_model_number" | "sku"
    ) {
        return Err(CatalogError::Validation(
            "verified identity requires manufacturer_part_number, manufacturer_model_number, or sku"
                .to_string(),
        ));
    }
    if identity.manufacturer_identifier_scope != EXACT_CATALOG_PRODUCT_IDENTIFIER_SCOPE {
        return Err(CatalogError::Validation(
            "verified identity requires manufacturer_identifier_scope=exact_catalog_product; a component, approval/article, family/series, or unknown identifier cannot identify the catalog product"
                .to_string(),
        ));
    }
    if !is_usable_avionics_label(&identity.canonical_manufacturer, &identity.canonical_model) {
        return Err(CatalogError::Validation(
            "verified identity must name one concrete manufacturer and model".to_string(),
        ));
    }
    if identity.canonical_types.is_empty() {
        return Err(CatalogError::Validation(
            "verified identity requires at least one canonical avionics capability".to_string(),
        ));
    }
    let normalized_model = normalize_avionics_model_name(&identity.canonical_model);
    if identity
        .canonical_types
        .iter()
        .any(|capability| normalized_model == normalize_name(capability))
        || normalized_model
            .split_whitespace()
            .any(|token| matches!(token, "series" | "family"))
        || combines_multiple_model_numbers(&identity.canonical_model)
    {
        return Err(CatalogError::Validation(
            "verified identity must describe one exact product or suite generation, not a class, family, series, or combined model label"
                .to_string(),
        ));
    }
    if normalize_avionics_identifier(&identity.manufacturer_identifier).is_empty() {
        return Err(CatalogError::Validation(
            "verified identity has an unusable manufacturer identifier".to_string(),
        ));
    }
    if normalize_avionics_identifier(&identity.manufacturer_identifier)
        == normalize_avionics_identifier(&identity.canonical_model)
        && identity.manufacturer_identifier_kind != "manufacturer_model_number"
    {
        return Err(CatalogError::Validation(
            "an identifier equal to the canonical model must use manufacturer_model_number"
                .to_string(),
        ));
    }
    Ok(identity)
}

fn combines_multiple_model_numbers(value: &str) -> bool {
    if !value.contains('/') {
        return false;
    }
    let mut numeric_groups = 0;
    let mut in_digits = false;
    for character in value.chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                numeric_groups += 1;
                in_digits = true;
            }
        } else {
            in_digits = false;
        }
    }
    numeric_groups > 1
}

fn collision_response_issues(
    context: &AvionicsUnitResolutionContext,
    proposed: &VerifiedIdentity,
    response: &Value,
    source_evidence_proofs: &[SourceEvidenceProof],
) -> Vec<String> {
    match proposal_attestation_with_direct_source_proofs(
        context,
        proposed,
        response,
        source_evidence_proofs,
    ) {
        Err(error) => vec![error.to_string()],
        Ok(attestation) if attestation.confirmed => {
            collision_reviews_with_direct_source_proofs(context, response, source_evidence_proofs)
                .err()
                .map(|error| vec![error.to_string()])
                .unwrap_or_default()
        }
        Ok(_) => Vec::new(),
    }
}

fn proposal_attestation_with_direct_source_proofs(
    context: &AvionicsUnitResolutionContext,
    proposed: &VerifiedIdentity,
    response: &Value,
    source_evidence_proofs: &[SourceEvidenceProof],
) -> CatalogResult<ProposalAttestation> {
    for (field, expected) in [
        (
            "canonical_manufacturer",
            proposed.canonical_manufacturer.as_str(),
        ),
        ("canonical_model", proposed.canonical_model.as_str()),
        (
            "manufacturer_identifier_kind",
            proposed.manufacturer_identifier_kind.as_str(),
        ),
        (
            "manufacturer_identifier",
            proposed.manufacturer_identifier.as_str(),
        ),
    ] {
        if string_field(response, field) != expected {
            return Err(CatalogError::Validation(format!(
                "independent proposal review must repeat {field} exactly"
            )));
        }
    }
    if canonical_types_from_response(response, "canonical_types")? != proposed.canonical_types {
        return Err(CatalogError::Validation(
            "independent proposal review must repeat canonical_types exactly".to_string(),
        ));
    }
    let decision = required_field(response, "proposal_decision")?;
    let reason = required_field(response, "proposal_reason")?;
    let manufacturer_identifier_scope =
        required_field(response, "proposal_manufacturer_identifier_scope")?;
    if !MANUFACTURER_IDENTIFIER_SCOPES.contains(&manufacturer_identifier_scope.as_str()) {
        return Err(CatalogError::Validation(format!(
            "unexpected proposal_manufacturer_identifier_scope {manufacturer_identifier_scope}"
        )));
    }
    if decision == "not_confirmed" {
        return Ok(ProposalAttestation {
            confirmed: false,
            source_url: String::new(),
            source_title: String::new(),
            evidence: String::new(),
            reason,
        });
    }
    if decision != "confirmed_same_as_input" {
        return Err(CatalogError::Validation(format!(
            "unexpected proposal_decision {decision}"
        )));
    }
    if manufacturer_identifier_scope != EXACT_CATALOG_PRODUCT_IDENTIFIER_SCOPE {
        return Err(CatalogError::Validation(
            "confirmed proposal review requires proposal_manufacturer_identifier_scope=exact_catalog_product; a component identifier cannot attest the complete proposed catalog product"
                .to_string(),
        ));
    }
    if string_field(response, "proposal_confidence") != "very_high" {
        return Err(CatalogError::Validation(
            "confirmed proposal review requires very_high confidence".to_string(),
        ));
    }
    if !is_usable_avionics_label(&context.candidate.manufacturer, &context.candidate.model) {
        return Err(CatalogError::Validation(
            "generic raw listing labels cannot receive a confirmed catalog identity".to_string(),
        ));
    }
    if context.requires_listing_evidence {
        let input_evidence = required_field(response, "input_evidence_text")?;
        if !context.listing_context.contains(&input_evidence) {
            return Err(CatalogError::Validation(
                "input_evidence_text must be copied exactly from listing_context".to_string(),
            ));
        }
        let raw_model_key = normalize_avionics_identifier(&context.candidate.model);
        if !exact_compact_identity_is_present(&input_evidence, &raw_model_key) {
            return Err(CatalogError::Validation(
                "listing input evidence does not contain the complete discriminating raw model label at alphanumeric boundaries"
                    .to_string(),
            ));
        }
        let canonical_model_key = normalize_avionics_identifier(&proposed.canonical_model);
        if !exact_compact_identity_is_present(&input_evidence, &canonical_model_key) {
            return Err(CatalogError::Validation(
                "listing input evidence does not contain the complete proposed canonical model at alphanumeric boundaries"
                    .to_string(),
            ));
        }
    }

    let source_url = required_field(response, "proposal_source_url")?;
    let source_title = required_field(response, "proposal_source_title")?;
    let evidence = required_field(response, "proposal_evidence")?;
    let mut issues = Vec::new();
    validate_evidence_values(
        &source_url,
        &source_title,
        &evidence,
        proposed.canonical_manufacturer.as_str(),
        proposed.canonical_model.as_str(),
        proposed.manufacturer_identifier.as_str(),
        &context.source_url,
        source_evidence_proofs,
        &mut issues,
    );
    if !issues.is_empty() {
        return Err(CatalogError::Validation(format!(
            "independent proposal attestation lacks grounded authoritative evidence: {}",
            issues.join("; ")
        )));
    }
    Ok(ProposalAttestation {
        confirmed: true,
        source_url,
        source_title,
        evidence,
        reason,
    })
}

fn collision_reviews_with_direct_source_proofs(
    context: &AvionicsUnitResolutionContext,
    response: &Value,
    source_evidence_proofs: &[SourceEvidenceProof],
) -> CatalogResult<Vec<CollisionReview>> {
    let values = response
        .get("reviews")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CatalogError::Validation("collision response missing reviews".to_string())
        })?;
    let expected = context
        .catalog_candidates
        .iter()
        .map(|candidate| candidate.id)
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut reviews = Vec::with_capacity(values.len());
    for value in values {
        let catalog_id = value
            .get("catalog_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                CatalogError::Validation(
                    "collision review catalog_id must be an integer".to_string(),
                )
            })?;
        if !expected.contains(&catalog_id) {
            return Err(CatalogError::Validation(format!(
                "collision review returned unknown catalog id {catalog_id}"
            )));
        }
        if !seen.insert(catalog_id) {
            return Err(CatalogError::Validation(format!(
                "collision review repeated catalog id {catalog_id}"
            )));
        }
        let decision = required_field(value, "decision")?;
        if !matches!(decision.as_str(), "same_product" | "different_product") {
            return Err(CatalogError::Validation(format!(
                "collision review {catalog_id} has invalid decision {decision}"
            )));
        }
        if string_field(value, "confidence") != "very_high" {
            return Err(CatalogError::Validation(format!(
                "collision review {catalog_id} must have very_high confidence before catalog storage"
            )));
        }
        let candidate_source_url = required_field(value, "candidate_source_url")?;
        let candidate_source_title = required_field(value, "candidate_source_title")?;
        let candidate_evidence = required_field(value, "candidate_evidence")?;
        let reason = required_field(value, "reason")?;
        let reviewed_candidate = context
            .catalog_candidates
            .iter()
            .find(|candidate| candidate.id == catalog_id)
            .expect("collision review catalog_id membership was checked above");
        validate_collision_decision_relation(response, reviewed_candidate, &decision)?;
        let mut issues = Vec::new();
        validate_evidence_values(
            &candidate_source_url,
            &candidate_source_title,
            &candidate_evidence,
            &reviewed_candidate.manufacturer,
            &reviewed_candidate.model,
            &reviewed_candidate.manufacturer_identifier,
            &context.source_url,
            source_evidence_proofs,
            &mut issues,
        );
        if !issues.is_empty() {
            return Err(CatalogError::Validation(format!(
                "collision review {catalog_id} lacks authoritative candidate evidence: {}",
                issues.join("; ")
            )));
        }
        reviews.push(CollisionReview {
            catalog_id,
            decision,
            candidate_source_url,
            candidate_source_title,
            candidate_evidence,
            reason,
        });
    }
    if seen != expected {
        let mut missing = expected.difference(&seen).copied().collect::<Vec<_>>();
        missing.sort_unstable();
        return Err(CatalogError::Validation(format!(
            "collision review omitted shortlisted catalog ids {missing:?}"
        )));
    }
    Ok(reviews)
}

fn validate_collision_decision_relation(
    response: &Value,
    candidate: &AvionicsCatalogCandidate,
    decision: &str,
) -> CatalogResult<()> {
    let proposal_manufacturer_key =
        normalize_avionics_manufacturer_name(string_field(response, "canonical_manufacturer"));
    let candidate_manufacturer_key = normalize_avionics_manufacturer_name(&candidate.manufacturer);
    let exact_manufacturer_match = !proposal_manufacturer_key.is_empty()
        && proposal_manufacturer_key == candidate_manufacturer_key;
    let proposal_model_key =
        normalize_avionics_identifier(string_field(response, "canonical_model"));
    let candidate_model_key = normalize_avionics_identifier(&candidate.model);
    let proposal_identifier_key =
        normalize_avionics_identifier(string_field(response, "manufacturer_identifier"));
    let candidate_identifier_key =
        normalize_avionics_identifier(&candidate.manufacturer_identifier);
    let exact_identifier_match = !proposal_identifier_key.is_empty()
        && !candidate_identifier_key.is_empty()
        && string_field(response, "manufacturer_identifier_kind")
            == candidate.manufacturer_identifier_kind
        && proposal_identifier_key == candidate_identifier_key;
    match decision {
        "same_product" => {
            let supported = exact_manufacturer_match
                && if candidate_identifier_key.is_empty() {
                    !proposal_model_key.is_empty() && proposal_model_key == candidate_model_key
                } else {
                    exact_identifier_match
                };
            if !supported {
                return Err(CatalogError::Validation(format!(
                    "collision review {} cannot claim same_product without the same manufacturer and an exact stable-identifier match, or an exact normalized model match for a legacy candidate without an identifier",
                    candidate.id
                )));
            }
        }
        "different_product" => {
            let contradicts_proven_identity = exact_manufacturer_match
                && (exact_identifier_match
                    || (candidate_identifier_key.is_empty()
                        && !proposal_model_key.is_empty()
                        && proposal_model_key == candidate_model_key));
            if contradicts_proven_identity {
                return Err(CatalogError::Validation(format!(
                    "collision review {} cannot claim different_product for independently proven identities with the same exact catalog signal",
                    candidate.id
                )));
            }
        }
        _ => unreachable!("collision decision enum was checked before relation validation"),
    }
    Ok(())
}

fn validate_authoritative_evidence(
    response: &Value,
    listing_source_url: &str,
    source_evidence_proofs: &[SourceEvidenceProof],
    issues: &mut Vec<String>,
) {
    validate_evidence_values(
        string_field(response, "identity_source_url"),
        string_field(response, "identity_source_title"),
        string_field(response, "identity_evidence"),
        string_field(response, "canonical_manufacturer"),
        string_field(response, "canonical_model"),
        string_field(response, "manufacturer_identifier"),
        listing_source_url,
        source_evidence_proofs,
        issues,
    );
}

fn validate_evidence_values(
    source_url: &str,
    source_title: &str,
    evidence: &str,
    canonical_manufacturer: &str,
    canonical_model: &str,
    manufacturer_identifier: &str,
    listing_source_url: &str,
    source_evidence_proofs: &[SourceEvidenceProof],
    issues: &mut Vec<String>,
) {
    let parsed = url::Url::parse(source_url).ok();
    if parsed.as_ref().is_none_or(|url| url.scheme() != "https") {
        issues.push("identity source must use its final HTTPS URL".to_string());
    }
    let lowered = source_url.to_ascii_lowercase();
    if [
        "/listing/",
        "/listings/",
        "/aircraft-for-sale/",
        "/classifieds/",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
    {
        issues.push("ordinary sale listings are not authoritative identity evidence".to_string());
    }
    let host = parsed
        .as_ref()
        .and_then(|url| url.host_str())
        .unwrap_or_default()
        .trim_start_matches("www.");
    if [
        "ebay.com",
        "amazon.com",
        "facebook.com",
        "craigslist.org",
        "controller.com",
        "trade-a-plane.com",
        "barnstormers.com",
        "aircraft.com",
        "globalair.com",
    ]
    .iter()
    .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        issues.push(
            "marketplace or broker pages are not authoritative identity evidence".to_string(),
        );
    }
    if !listing_source_url.trim().is_empty() && source_url.trim() == listing_source_url.trim() {
        issues.push("listing source URL cannot also be identity evidence".to_string());
    }
    if source_title.trim().chars().count() < 4 {
        issues.push("identity source title must be specific and non-empty".to_string());
    }
    let evidence_character_count = evidence.trim().chars().count();
    let model_key = normalize_avionics_identifier(canonical_model);
    let identifier_key = normalize_avionics_identifier(manufacturer_identifier);
    let manufacturer_key = normalize_avionics_identifier(canonical_manufacturer);
    let is_specific_short_model_designation = evidence_character_count >= 12
        && !model_key.is_empty()
        && model_key == identifier_key
        && !manufacturer_key.is_empty()
        && exact_compact_identity_is_present(evidence, &manufacturer_key);
    if evidence_character_count < 20 && !is_specific_short_model_designation {
        issues.push(
            "identity evidence must contain a specific supporting fact; a short model-designation row must also name the canonical manufacturer"
                .to_string(),
        );
    }
    if !exact_evidence_identity_signal_is_present(
        evidence,
        canonical_model,
        manufacturer_identifier,
    ) {
        if normalize_avionics_identifier(manufacturer_identifier).is_empty() {
            issues.push(
                "identity evidence must itself contain the complete canonical model at alphanumeric boundaries"
                    .to_string(),
            );
        } else {
            issues.push(
                "identity evidence must itself contain the complete canonical model and manufacturer identifier at alphanumeric boundaries"
                    .to_string(),
            );
        }
    }
    if !evidence_is_bound_to_direct_source_proof(
        source_url,
        evidence,
        canonical_model,
        manufacturer_identifier,
        source_evidence_proofs,
    ) {
        issues.push(
            "identity evidence must be an exact server-fetched publisher span bound to the final source URL and content digest"
                .to_string(),
        );
    }
}

fn evidence_is_bound_to_direct_source_proof(
    source_url: &str,
    evidence: &str,
    canonical_model: &str,
    manufacturer_identifier: &str,
    source_evidence_proofs: &[SourceEvidenceProof],
) -> bool {
    exact_evidence_identity_signal_is_present(evidence, canonical_model, manufacturer_identifier)
        && source_evidence_proofs.iter().any(|proof| {
            proof.content_sha256.len() == 64
                && proof
                    .content_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && proof.matches_excerpt(source_url, evidence)
        })
}

fn exact_evidence_identity_signal_is_present(
    evidence: &str,
    canonical_model: &str,
    manufacturer_identifier: &str,
) -> bool {
    let identifier_key = normalize_avionics_identifier(manufacturer_identifier);
    if identifier_key.is_empty() {
        let model_key = normalize_avionics_identifier(canonical_model);
        !model_key.is_empty() && exact_compact_identity_is_present(evidence, &model_key)
    } else {
        exact_product_identity_signal_is_present(evidence, canonical_model, manufacturer_identifier)
    }
}

fn approved_identity_from_verified(
    id: i64,
    identity: &VerifiedIdentity,
) -> ApprovedAvionicsIdentity {
    ApprovedAvionicsIdentity {
        id,
        manufacturer: identity.canonical_manufacturer.clone(),
        model: identity.canonical_model.clone(),
        avionics_types: identity.canonical_types.clone(),
        manufacturer_identifier_kind: identity.manufacturer_identifier_kind.clone(),
        manufacturer_identifier: identity.manufacturer_identifier.clone(),
        evidence_url: identity.identity_source_url.clone(),
        evidence_title: identity.identity_source_title.clone(),
        evidence: identity.identity_evidence.clone(),
        reason: identity.reason.clone(),
        grounded_claim_source_urls: identity.grounded_claim_source_urls.clone(),
    }
}

/// Persist fresh authoritative evidence and the current-policy reuse conclusion
/// for an immutable approved product after a full identity/collision review.
///
/// Canonical identity and capabilities are compared under the catalog lock.
/// Only then is the non-identity evidence refreshed and its positive cache
/// fingerprint computed in the same transaction.
async fn persist_existing_reuse_attestation(
    db: &AppDb,
    reviewed_existing: &AvionicsCatalogCandidate,
    identity: &VerifiedIdentity,
    reviewed_catalog_fingerprint: &str,
) -> CatalogResult<()> {
    let additions = approved_capability_additions(reviewed_existing, identity)?;
    if !additions.is_empty() {
        return Err(CatalogError::Validation(
            "existing-product reuse attestation cannot skip reviewed capability enrichment"
                .to_string(),
        ));
    }
    let catalog_lock_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            "LOCK TABLE avionics_models, avionics_model_types, avionics_types, avionics_manufacturers, avionics_approved_product_identities, avionics_product_reuse_attestations, avionics_authoritative_source_origins, avionics_authoritative_source_origin_revocations IN SHARE ROW EXCLUSIVE MODE",
        ),
    };

    macro_rules! attest {
        ($pool:expr, $refresh_grounded:path) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&catalog_lock_sql)
                .execute(&mut *transaction)
                .await?;
            let catalog_select_sql = db.sql(CATALOG_SELECT_SQL);
            let current_rows = sqlx::query_as::<_, CatalogRow>(&catalog_select_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let current_catalog = catalog_candidates_from_rows(current_rows);
            if catalog_fingerprint(&current_catalog) != reviewed_catalog_fingerprint {
                return Err(CatalogError::Validation(
                    "avionics catalog changed during Gemini reuse review; retry against the current catalog"
                        .to_string(),
                ));
            }
            let current = current_catalog
                .iter()
                .find(|candidate| candidate.id == reviewed_existing.id)
                .ok_or_else(|| {
                    CatalogError::Validation(format!(
                        "approved catalog id {} disappeared during reuse review",
                        reviewed_existing.id
                    ))
                })?;
            if current.catalog_status != "approved"
                || !approved_capability_additions(current, identity)?.is_empty()
            {
                return Err(CatalogError::Validation(
                    "approved catalog identity changed during Gemini reuse review; retry against the current catalog"
                        .to_string(),
                ));
            }
            let reuse_attested = $refresh_grounded(
                db,
                &mut transaction,
                reviewed_existing.id,
                identity.identity_source_url.as_str(),
                identity.identity_source_title.as_str(),
                identity.identity_evidence.as_str(),
            )
            .await?;
            if !reuse_attested {
                return Err(CatalogError::Validation(format!(
                    "approved catalog id {} could not be bound to a current active exact manufacturer source origin",
                    reviewed_existing.id
                )));
            }
            transaction.commit().await?;
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            attest!(pool, refresh_grounded_evidence_and_reuse_attestation_sqlite)
        }
        DatabaseBackend::Postgres(pool) => attest!(
            pool,
            refresh_grounded_evidence_and_reuse_attestation_postgres
        ),
    }
    Ok(())
}

async fn persist_approved_capability_enrichment(
    db: &AppDb,
    reviewed_existing: &AvionicsCatalogCandidate,
    identity: &VerifiedIdentity,
    reviewed_catalog_fingerprint: &str,
) -> CatalogResult<ApprovedAvionicsIdentity> {
    let reviewed_additions = approved_capability_additions(reviewed_existing, identity)?;
    if reviewed_additions.is_empty() {
        return Ok(approved_identity_from_verified(
            reviewed_existing.id,
            identity,
        ));
    }
    let evidence_origins = std::iter::once(&identity.identity_source_url)
        .chain(identity.grounded_claim_source_urls.iter())
        .map(|source_url| {
            canonical_exact_https_origin(source_url).map_err(manufacturer_origin_error)
        })
        .collect::<CatalogResult<BTreeSet<_>>>()?;
    let catalog_lock_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            "LOCK TABLE avionics_models, avionics_model_types, avionics_types, avionics_manufacturers, avionics_approved_product_identities, avionics_product_reuse_attestations, avionics_authoritative_source_origins, avionics_authoritative_source_origin_revocations IN SHARE ROW EXCLUSIVE MODE",
        ),
    };
    let revoked_source_sql = db.sql(
        r#"SELECT EXISTS (
             SELECT 1
             FROM avionics_authoritative_source_origins source_origin
             JOIN avionics_authoritative_source_origin_revocations revocation
               ON revocation.avionics_authoritative_source_origin_id =
                  source_origin.id
             WHERE source_origin.https_origin = ?
           )"#,
    );

    macro_rules! enrich {
        ($pool:expr, $refresh_grounded:path) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&catalog_lock_sql)
                .execute(&mut *transaction)
                .await?;
            for evidence_origin in &evidence_origins {
                let evidence_origin_revoked = match db.backend() {
                    DatabaseBackend::Sqlite(_) => {
                        sqlx::query_scalar::<_, i64>(&revoked_source_sql)
                            .bind(evidence_origin.as_str())
                            .fetch_one(&mut *transaction)
                            .await?
                            != 0
                    }
                    DatabaseBackend::Postgres(_) => {
                        sqlx::query_scalar::<_, bool>(&revoked_source_sql)
                            .bind(evidence_origin.as_str())
                            .fetch_one(&mut *transaction)
                            .await?
                    }
                };
                if evidence_origin_revoked {
                    return Err(CatalogError::Validation(format!(
                        "capability evidence origin {evidence_origin:?} was revoked before persistence"
                    )));
                }
            }

            let catalog_select_sql = db.sql(CATALOG_SELECT_SQL);
            let current_rows = sqlx::query_as::<_, CatalogRow>(&catalog_select_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let current_catalog = catalog_candidates_from_rows(current_rows);
            if catalog_fingerprint(&current_catalog) != reviewed_catalog_fingerprint {
                return Err(CatalogError::Validation(
                    "avionics catalog changed during Gemini capability review; retry against the current catalog"
                        .to_string(),
                ));
            }
            let current = current_catalog
                .iter()
                .find(|candidate| candidate.id == reviewed_existing.id)
                .ok_or_else(|| {
                    CatalogError::Validation(format!(
                        "approved catalog id {} disappeared during capability review",
                        reviewed_existing.id
                    ))
                })?;
            if current.catalog_status != "approved" {
                return Err(CatalogError::Validation(format!(
                    "catalog id {} is no longer approved; retry capability review",
                    reviewed_existing.id
                )));
            }
            let additions = approved_capability_additions(current, identity)?;
            if additions.is_empty() {
                return Err(CatalogError::Validation(
                    "approved catalog capabilities changed during Gemini review; retry against the current catalog"
                        .to_string(),
                ));
            }

            for capability in &additions {
                let type_key = normalize_name(capability);
                let insert_type = db.sql(
                    "INSERT INTO avionics_types (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
                );
                sqlx::query(&insert_type)
                    .bind(capability.as_str())
                    .bind(type_key.as_str())
                    .execute(&mut *transaction)
                    .await?;
                let select_type =
                    db.sql("SELECT id FROM avionics_types WHERE normalized_name = ?");
                let type_id: i64 = sqlx::query_scalar(&select_type)
                    .bind(type_key.as_str())
                    .fetch_one(&mut *transaction)
                    .await?;
                let insert_membership = db.sql(
                    "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?) ON CONFLICT (avionics_model_id, avionics_type_id) DO NOTHING",
                );
                sqlx::query(&insert_membership)
                    .bind(reviewed_existing.id)
                    .bind(type_id)
                    .execute(&mut *transaction)
                    .await?;
            }

            let touch_model = db.sql(
                "UPDATE avionics_models SET updated_at = CURRENT_TIMESTAMP WHERE id = ? AND catalog_status = 'approved'",
            );
            let touched = sqlx::query(&touch_model)
                .bind(reviewed_existing.id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if touched != 1 {
                return Err(CatalogError::Validation(
                    "approved catalog identity changed during capability enrichment".to_string(),
                ));
            }
            let reuse_attested = $refresh_grounded(
                db,
                &mut transaction,
                reviewed_existing.id,
                identity.identity_source_url.as_str(),
                identity.identity_source_title.as_str(),
                identity.identity_evidence.as_str(),
            )
            .await?;
            if !reuse_attested {
                return Err(CatalogError::Validation(format!(
                    "approved catalog id {} capability enrichment could not be bound to a current active exact manufacturer source origin",
                    reviewed_existing.id
                )));
            }
            transaction.commit().await?;
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            enrich!(pool, refresh_grounded_evidence_and_reuse_attestation_sqlite)
        }
        DatabaseBackend::Postgres(pool) => enrich!(
            pool,
            refresh_grounded_evidence_and_reuse_attestation_postgres
        ),
    }
    Ok(approved_identity_from_verified(
        reviewed_existing.id,
        identity,
    ))
}

async fn persist_approved_identity(
    db: &AppDb,
    requested_target_id: Option<i64>,
    confirmed_same_ids: &[i64],
    identity: &VerifiedIdentity,
    reviewed_catalog_fingerprint: &str,
) -> CatalogResult<ApprovedAvionicsIdentity> {
    if identity.canonical_types.is_empty() {
        return Err(CatalogError::Validation(
            "cannot persist an avionics product without a canonical capability".to_string(),
        ));
    }
    let distinct_confirmed_ids = confirmed_same_ids.iter().copied().collect::<HashSet<_>>();
    if distinct_confirmed_ids.len() > 1 {
        let mut ids = distinct_confirmed_ids.iter().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        return Err(CatalogError::Validation(format!(
            "Gemini confirmed multiple legacy catalog rows as the same product ({ids:?}); run explicit avionics consolidation before approving this identity"
        )));
    }
    let model_key = normalize_avionics_model_name(&identity.canonical_model);
    let identifier_key = normalize_avionics_identifier(&identity.manufacturer_identifier);
    let allowed_ids = distinct_confirmed_ids;
    let catalog_lock_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => {
            db.sql(
                "LOCK TABLE avionics_models, avionics_model_types, avionics_types, avionics_manufacturers, avionics_manufacturer_canonical_keys, avionics_manufacturer_identities, avionics_manufacturer_identity_memberships, avionics_manufacturer_identity_merges, avionics_manufacturer_alias_candidates, avionics_approved_product_identities IN SHARE ROW EXCLUSIVE MODE",
            )
        }
    };

    macro_rules! persist {
        (
            $pool:expr,
            $admit_manufacturer:path,
            $refresh_reuse:path
        ) => {{
            let mut transaction = $pool.begin().await?;
            // Serialize catalog approvals, then prove that Gemini reviewed the
            // same active identity snapshot that is about to be mutated.
            sqlx::query(&catalog_lock_sql)
                .execute(&mut *transaction)
                .await?;
            let catalog_select_sql = db.sql(CATALOG_SELECT_SQL);
            let current_rows = sqlx::query_as::<_, CatalogRow>(&catalog_select_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let current_fingerprint =
                catalog_fingerprint(&catalog_candidates_from_rows(current_rows));
            if current_fingerprint != reviewed_catalog_fingerprint {
                return Err(CatalogError::Validation(
                    "avionics catalog changed during Gemini review; retry against the current catalog"
                        .to_string(),
                ));
            }
            let manufacturer_admission = ManufacturerProductAdmission {
                manufacturer: identity.canonical_manufacturer.as_str(),
                model: identity.canonical_model.as_str(),
                manufacturer_identifier_kind: identity.manufacturer_identifier_kind.as_str(),
                manufacturer_identifier: identity.manufacturer_identifier.as_str(),
                evidence: ManufacturerIdentityEvidence {
                    source_url: identity.identity_source_url.clone(),
                    source_title: identity.identity_source_title.clone(),
                    evidence_text: identity.identity_evidence.clone(),
                },
                additional_evidence_source_urls: &identity.grounded_claim_source_urls,
            };
            let manufacturer_scope = match $admit_manufacturer(
                db,
                &mut transaction,
                &manufacturer_admission,
            )
            .await
            .map_err(manufacturer_origin_error)?
            {
                    ManufacturerProductAdmissionOutcome::Admitted(scope) => scope,
                    ManufacturerProductAdmissionOutcome::PendingAliasReview => {
                        transaction.commit().await?;
                        return Err(CatalogError::Validation(
                            "manufacturer identity requires human alias review before this product can be approved"
                                .to_string(),
                        ));
                    }
                };
            let manufacturer_id = manufacturer_scope.avionics_manufacturer_id;

            let mut type_ids = Vec::with_capacity(identity.canonical_types.len());
            for canonical_type in &identity.canonical_types {
                let type_key = normalize_name(canonical_type);
                let insert_type = db.sql(
                    "INSERT INTO avionics_types (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
                );
                sqlx::query(&insert_type)
                    .bind(canonical_type.trim())
                    .bind(type_key.as_str())
                    .execute(&mut *transaction)
                    .await?;
                let select_type =
                    db.sql("SELECT id FROM avionics_types WHERE normalized_name = ?");
                let type_id: i64 = sqlx::query_scalar(&select_type)
                    .bind(type_key.as_str())
                    .fetch_one(&mut *transaction)
                    .await?;
                type_ids.push(type_id);
            }

            let select_identifier_collision = db.sql(
                "SELECT id FROM avionics_models WHERE avionics_manufacturer_id = ? AND manufacturer_identifier_kind = ? AND normalized_manufacturer_identifier = ? ORDER BY id LIMIT 1",
            );
            let identifier_collision: Option<i64> = sqlx::query_scalar(&select_identifier_collision)
                .bind(manufacturer_id)
                .bind(identity.manufacturer_identifier_kind.as_str())
                .bind(identifier_key.as_str())
                .fetch_optional(&mut *transaction)
                .await?;
            let select_name_collision = db.sql(
                "SELECT id FROM avionics_models WHERE avionics_manufacturer_id = ? AND normalized_name = ? ORDER BY id LIMIT 1",
            );
            let name_collision: Option<i64> = sqlx::query_scalar(&select_name_collision)
                .bind(manufacturer_id)
                .bind(model_key.as_str())
                .fetch_optional(&mut *transaction)
                .await?;

            for collision in [identifier_collision, name_collision].into_iter().flatten() {
                if Some(collision) != requested_target_id && !allowed_ids.contains(&collision) {
                    return Err(CatalogError::Validation(format!(
                        "catalog changed during review: unreviewed collision with catalog id {collision}; Gemini must re-evaluate"
                    )));
                }
            }
            let mut target_id = requested_target_id;
            if let (Some(identifier_id), Some(name_id)) = (identifier_collision, name_collision) {
                if identifier_id != name_id {
                    return Err(CatalogError::Validation(format!(
                        "verified identifier and canonical name collide with different legacy rows ({identifier_id}, {name_id}); explicit duplicate merge is required"
                    )));
                }
                target_id = Some(identifier_id);
            } else if let Some(collision_id) = identifier_collision.or(name_collision) {
                // The independent review confirmed this collision as the same
                // product. Promote the row already owning the verified key so
                // canonicalization cannot violate a uniqueness constraint.
                target_id = Some(collision_id);
            }

            let stored_id = if let Some(target_id) = target_id {
                let target_check = db.sql(
                    "SELECT catalog_status, normalized_manufacturer_identifier FROM avionics_models WHERE id = ?",
                );
                let target_state: Option<(String, Option<String>)> = sqlx::query_as(&target_check)
                    .bind(target_id)
                    .fetch_optional(&mut *transaction)
                    .await?;
                let target_status = target_state.as_ref().map(|state| state.0.as_str());
                match target_status {
                    Some("unreviewed") => {}
                    Some("approved") => {
                        return Err(CatalogError::Validation(format!(
                            "catalog id {target_id} became approved during review; retry identity resolution"
                        )));
                    }
                    Some("rejected") => {
                        return Err(CatalogError::Validation(format!(
                            "catalog id {target_id} was rejected during review"
                        )));
                    }
                    _ => {
                        return Err(CatalogError::Validation(format!(
                            "catalog id {target_id} disappeared during review"
                        )));
                    }
                }
                if target_state
                    .as_ref()
                    .and_then(|state| state.1.as_deref())
                    .is_some_and(|existing| {
                        !existing.is_empty() && existing != identifier_key.as_str()
                    })
                {
                    return Err(CatalogError::Validation(format!(
                        "catalog id {target_id} has a conflicting legacy manufacturer identifier; explicit adjudication is required"
                    )));
                }
                let update = db.sql(
                    r#"
                    UPDATE avionics_models
                    SET
                      avionics_manufacturer_id = ?,
                      name = ?,
                      normalized_name = ?,
                      manufacturer_identifier_kind = ?,
                      manufacturer_identifier = ?,
                      normalized_manufacturer_identifier = ?,
                      identity_source_url = ?,
                      identity_source_title = ?,
                      identity_evidence_text = ?,
                      identity_evidence_kind = 'authoritative_reference',
                      identity_confidence = 'very_high',
                      catalog_reviewed_at = CURRENT_TIMESTAMP,
                      introduced_year = NULL,
                      discontinued_year = NULL,
                      estimated_unit_value_usd = NULL,
                      value_basis = 'unreviewed',
                      replacement_cost_usd = NULL,
                      value_reference_year = NULL,
                      value_source = NULL,
                      valuation_scope = 'unit',
                      updated_at = CURRENT_TIMESTAMP
                    WHERE id = ? AND catalog_status = 'unreviewed'
                    "#,
                );
                let updated = sqlx::query(&update)
                    .bind(manufacturer_id)
                    .bind(identity.canonical_model.trim())
                    .bind(model_key.as_str())
                    .bind(identity.manufacturer_identifier_kind.as_str())
                    .bind(identity.manufacturer_identifier.trim())
                    .bind(identifier_key.as_str())
                    .bind(identity.identity_source_url.as_str())
                    .bind(identity.identity_source_title.as_str())
                    .bind(identity.identity_evidence.as_str())
                    .bind(target_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if updated != 1 {
                    return Err(CatalogError::Validation(
                        "catalog entry changed while it was being approved; retry identity resolution".to_string(),
                    ));
                }
                // Legacy value and suite metadata were not part of the
                // identity review. Remove the graph as well as the numeric
                // fields above so catalog approval cannot bless stale value
                // assumptions by association.
                let delete_suite_memberships = db.sql(
                    "DELETE FROM avionics_suite_components WHERE suite_model_id = ? OR component_model_id = ?",
                );
                sqlx::query(&delete_suite_memberships)
                    .bind(target_id)
                    .bind(target_id)
                    .execute(&mut *transaction)
                    .await?;
                target_id
            } else {
                let insert = db.sql(
                    r#"
                    INSERT INTO avionics_models (
                      avionics_manufacturer_id,
                      name,
                      normalized_name,
                      catalog_status,
                      manufacturer_identifier_kind,
                      manufacturer_identifier,
                      normalized_manufacturer_identifier,
                      identity_source_url,
                      identity_source_title,
                      identity_evidence_text,
                      identity_evidence_kind,
                      identity_confidence,
                      catalog_reviewed_at
                    )
                    VALUES (?, ?, ?, 'unreviewed', ?, ?, ?, ?, ?, ?, 'authoritative_reference', 'very_high', CURRENT_TIMESTAMP)
                    "#,
                );
                sqlx::query(&insert)
                    .bind(manufacturer_id)
                    .bind(identity.canonical_model.trim())
                    .bind(model_key.as_str())
                    .bind(identity.manufacturer_identifier_kind.as_str())
                    .bind(identity.manufacturer_identifier.trim())
                    .bind(identifier_key.as_str())
                    .bind(identity.identity_source_url.as_str())
                    .bind(identity.identity_source_title.as_str())
                    .bind(identity.identity_evidence.as_str())
                    .execute(&mut *transaction)
                    .await?;
                let select = db.sql(
                    "SELECT id FROM avionics_models WHERE avionics_manufacturer_id = ? AND manufacturer_identifier_kind = ? AND normalized_manufacturer_identifier = ?",
                );
                sqlx::query_scalar(&select)
                    .bind(manufacturer_id)
                    .bind(identity.manufacturer_identifier_kind.as_str())
                    .bind(identifier_key.as_str())
                    .fetch_one(&mut *transaction)
                    .await?
            };
            for type_id in &type_ids {
                let insert_membership = db.sql(
                    "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?) ON CONFLICT (avionics_model_id, avionics_type_id) DO NOTHING",
                );
                sqlx::query(&insert_membership)
                    .bind(stored_id)
                    .bind(*type_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            let select_existing_types = db.sql(
                "SELECT avionics_type_id FROM avionics_model_types WHERE avionics_model_id = ?",
            );
            let existing_type_ids: Vec<i64> = sqlx::query_scalar(&select_existing_types)
                .bind(stored_id)
                .fetch_all(&mut *transaction)
                .await?;
            let desired_type_ids = type_ids.iter().copied().collect::<HashSet<_>>();
            for stale_type_id in existing_type_ids
                .into_iter()
                .filter(|type_id| !desired_type_ids.contains(type_id))
            {
                let delete_membership = db.sql(
                    "DELETE FROM avionics_model_types WHERE avionics_model_id = ? AND avionics_type_id = ?",
                );
                sqlx::query(&delete_membership)
                    .bind(stored_id)
                    .bind(stale_type_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            let approve = db.sql(
                r#"
                UPDATE avionics_models
                SET catalog_status = 'approved',
                    catalog_reviewed_at = CURRENT_TIMESTAMP,
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = ?
                  AND catalog_status = 'unreviewed'
                  AND EXISTS (
                    SELECT 1
                    FROM avionics_model_types model_type
                    WHERE model_type.avionics_model_id = avionics_models.id
                  )
                "#,
            );
            let approved = sqlx::query(&approve)
                .bind(stored_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if approved != 1 {
                return Err(CatalogError::Validation(
                    "catalog product could not be approved with its verified capabilities"
                        .to_string(),
                ));
            }
            let stage_exact_aliases = db.sql(
                r#"INSERT INTO avionics_manufacturer_alias_candidates (
                     avionics_manufacturer_id,
                     candidate_manufacturer_identity_id,
                     candidate_basis, matched_avionics_model_id,
                     reason, confidence
                   )
                   SELECT DISTINCT
                     signal.right_avionics_manufacturer_id,
                     current_identity.avionics_manufacturer_identity_id,
                     signal.candidate_basis,
                     signal.left_avionics_model_id,
                     CASE signal.candidate_basis
                       WHEN 'exact_stable_identifier' THEN
                         'A catalog product under this unassigned raw maker shares an exact nonblank manufacturer identifier kind and value with a product belonging to the evidence-backed candidate identity.'
                       ELSE
                         'A catalog product under this unassigned raw maker shares an exact canonical product name with a product belonging to the evidence-backed candidate identity.'
                     END,
                     CASE signal.candidate_basis
                       WHEN 'exact_stable_identifier' THEN 'high'
                       ELSE 'medium'
                     END
                   FROM avionics_manufacturer_effective_memberships current_identity
                   JOIN avionics_legacy_manufacturer_alias_signals signal
                     ON signal.left_avionics_manufacturer_id
                       = current_identity.avionics_manufacturer_id
                   LEFT JOIN avionics_manufacturer_identity_memberships other_membership
                     ON other_membership.avionics_manufacturer_id
                       = signal.right_avionics_manufacturer_id
                   WHERE current_identity.avionics_manufacturer_id = ?
                     AND other_membership.avionics_manufacturer_id IS NULL
                   ON CONFLICT DO NOTHING"#,
            );
            sqlx::query(&stage_exact_aliases)
                .bind(manufacturer_id)
                .execute(&mut *transaction)
                .await?;
            let reuse_attested = $refresh_reuse(
                db,
                &mut transaction,
                stored_id,
                identity.identity_source_url.as_str(),
            )
            .await?;
            if !reuse_attested {
                return Err(CatalogError::Validation(format!(
                    "newly approved catalog id {stored_id} could not be bound to a current active exact manufacturer source origin"
                )));
            }
            transaction.commit().await?;
            stored_id
        }};
    }

    let stored_id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            persist!(
                pool,
                admit_manufacturer_product_scope_sqlite,
                refresh_reuse_attestation_sqlite
            )
        }
        DatabaseBackend::Postgres(pool) => {
            persist!(
                pool,
                admit_manufacturer_product_scope_postgres,
                refresh_reuse_attestation_postgres
            )
        }
    };
    Ok(approved_identity_from_verified(stored_id, identity))
}

fn string_field<'a>(value: &'a Value, field: &str) -> &'a str {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
}

fn required_field(value: &Value, field: &str) -> CatalogResult<String> {
    let value = string_field(value, field);
    if value.is_empty() {
        return Err(CatalogError::Validation(format!(
            "Gemini avionics identity response missing {field}"
        )));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use url::Url;

    use super::{
        approved_candidate_adjudication_plan, approved_candidate_adjudication_selection,
        attest_grounded_existing_avionics_identity, attest_pending_review_product_identity,
        canonical_avionics_types_for_label, canonical_types_from_response, catalog_fingerprint,
        collision_correction_plan, collision_response_issues,
        collision_reviews_with_direct_source_proofs,
        deterministic_graph_approved_identity_from_source,
        evidence_is_bound_to_direct_source_proof, exact_compact_identity_is_present,
        exact_product_identity_signal_is_present, expanded_collision_context,
        explicit_authoritative_direct_source_plan, known_approved_local_match,
        load_catalog_candidates, load_known_approved_candidates, load_review_catalog_candidates,
        manufacturer_collision_snapshot_sha256, manufacturer_scoped_catalog_candidates,
        model_identity_relation_score, nonpositive_identity_outcome,
        opportunistic_authoritative_direct_source_plan, persist_approved_capability_enrichment,
        persist_approved_identity, persist_existing_reuse_attestation,
        proposal_attestation_with_direct_source_proofs,
        require_response_evidence_source_urls_not_revoked, resolution_issues,
        resolution_issues_with_direct_source_proofs, resolve_verified_local_avionics_identity,
        revalidate_direct_source_admission_state, select_opportunistic_authoritative_source_urls,
        select_unique_exact_review_candidate, shortlist_avionics_candidates,
        should_run_listing_only_approved_candidate_adjudication,
        stable_oem_identifier_has_placeholder, validate_authorized_direct_source_response,
        validate_collision_decision_relation, validate_evidence_values,
        verified_identity_from_response, ApprovedAvionicsIdentity,
        ApprovedAvionicsProductSourceRequest, ApprovedProductSourceVerification,
        AuthoritativeDirectSourcePlan, AuthoritativeSourceHintRow, AvionicsCatalogCandidate,
        AvionicsIdentityOutcome, AvionicsIdentityRequest, AvionicsUnitResolutionCandidate,
        AvionicsUnitResolutionContext, CollisionCorrectionPlan, DirectSourceRequirement,
        GeminiGroundingSource, GeminiGroundingSupport, GroundedJsonResponse, IdentityGroundingPlan,
        KnownApprovedAvionicsCandidate, PendingProductAttestationCommitGuard,
        ReviewCatalogCandidate, VerifiedIdentity, COLLISION_STRUCTURE_CALL_BUDGET,
        KNOWN_APPROVED_SELECT_SQL,
    };
    use crate::avionics::manufacturer::{
        ensure_manufacturer_identity, ManufacturerIdentityEvidence,
        ManufacturerSourceOriginAdmission,
    };
    use crate::db::{AppDb, DatabaseBackend};
    use crate::gemini::curation::workflow::{
        SourceEvidenceProof, SourceEvidenceSpanProof, MAX_EXACT_PRODUCT_SIGNAL_TOKEN_SPAN,
    };
    use crate::gemini::interactions::FetchedSourceDocument;
    use crate::gemini::source::{TextRow, TextRowKind};
    use crate::html::clean::normalize_source_evidence_span;
    use crate::normalize::{
        normalize_avionics_identifier, normalize_avionics_manufacturer_name,
        normalize_avionics_model_name,
    };

    fn candidate(id: i64, model: &str, status: &str) -> AvionicsCatalogCandidate {
        AvionicsCatalogCandidate {
            id,
            manufacturer: "Garmin".to_string(),
            model: model.to_string(),
            avionics_types: vec!["Transponder".to_string()],
            manufacturer_identifier_kind: "manufacturer_part_number".to_string(),
            manufacturer_identifier: format!("011-TEST-{id}"),
            catalog_status: status.to_string(),
        }
    }

    fn review_candidate(
        candidate: AvionicsCatalogCandidate,
        manufacturer_identity_id: Option<i64>,
    ) -> ReviewCatalogCandidate {
        ReviewCatalogCandidate {
            candidate,
            manufacturer_identity_id,
        }
    }

    fn review_candidate_with_types(id: i64, model: &str, types: &[&str]) -> ReviewCatalogCandidate {
        let mut candidate = candidate(id, model, "unreviewed");
        candidate.avionics_types = types.iter().map(|value| (*value).to_string()).collect();
        review_candidate(candidate, Some(1))
    }

    fn direct_source_proof(source_url: &str, evidence: &str) -> SourceEvidenceProof {
        let normalized_span = normalize_source_evidence_span(evidence);
        SourceEvidenceProof {
            final_url: source_url.to_string(),
            content_sha256: "d".repeat(64),
            evidence_spans: vec![SourceEvidenceSpanProof {
                span_sha256: format!("{:x}", Sha256::digest(normalized_span.as_bytes())),
                normalized_span,
            }],
        }
    }

    fn direct_source_response(final_urls: &[&str]) -> GroundedJsonResponse {
        GroundedJsonResponse {
            value: json!({}),
            google_search_used: false,
            url_context_used: false,
            authoritative_direct_source_verified: true,
            authoritative_direct_source_final_urls: final_urls
                .iter()
                .map(|url| (*url).to_string())
                .collect(),
            grounding_sources: Vec::new(),
            grounding_supports: Vec::new(),
            source_evidence_proofs: Vec::new(),
            interaction_audits: Vec::new(),
            verified_evidence: None,
        }
    }

    #[test]
    fn collision_domain_correction_cannot_exceed_shared_structure_budget() {
        assert_eq!(
            collision_correction_plan(1).unwrap(),
            CollisionCorrectionPlan::CorrectOnce
        );
        assert_eq!(
            collision_correction_plan(COLLISION_STRUCTURE_CALL_BUDGET).unwrap(),
            CollisionCorrectionPlan::BudgetExhausted
        );
        for invalid in [0, COLLISION_STRUCTURE_CALL_BUDGET + 1, 4] {
            assert!(collision_correction_plan(invalid).is_err());
        }
    }

    #[test]
    fn review_retrieval_surfaces_only_one_exact_model_candidate() {
        let exact = candidate(1, "GPS 150", "unreviewed");
        let neighbor = candidate(2, "GPS 150XL", "approved");
        let selected = select_unique_exact_review_candidate(
            "Garmin",
            "GPS-150",
            &["Transponder".to_string()],
            Some(1),
            &[
                review_candidate(exact.clone(), Some(1)),
                review_candidate(neighbor, Some(1)),
            ],
        )
        .unwrap();
        assert_eq!(selected.id, exact.id);
        assert_eq!(selected.catalog_status, "unreviewed");

        let mut historical_maker = exact.clone();
        historical_maker.id = 3;
        historical_maker.manufacturer = "King Radio".to_string();
        let selected = select_unique_exact_review_candidate(
            "Garmin",
            "GPS 150",
            &["Transponder".to_string()],
            Some(1),
            &[
                review_candidate(exact.clone(), Some(1)),
                review_candidate(historical_maker.clone(), Some(2)),
            ],
        )
        .unwrap();
        assert_eq!(selected.id, exact.id);

        assert!(
            select_unique_exact_review_candidate(
                "Unknown historical maker",
                "GPS 150",
                &["Transponder".to_string()],
                None,
                &[
                    review_candidate(exact, Some(1)),
                    review_candidate(historical_maker, Some(2)),
                ],
            )
            .is_none(),
            "an unknown maker must not break a globally ambiguous exact-model tie"
        );
    }

    #[test]
    fn review_retrieval_rejects_split_gia63w_before_capability_filtering() {
        let catalog = [
            review_candidate_with_types(4, "GIA-63W", &["NAV", "COM"]),
            review_candidate_with_types(239, "GIA63W", &["GPS"]),
        ];

        assert!(select_unique_exact_review_candidate(
            "Garmin",
            "GIA63W",
            &["GPS".to_string()],
            Some(1),
            &catalog,
        )
        .is_none());
        assert!(select_unique_exact_review_candidate(
            "Garmin",
            "GIA-63W",
            &["NAV".to_string(), "COM".to_string()],
            Some(1),
            &catalog,
        )
        .is_none());
    }

    #[test]
    fn review_retrieval_rejects_split_g1000_before_capability_filtering() {
        let catalog = [
            review_candidate_with_types(703, "G1000", &["Integrated Flight Deck"]),
            review_candidate_with_types(718, "G1000", &["GPS"]),
        ];

        assert!(select_unique_exact_review_candidate(
            "Garmin",
            "G1000",
            &["Integrated Flight Deck".to_string()],
            Some(1),
            &catalog,
        )
        .is_none());
    }

    #[test]
    fn review_retrieval_rejects_split_gi275_before_capability_filtering() {
        let catalog = [
            review_candidate_with_types(20, "GI 275", &["Flight Display"]),
            review_candidate_with_types(233, "GI275", &["Standby Instrument"]),
        ];

        assert!(select_unique_exact_review_candidate(
            "Garmin",
            "GI275",
            &["Flight Display".to_string()],
            Some(1),
            &catalog,
        )
        .is_none());
    }

    #[test]
    fn review_retrieval_preserves_unique_kx_170b_for_unscoped_king_label() {
        let mut kx_170b = candidate(51, "KX-170B", "approved");
        kx_170b.manufacturer = "BendixKing".to_string();
        kx_170b.avionics_types = vec!["NAV".to_string(), "COM".to_string()];

        let selected = select_unique_exact_review_candidate(
            "King",
            "KX 170B",
            &["NAV".to_string(), "COM".to_string()],
            None,
            &[review_candidate(kx_170b, Some(3))],
        )
        .unwrap();
        assert_eq!(selected.id, 51);
        assert_eq!(selected.catalog_status, "approved");
    }

    fn context(candidates: Vec<AvionicsCatalogCandidate>) -> AvionicsUnitResolutionContext {
        AvionicsUnitResolutionContext {
            aircraft_manufacturer: "Cessna".to_string(),
            aircraft_model: "172".to_string(),
            aircraft_variant: "S".to_string(),
            model_year: 2020,
            source_url: "https://broker.example/aircraft/1".to_string(),
            listing_context: "Garmin GTX345R installed".to_string(),
            requires_listing_evidence: true,
            authoritative_direct_source_urls: Vec::new(),
            authoritative_identity_anchors: Vec::new(),
            candidate: AvionicsUnitResolutionCandidate {
                manufacturer: "Garmin".to_string(),
                model: "GTX345R".to_string(),
                avionics_types: vec!["Transponder".to_string()],
                quantity: 1,
            },
            catalog_candidates: candidates,
        }
    }

    fn verified_identity() -> VerifiedIdentity {
        VerifiedIdentity {
            canonical_manufacturer: "Garmin".to_string(),
            canonical_model: "GTX 345R".to_string(),
            canonical_types: vec!["Transponder".to_string()],
            manufacturer_identifier_kind: "manufacturer_part_number".to_string(),
            manufacturer_identifier: "011-03520-00".to_string(),
            manufacturer_identifier_scope: "exact_catalog_product".to_string(),
            identity_source_url: "https://static.garmin.com/manuals/gtx345r.pdf".to_string(),
            identity_source_title: "GTX 345R installation manual".to_string(),
            identity_evidence:
                "The manufacturer manual identifies the exact product and part number.".to_string(),
            reason: "Authoritative manufacturer documentation.".to_string(),
            grounded_claim_source_urls: vec![
                "https://static.garmin.com/manuals/gtx345r.pdf".to_string()
            ],
        }
    }

    fn known_candidate(id: i64, model: &str) -> KnownApprovedAvionicsCandidate {
        KnownApprovedAvionicsCandidate {
            id,
            manufacturer: "Garmin".to_string(),
            model: model.to_string(),
            avionics_types: vec!["Transponder".to_string()],
            manufacturer_identifier_kind: "manufacturer_part_number".to_string(),
            manufacturer_identifier: format!("011-KNOWN-{id}"),
            identity_source_url: "https://static.garmin.com/manual.pdf".to_string(),
            identity_source_title: "Garmin installation manual".to_string(),
            identity_evidence: "Garmin identifies the exact approved product.".to_string(),
            avionics_manufacturer_identity_id: 1,
            canonical_product_key: normalize_avionics_identifier(model),
            canonical_identifier_key: normalize_avionics_identifier(&format!("011-KNOWN-{id}")),
        }
    }

    fn deterministic_review_candidate(
        candidate: &KnownApprovedAvionicsCandidate,
    ) -> ReviewCatalogCandidate {
        review_candidate(
            AvionicsCatalogCandidate {
                id: candidate.id,
                manufacturer: candidate.manufacturer.clone(),
                model: candidate.model.clone(),
                avionics_types: candidate.avionics_types.clone(),
                manufacturer_identifier_kind: candidate.manufacturer_identifier_kind.clone(),
                manufacturer_identifier: candidate.manufacturer_identifier.clone(),
                catalog_status: "approved".to_string(),
            },
            Some(candidate.avionics_manufacturer_identity_id),
        )
    }

    fn deterministic_gtx33_fixture() -> (
        ApprovedAvionicsProductSourceRequest,
        KnownApprovedAvionicsCandidate,
        KnownApprovedAvionicsCandidate,
        ManufacturerSourceOriginAdmission,
        FetchedSourceDocument,
    ) {
        let mut target = known_candidate(33, "GTX 33");
        target.manufacturer_identifier = "011-00455-00".to_string();
        target.canonical_identifier_key =
            normalize_avionics_identifier(&target.manufacturer_identifier);
        let mut longer_neighbor = known_candidate(330, "GTX 330");
        longer_neighbor.manufacturer_identifier = "011-00873-00".to_string();
        longer_neighbor.canonical_identifier_key =
            normalize_avionics_identifier(&longer_neighbor.manufacturer_identifier);

        let request = ApprovedAvionicsProductSourceRequest {
            source_url: "https://www.garmin.com/en-US/p/GTX33/".to_string(),
            manufacturer: target.manufacturer.clone(),
            model: target.model.clone(),
            avionics_types: target.avionics_types.clone(),
            manufacturer_identifier_kind: target.manufacturer_identifier_kind.clone(),
            manufacturer_identifier: target.manufacturer_identifier.clone(),
        };
        let admission = ManufacturerSourceOriginAdmission {
            avionics_manufacturer_id: 1,
            effective_manufacturer_identity_id: target.avionics_manufacturer_identity_id,
            canonical_origins: vec!["https://www.garmin.com".to_string()],
        };
        let fetched = FetchedSourceDocument {
            final_url: Url::parse("https://www.garmin.com/en-US/p/GTX33/")
                .expect("fixture URL should parse"),
            content_sha256: "d".repeat(64),
            publisher_text:
                "Garmin GTX 33 Mode S transponder; manufacturer part number 011-00455-00."
                    .to_string(),
            source_text_rows: vec![TextRow {
                kind: TextRowKind::HtmlTableRow,
                ordinal: 0,
                text: "GTX 33 Mode S Transponder | 011-00455-00".to_string(),
            }],
            source_text_rows_complete: true,
        };
        (request, target, longer_neighbor, admission, fetched)
    }

    #[test]
    fn deterministic_oem_proof_allows_a_distinct_part_number_prefix_neighbor() {
        let (request, target, neighbor, admission, fetched) = deterministic_gtx33_fixture();
        let catalog = vec![
            deterministic_review_candidate(&target),
            deterministic_review_candidate(&neighbor),
        ];

        let approved = deterministic_graph_approved_identity_from_source(
            &request,
            target.id,
            "Garmin GTX 33 product reference",
            &fetched,
            &admission,
            &[target.clone(), neighbor],
            &catalog,
        )
        .expect("the exact GTX 33 part number should disambiguate GTX 330");

        assert_eq!(approved.id, target.id);
        assert_eq!(
            approved.evidence,
            "GTX 33 Mode S Transponder | 011-00455-00"
        );
        assert_eq!(approved.grounded_claim_source_urls.len(), 1);
    }

    #[test]
    fn deterministic_oem_proof_rejects_a_longer_prefix_neighbor_in_the_same_row() {
        let (mut request, mut target, mut neighbor, admission, mut fetched) =
            deterministic_gtx33_fixture();
        target.model = "G1000".to_string();
        target.canonical_product_key = normalize_avionics_identifier(&target.model);
        target.manufacturer_identifier = "011-01000-00".to_string();
        target.canonical_identifier_key =
            normalize_avionics_identifier(&target.manufacturer_identifier);
        neighbor.model = "G1000 NXi".to_string();
        neighbor.canonical_product_key = normalize_avionics_identifier(&neighbor.model);
        neighbor.manufacturer_identifier = "011-01000-01".to_string();
        neighbor.canonical_identifier_key =
            normalize_avionics_identifier(&neighbor.manufacturer_identifier);
        request.model = target.model.clone();
        request.manufacturer_identifier = target.manufacturer_identifier.clone();
        fetched.publisher_text =
            "Garmin G1000 NXi Integrated Flight Deck; sku0 011-01000-00.".to_string();
        fetched.source_text_rows = vec![TextRow {
            kind: TextRowKind::HtmlTableRow,
            ordinal: 0,
            text: format!(
                "G1000 NXi Integrated Flight Deck | {}",
                target.manufacturer_identifier
            ),
        }];
        let catalog = vec![
            deterministic_review_candidate(&target),
            deterministic_review_candidate(&neighbor),
        ];

        let error = deterministic_graph_approved_identity_from_source(
            &request,
            target.id,
            "Garmin G1000 product reference",
            &fetched,
            &admission,
            std::slice::from_ref(&target),
            &catalog,
        )
        .expect_err("a G1000 NXi row cannot attest the shorter G1000 target");

        assert!(error.contains("longer manufacturer-scoped prefix-neighbor"));
    }

    #[test]
    fn deterministic_oem_proof_rejects_exact_catalog_duplicates() {
        let (request, target, _, admission, fetched) = deterministic_gtx33_fixture();

        let mut exact_model_duplicate = known_candidate(34, "GTX-33");
        exact_model_duplicate.manufacturer_identifier = "011-99999-00".to_string();
        exact_model_duplicate.canonical_identifier_key =
            normalize_avionics_identifier(&exact_model_duplicate.manufacturer_identifier);
        let model_catalog = vec![
            deterministic_review_candidate(&target),
            deterministic_review_candidate(&exact_model_duplicate),
        ];
        let error = deterministic_graph_approved_identity_from_source(
            &request,
            target.id,
            "Garmin GTX 33 product reference",
            &fetched,
            &admission,
            std::slice::from_ref(&target),
            &model_catalog,
        )
        .expect_err("an exact normalized model duplicate must fail closed");
        assert!(error.contains("exact-model duplicate"));

        let mut exact_identifier_duplicate = known_candidate(35, "Legacy GTX label");
        exact_identifier_duplicate.manufacturer_identifier = target.manufacturer_identifier.clone();
        exact_identifier_duplicate.canonical_identifier_key =
            target.canonical_identifier_key.clone();
        let identifier_catalog = vec![
            deterministic_review_candidate(&target),
            deterministic_review_candidate(&exact_identifier_duplicate),
        ];
        let error = deterministic_graph_approved_identity_from_source(
            &request,
            target.id,
            "Garmin GTX 33 product reference",
            &fetched,
            &admission,
            std::slice::from_ref(&target),
            &identifier_catalog,
        )
        .expect_err("an exact stable-identifier duplicate must fail closed");
        assert!(error.contains("manufacturer-identifier duplicate"));
    }

    #[test]
    fn deterministic_oem_proof_rejects_identity_drift_and_ambiguous_short_prefixes() {
        let (mut request, mut target, neighbor, admission, mut fetched) =
            deterministic_gtx33_fixture();
        let catalog = vec![
            deterministic_review_candidate(&target),
            deterministic_review_candidate(&neighbor),
        ];

        request.avionics_types = vec!["GPS".to_string()];
        let error = deterministic_graph_approved_identity_from_source(
            &request,
            target.id,
            "Garmin GTX 33 product reference",
            &fetched,
            &admission,
            std::slice::from_ref(&target),
            &catalog,
        )
        .expect_err("capability drift must invalidate the hash-bound decision");
        assert!(error.contains("changed manufacturer, model, capabilities, or stable identifier"));

        request.avionics_types = target.avionics_types.clone();
        let mut wrong_manufacturer_admission = admission.clone();
        wrong_manufacturer_admission.effective_manufacturer_identity_id += 1;
        let error = deterministic_graph_approved_identity_from_source(
            &request,
            target.id,
            "Garmin GTX 33 product reference",
            &fetched,
            &wrong_manufacturer_admission,
            std::slice::from_ref(&target),
            &catalog,
        )
        .expect_err("a source admitted for another manufacturer identity must fail closed");
        assert!(error.contains("not owned by the manufacturer identity"));

        target.manufacturer_identifier_kind = "manufacturer_part_number".to_string();
        target.manufacturer_identifier = "GTX 33".to_string();
        target.canonical_identifier_key = target.canonical_product_key.clone();
        request.manufacturer_identifier = target.manufacturer_identifier.clone();
        fetched.publisher_text = "Garmin GTX 33 transponder; sku0 GTX 33.".to_string();
        fetched.source_text_rows = vec![TextRow {
            kind: TextRowKind::HtmlTableRow,
            ordinal: 0,
            text: "GTX 33 Transponder | GTX 33".to_string(),
        }];
        let catalog = vec![
            deterministic_review_candidate(&target),
            deterministic_review_candidate(&neighbor),
        ];
        let error = deterministic_graph_approved_identity_from_source(
            &request,
            target.id,
            "Garmin GTX 33 product reference",
            &fetched,
            &admission,
            std::slice::from_ref(&target),
            &catalog,
        )
        .expect_err("a short prefix needs an independent OEM part number or SKU");
        assert!(error.contains("lacks a distinct OEM part number or SKU"));
    }

    #[test]
    fn deterministic_oem_proof_rejects_placeholder_part_numbers_only() {
        let (mut request, mut target, _, admission, mut fetched) = deterministic_gtx33_fixture();
        target.manufacturer_identifier = "010-01217-xx".to_string();
        target.canonical_identifier_key =
            normalize_avionics_identifier(&target.manufacturer_identifier);
        request.manufacturer_identifier = target.manufacturer_identifier.clone();
        fetched.publisher_text =
            "Garmin GTX 33 family reference; manufacturer part number 010-01217-xx.".to_string();
        let catalog = vec![deterministic_review_candidate(&target)];

        let error = deterministic_graph_approved_identity_from_source(
            &request,
            target.id,
            "Garmin GTX 33 family reference",
            &fetched,
            &admission,
            std::slice::from_ref(&target),
            &catalog,
        )
        .expect_err("a wildcard family part number cannot prove one exact product");
        assert!(error.contains("wildcard or placeholder"));
        assert!(
            !stable_oem_identifier_has_placeholder("manufacturer_model_number", "GTX X"),
            "X remains valid content in a manufacturer model number"
        );
    }

    #[test]
    fn deterministic_oem_proof_requires_one_visible_structural_row() {
        let (request, target, _, admission, mut fetched) = deterministic_gtx33_fixture();
        fetched.source_text_rows.clear();
        let catalog = vec![deterministic_review_candidate(&target)];

        let error = deterministic_graph_approved_identity_from_source(
            &request,
            target.id,
            "Garmin GTX 33 product reference",
            &fetched,
            &admission,
            std::slice::from_ref(&target),
            &catalog,
        )
        .expect_err("flat publisher text cannot replace one visible structural row");
        assert!(error.contains("no bounded visible HTML table row or PDF visual row"));
    }

    #[test]
    fn deterministic_oem_proof_never_cross_pairs_adjacent_pdf_rows() {
        let (request, target, _, admission, mut fetched) = deterministic_gtx33_fixture();
        fetched.final_url =
            Url::parse("https://static.garmin.com/pumac/transponder-table.pdf").unwrap();
        fetched.publisher_text = format!(
            "GTX 33 transponder 011-UNRELATED-00\nGTX 330 transponder {}",
            target.manufacturer_identifier
        );
        fetched.source_text_rows = vec![
            TextRow {
                kind: TextRowKind::PdfVisualRow,
                ordinal: 0,
                text: "GTX 33 transponder 011-UNRELATED-00".to_string(),
            },
            TextRow {
                kind: TextRowKind::PdfVisualRow,
                ordinal: 1,
                text: format!("GTX 330 transponder {}", target.manufacturer_identifier),
            },
        ];
        let catalog = vec![deterministic_review_candidate(&target)];

        let error = deterministic_graph_approved_identity_from_source(
            &request,
            target.id,
            "Garmin transponder table",
            &fetched,
            &admission,
            std::slice::from_ref(&target),
            &catalog,
        )
        .expect_err("adjacent PDF rows must never become one deterministic identity record");
        assert!(error.contains("no bounded visible HTML table row or PDF visual row"));
    }

    async fn seed_unreviewed_legacy_identity(
        db: &AppDb,
        identity: &VerifiedIdentity,
        manufacturer_identifier: &str,
        with_legacy_value_metadata: bool,
    ) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let manufacturer_key =
            normalize_avionics_manufacturer_name(&identity.canonical_manufacturer);
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(identity.canonical_manufacturer.trim())
        .bind(&manufacturer_key)
        .execute(pool)
        .await
        .expect("legacy manufacturer should seed");
        let manufacturer_id: i64 =
            sqlx::query_scalar("SELECT id FROM avionics_manufacturers WHERE normalized_name = ?")
                .bind(&manufacturer_key)
                .fetch_one(pool)
                .await
                .expect("legacy manufacturer should load");
        let identifier_key = normalize_avionics_identifier(manufacturer_identifier);
        sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, introduced_year,
              estimated_unit_value_usd, value_basis, replacement_cost_usd,
              value_reference_year, value_source, valuation_scope
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .bind(identity.canonical_model.trim())
        .bind(normalize_avionics_model_name(&identity.canonical_model))
        .bind(identity.manufacturer_identifier_kind.as_str())
        .bind(manufacturer_identifier)
        .bind(identifier_key)
        .bind(with_legacy_value_metadata.then_some(1999_i64))
        .bind(with_legacy_value_metadata.then_some(999_999_f64))
        .bind(if with_legacy_value_metadata {
            "installed_contribution"
        } else {
            "unreviewed"
        })
        .bind(with_legacy_value_metadata.then_some(1_000_000_f64))
        .bind(with_legacy_value_metadata.then_some(2026_i64))
        .bind(with_legacy_value_metadata.then_some("legacy-import"))
        .bind(if with_legacy_value_metadata {
            "integrated_suite"
        } else {
            "unit"
        })
        .fetch_one(pool)
        .await
        .expect("unreviewed legacy identity should seed")
    }

    fn grounding(
        url: &str,
        title: &str,
        evidence: &str,
    ) -> (Vec<GeminiGroundingSource>, Vec<GeminiGroundingSupport>) {
        (
            vec![GeminiGroundingSource {
                chunk_index: 0,
                url: url.to_string(),
                title: title.to_string(),
            }],
            vec![GeminiGroundingSupport {
                text: evidence.to_string(),
                source_indices: vec![0],
            }],
        )
    }

    fn reject_response() -> serde_json::Value {
        json!({
            "status": "reject",
            "catalog_id": 0,
            "canonical_manufacturer": "",
            "canonical_model": "",
            "canonical_types": [],
            "manufacturer_identifier_kind": "none",
            "manufacturer_identifier": "",
            "manufacturer_identifier_scope": "none",
            "rejection_basis": "generic_or_class_only",
            "confidence": "high",
            "identity_source_url": "",
            "identity_source_title": "",
            "identity_evidence": "",
            "reason": "Garmin GTX 345R is a generic or class-only label, not a concrete product."
        })
    }

    #[test]
    fn ungrounded_reject_requires_correction_and_cannot_become_rejected() {
        let context = context(vec![]);
        let response = reject_response();

        let issues = resolution_issues(&context, &response, false, &[], &[]);
        assert!(issues.iter().any(|issue| {
            issue.contains(
                "verified grounding with the candidate-specific negative reason contained in one linked cited support span",
            )
        }));

        let outcome = nonpositive_identity_outcome(&context, &response, false, &[], &[])
            .expect("reject is a non-positive outcome");
        assert!(matches!(
            outcome,
            AvionicsIdentityOutcome::Unresolved { .. }
        ));

        // A provider flag without the actual cited metadata also fails closed.
        let outcome = nonpositive_identity_outcome(&context, &response, true, &[], &[])
            .expect("reject is a non-positive outcome");
        assert!(matches!(
            outcome,
            AvionicsIdentityOutcome::Unresolved { .. }
        ));
    }

    #[test]
    fn direct_source_reject_cannot_be_blanket_short_circuited_before_correction() {
        let context = context(vec![]);
        let reject = reject_response();
        let source_url = "https://static.garmin.com/manuals/gtx345r.pdf";
        let exact_publisher_span =
            "Garmin GTX 345R remote transponder, manufacturer part number 011-03520-00.";

        let reject_issues = resolution_issues_with_direct_source_proofs(
            &context,
            &reject,
            true,
            &[],
            &[],
            &[direct_source_proof(source_url, exact_publisher_span)],
        );
        assert!(
            reject_issues
                .iter()
                .any(|issue| issue.contains("candidate-specific negative reason")),
            "a direct publisher identity span cannot authorize an automatic negative claim"
        );

        let supported_positive = json!({
            "status": "propose_new",
            "catalog_id": 0,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "manufacturer_identifier_scope": "exact_catalog_product",
            "rejection_basis": "none",
            "confidence": "very_high",
            "identity_source_url": source_url,
            "identity_source_title": "Garmin GTX 345R installation manual",
            "identity_evidence": exact_publisher_span,
            "reason": "The exact publisher span identifies one concrete transponder."
        });
        assert!(
            resolution_issues_with_direct_source_proofs(
                &context,
                &supported_positive,
                true,
                &[],
                &[],
                &[direct_source_proof(source_url, exact_publisher_span)],
            )
            .is_empty(),
            "the unchanged direct-source packet can support a positive correction even when the first structure response was an unsafe reject"
        );
    }

    #[test]
    fn direct_source_wrong_excerpt_can_be_repaired_from_the_same_publisher_window() {
        let context = context(vec![]);
        let source_url = "https://static.garmin.com/manuals/gtx345r.pdf";
        let incomplete_span = "Garmin identifies the GTX 345R as a remote transponder.";
        let exact_publisher_span =
            "Garmin GTX 345R remote transponder, manufacturer part number 011-03520-00.";
        let mut response = json!({
            "status": "propose_new",
            "catalog_id": 0,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "manufacturer_identifier_scope": "exact_catalog_product",
            "rejection_basis": "none",
            "confidence": "very_high",
            "identity_source_url": source_url,
            "identity_source_title": "Garmin GTX 345R installation manual",
            "identity_evidence": incomplete_span,
            "reason": "The manufacturer manual identifies the unit."
        });

        let initial_issues = resolution_issues_with_direct_source_proofs(
            &context,
            &response,
            true,
            &[],
            &[],
            &[direct_source_proof(source_url, incomplete_span)],
        );
        assert!(initial_issues.iter().any(|issue| {
            issue.contains("complete canonical model and manufacturer identifier")
        }));

        response["identity_evidence"] = json!(exact_publisher_span);
        assert!(
            resolution_issues_with_direct_source_proofs(
                &context,
                &response,
                true,
                &[],
                &[],
                &[direct_source_proof(source_url, exact_publisher_span)],
            )
            .is_empty(),
            "a tools-disabled correction may select a sufficient exact excerpt from an unchanged verified publisher window"
        );
    }

    #[test]
    fn grounded_high_confidence_reject_remains_rejected() {
        let context = context(vec![]);
        let response = reject_response();
        let (sources, supports) = grounding(
            "https://www.garmin.com/en-US/aviation/",
            "Garmin aviation products",
            "Garmin GTX 345R is a generic or class-only label, not a concrete product.",
        );

        assert!(resolution_issues(&context, &response, true, &sources, &supports).is_empty());
        let outcome = nonpositive_identity_outcome(&context, &response, true, &sources, &supports)
            .expect("reject is a non-positive outcome");
        assert!(matches!(outcome, AvionicsIdentityOutcome::Rejected { .. }));
    }

    #[test]
    fn unrelated_grounding_cannot_authorize_automatic_rejection() {
        let context = context(vec![]);
        let response = reject_response();
        let (sources, supports) = grounding(
            "https://www.garmin.com/en-US/aviation/",
            "Garmin aviation products",
            "The cited page describes general aviation equipment categories.",
        );

        let issues = resolution_issues(&context, &response, true, &sources, &supports);
        assert!(issues
            .iter()
            .any(|issue| issue.contains("candidate-specific negative reason")));
        let outcome = nonpositive_identity_outcome(&context, &response, true, &sources, &supports)
            .expect("reject is a non-positive outcome");
        assert!(matches!(
            outcome,
            AvionicsIdentityOutcome::Unresolved { .. }
        ));

        let (sources, supports) = grounding(
            "https://example.gov/avionics/gtx-345r",
            "GTX 345R record",
            "The GTX-345R identifier appears in this record.",
        );
        let issues = resolution_issues(&context, &response, true, &sources, &supports);
        assert!(issues
            .iter()
            .any(|issue| issue.contains("candidate-specific negative reason")));
        let outcome = nonpositive_identity_outcome(&context, &response, true, &sources, &supports)
            .expect("reject is a non-positive outcome");
        assert!(matches!(
            outcome,
            AvionicsIdentityOutcome::Unresolved { .. }
        ));
    }

    #[test]
    fn identity_only_or_contradictory_citation_cannot_authorize_rejection() {
        let context = context(vec![]);
        let response = reject_response();

        let (sources, supports) = grounding(
            "https://example.gov/avionics/gtx-345r",
            "GTX 345R record",
            "Garmin GTX 345R is an identified avionics product.",
        );
        assert!(!resolution_issues(&context, &response, true, &sources, &supports).is_empty());
        let outcome = nonpositive_identity_outcome(&context, &response, true, &sources, &supports)
            .expect("reject is a non-positive outcome");
        assert!(matches!(
            outcome,
            AvionicsIdentityOutcome::Unresolved { .. }
        ));

        let (sources, supports) = grounding(
            "https://example.gov/avionics/gtx-345r",
            "GTX 345R installation record",
            "Garmin GTX 345R is a concrete avionics product installed in the aircraft.",
        );
        assert!(!resolution_issues(&context, &response, true, &sources, &supports).is_empty());
        let outcome = nonpositive_identity_outcome(&context, &response, true, &sources, &supports)
            .expect("reject is a non-positive outcome");
        assert!(matches!(
            outcome,
            AvionicsIdentityOutcome::Unresolved { .. }
        ));
    }

    #[test]
    fn exact_server_fetched_direct_source_proof_can_replace_missing_support_metadata() {
        let source_url = "https://static.garmin.com/manuals/gtx345r.pdf";
        let evidence = "The manual identifies the remote GTX 345R as part number 011-03520-00.";
        let proof = direct_source_proof(source_url, evidence);

        assert!(evidence_is_bound_to_direct_source_proof(
            source_url,
            evidence,
            "GTX 345R",
            "011-03520-00",
            std::slice::from_ref(&proof),
        ));
        assert!(!evidence_is_bound_to_direct_source_proof(
            source_url,
            "The manual identifies a different part number.",
            "GTX 345R",
            "011-03520-00",
            std::slice::from_ref(&proof),
        ));
        assert!(!evidence_is_bound_to_direct_source_proof(
            "https://unrelated.example/manual.pdf",
            evidence,
            "GTX 345R",
            "011-03520-00",
            std::slice::from_ref(&proof),
        ));
        assert!(!evidence_is_bound_to_direct_source_proof(
            source_url,
            evidence,
            "GTX 345",
            "011-03520-00",
            std::slice::from_ref(&proof),
        ));

        let mut malformed = proof;
        malformed.content_sha256 = "not-a-content-digest".to_string();
        assert!(!evidence_is_bound_to_direct_source_proof(
            source_url,
            evidence,
            "GTX 345R",
            "011-03520-00",
            &[malformed],
        ));
    }

    #[test]
    fn similarity_retrieval_includes_exact_typography_variant_but_does_not_resolve_it() {
        let catalog = vec![
            candidate(1, "GTX 345R", "approved"),
            candidate(2, "GMA 350", "approved"),
        ];
        let shortlist = shortlist_avionics_candidates(
            "Garmin",
            "GTX-345R",
            &["Transponder".to_string()],
            None,
            &catalog,
        );
        assert_eq!(
            shortlist.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn collision_catalog_is_scoped_to_one_effective_manufacturer_identity() {
        let mut canonical_alias = candidate(1, "X 100", "unreviewed");
        canonical_alias.manufacturer = "Garmin International".to_string();
        let mut other_known_identity = candidate(2, "X 100", "unreviewed");
        other_known_identity.manufacturer = "Garmin".to_string();
        let mut name_only_fallback = candidate(3, "X 100", "unreviewed");
        name_only_fallback.manufacturer = "Garmin".to_string();
        let mut unrelated_name_only = candidate(4, "X 100", "unreviewed");
        unrelated_name_only.manufacturer = "BendixKing".to_string();
        let catalog = vec![
            review_candidate(canonical_alias, Some(10)),
            review_candidate(other_known_identity, Some(20)),
            review_candidate(name_only_fallback, None),
            review_candidate(unrelated_name_only, None),
        ];

        let scoped = manufacturer_scoped_catalog_candidates("Garmin", Some(10), &catalog);
        assert_eq!(
            scoped.iter().map(|candidate| candidate.id).collect::<Vec<_>>(),
            vec![1, 3],
            "an effective identity match wins over display spelling, while a different known identity can never bridge through the same spelling"
        );
    }

    #[test]
    fn cross_manufacturer_model_match_can_only_be_different_product() {
        let mut other_manufacturer = candidate(2, "X 100", "unreviewed");
        other_manufacturer.manufacturer = "BendixKing".to_string();
        other_manufacturer.manufacturer_identifier_kind = "none".to_string();
        other_manufacturer.manufacturer_identifier.clear();
        let response = json!({
            "canonical_manufacturer": "Garmin",
            "canonical_model": "X 100",
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-00001-00"
        });

        validate_collision_decision_relation(&response, &other_manufacturer, "different_product")
            .expect(
                "the same model label in another manufacturer namespace is a different product",
            );
        let error =
            validate_collision_decision_relation(&response, &other_manufacturer, "same_product")
                .expect_err("model equality alone must never merge manufacturers");
        assert!(error.to_string().contains("same manufacturer"));
    }

    #[test]
    fn listing_only_gemini_adjudication_requires_a_curated_oem_family_gate() {
        let direct = IdentityGroundingPlan::Direct(AuthoritativeDirectSourcePlan {
            source_urls: vec!["https://static.garmin.com/pumac/catalog.pdf".to_string()],
            identity_anchors: vec!["Garmin".to_string(), "G1000 NXi".to_string()],
            admission: ManufacturerSourceOriginAdmission {
                avionics_manufacturer_id: 1,
                effective_manufacturer_identity_id: 2,
                canonical_origins: vec!["https://static.garmin.com".to_string()],
            },
            requirement: DirectSourceRequirement::Explicit,
        });
        let search = IdentityGroundingPlan::Search;

        assert!(!should_run_listing_only_approved_candidate_adjudication(
            &direct
        ));
        assert!(!should_run_listing_only_approved_candidate_adjudication(
            &search
        ));
    }

    fn source_hint(url: &str, title: &str, evidence: &str) -> AuthoritativeSourceHintRow {
        AuthoritativeSourceHintRow {
            evidence_source_url: url.to_string(),
            evidence_source_title: title.to_string(),
            evidence_text: evidence.to_string(),
        }
    }

    #[test]
    fn opportunistic_source_selection_requires_one_exact_capability_compatible_product() {
        let mut request = local_request("Garmin GIA 63W GPS NAV COM");
        request.model = "GIA 63W".to_string();
        request.avionics_types = vec!["GPS".to_string(), "NAV".to_string(), "COM".to_string()];
        let exact = review_candidate_with_types(239, "GIA63W", &["GPS", "NAV", "COM"]);
        let shorter = review_candidate_with_types(11, "GIA 63", &["GPS", "NAV", "COM"]);
        let source = source_hint(
            "https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf",
            "Garmin GIA 63/GIA 63W Installation Manual",
            "GIA 63W Unit Only, (011-01105-00) 010-00386-00",
        );

        let selected = select_opportunistic_authoritative_source_urls(
            &request,
            &request.avionics_types,
            1,
            &[exact.clone(), shorter],
            std::slice::from_ref(&source),
        );
        assert_eq!(selected, vec![source.evidence_source_url.clone()]);

        let duplicate = review_candidate_with_types(240, "GIA-63W", &["GPS", "NAV", "COM"]);
        assert!(select_opportunistic_authoritative_source_urls(
            &request,
            &request.avionics_types,
            1,
            &[exact.clone(), duplicate],
            std::slice::from_ref(&source),
        )
        .is_empty());

        let mut rejected = exact;
        rejected.candidate.catalog_status = "rejected".to_string();
        assert!(select_opportunistic_authoritative_source_urls(
            &request,
            &request.avionics_types,
            1,
            &[rejected],
            std::slice::from_ref(&source),
        )
        .is_empty());
    }

    #[test]
    fn opportunistic_source_selection_rejects_capability_and_suffix_ambiguity() {
        let source = source_hint(
            "https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf",
            "Garmin GIA 63/GIA 63W Installation Manual",
            "GIA 63W Unit Only, (011-01105-00) 010-00386-00",
        );
        let mut mismatched = local_request("Garmin GDC 74 air data computer");
        mismatched.model = "GDC 74".to_string();
        mismatched.avionics_types = vec!["Air Data Computer".to_string()];
        assert!(select_opportunistic_authoritative_source_urls(
            &mismatched,
            &mismatched.avionics_types,
            1,
            &[review_candidate_with_types(
                86,
                "GDC-74",
                &["Integrated Flight Deck"],
            )],
            &[source.clone()],
        )
        .is_empty());

        let mut base = local_request("Garmin GIA 63 GPS NAV COM");
        base.model = "GIA 63".to_string();
        base.avionics_types = vec!["GPS".to_string(), "NAV".to_string(), "COM".to_string()];
        assert!(select_opportunistic_authoritative_source_urls(
            &base,
            &base.avionics_types,
            1,
            &[
                review_candidate_with_types(11, "GIA 63", &["GPS", "NAV", "COM"]),
                review_candidate_with_types(239, "GIA 63W", &["GPS", "NAV", "COM"]),
            ],
            &[source],
        )
        .is_empty());
    }

    #[test]
    fn collision_shortlist_admission_is_model_identity_first_and_number_aware() {
        let with_types = |id, model: &str, types: &[&str]| {
            let mut candidate = candidate(id, model, "unreviewed");
            candidate.avionics_types = types.iter().map(|value| (*value).to_string()).collect();
            candidate
        };
        let catalog = vec![
            with_types(4, "GIA-63W", &["NAV", "COM"]),
            with_types(11, "GIA 63", &["NAV", "COM"]),
            with_types(20, "GI 275", &["Flight Display"]),
            with_types(75, "GIA 63", &["Integrated Flight Deck"]),
            with_types(233, "GI 275", &["Standby Instrument"]),
            with_types(238, "GIA 64W", &["GPS"]),
            with_types(239, "GIA63W", &["GPS"]),
            with_types(248, "GIA 64W", &["NAV", "COM"]),
            with_types(474, "GI 106A", &["NAV", "COM"]),
            with_types(550, "Aera 660", &["GPS"]),
            with_types(602, "GI 209", &["Flight Display"]),
            with_types(635, "GI 106B", &["Flight Display"]),
        ];

        let shortlist = shortlist_avionics_candidates(
            "Garmin",
            "GIA63W",
            &["GPS".to_string(), "NAV".to_string(), "COM".to_string()],
            None,
            &catalog,
        );

        // The former combined-score admission returned all 12 rows here:
        // [4, 239, 11, 75, 238, 248, 474, 550, 20, 233, 602, 635].
        // In particular, Garmin and capability bonuses were sufficient to
        // admit GI 106A and Aera 660 despite no product-model relationship.
        assert_eq!(
            shortlist.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![4, 239, 11, 75],
            "manufacturer and capability bonuses may rank an admitted model relation, but cannot admit unrelated products or a different numeric model"
        );
        assert!(model_identity_relation_score("GIA63W", "GIA 63").is_some());
        assert!(model_identity_relation_score("GIA63W", "GIA 64W").is_none());
    }

    #[test]
    fn exact_stable_identifier_admits_a_candidate_without_a_model_relation() {
        let mut catalog_row = candidate(9, "Legacy Imported Label", "unreviewed");
        catalog_row.manufacturer_identifier = "011-03520-00".to_string();

        let shortlist = shortlist_avionics_candidates(
            "Garmin",
            "GTX 345R",
            &["Transponder".to_string()],
            Some("011-03520-00"),
            std::slice::from_ref(&catalog_row),
        );

        assert_eq!(
            shortlist.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![9]
        );
        assert!(
            shortlist_avionics_candidates(
                "Garmin",
                "GTX 345R",
                &["Transponder".to_string()],
                Some("prefix-011-03520-00-suffix"),
                std::slice::from_ref(&catalog_row),
            )
            .is_empty(),
            "substring overlap must not be promoted to stable-identifier equality"
        );
    }

    #[test]
    fn grounded_identifier_expands_collision_set_before_storage() {
        let mut catalog_row = candidate(9, "Legacy Imported Label", "unreviewed");
        catalog_row.manufacturer_identifier = "011-03520-00".to_string();
        let mut initial = context(vec![]);
        initial.authoritative_direct_source_urls =
            vec!["https://static.garmin.com/manual.pdf".to_string()];
        initial.authoritative_identity_anchors = vec!["Garmin".to_string(), "GTX 345R".to_string()];
        let expanded = expanded_collision_context(&initial, &verified_identity(), &[catalog_row]);
        assert_eq!(
            expanded
                .catalog_candidates
                .iter()
                .map(|candidate| candidate.id)
                .collect::<Vec<_>>(),
            vec![9]
        );
        assert_eq!(
            expanded.authoritative_direct_source_urls,
            initial.authoritative_direct_source_urls
        );
        assert_eq!(
            expanded.authoritative_identity_anchors,
            initial.authoritative_identity_anchors
        );
    }

    fn local_request(listing_context: &str) -> AvionicsIdentityRequest {
        AvionicsIdentityRequest {
            aircraft_manufacturer: "Cessna".to_string(),
            aircraft_model: "172".to_string(),
            aircraft_variant: "S".to_string(),
            model_year: 2020,
            source_url: "https://broker.example/aircraft/1".to_string(),
            listing_context: listing_context.to_string(),
            requires_listing_evidence: true,
            authoritative_direct_source_urls: Vec::new(),
            authoritative_identity_anchors: Vec::new(),
            manufacturer: "Garmin".to_string(),
            model: "GTX345R".to_string(),
            avionics_types: vec!["Transponder".to_string()],
            quantity: 1,
        }
    }

    async fn seed_garmin_static_source_authority(db: &AppDb) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Garmin', 'garmin') ON CONFLICT (normalized_name) DO NOTHING",
        )
        .execute(pool)
        .await
        .expect("Garmin manufacturer should seed");
        let manufacturer_id: i64 = sqlx::query_scalar(
            "SELECT id FROM avionics_manufacturers WHERE normalized_name = 'garmin'",
        )
        .fetch_one(pool)
        .await
        .expect("Garmin manufacturer should load");
        let membership = ensure_manufacturer_identity(
            db,
            manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: "https://www.garmin.com/company/about/".to_string(),
                source_title: "About Garmin".to_string(),
                evidence_text:
                    "Garmin's official site identifies Garmin as an avionics manufacturer."
                        .to_string(),
            },
        )
        .await
        .expect("Garmin manufacturer identity should seed");
        sqlx::query(
            r#"INSERT INTO avionics_authoritative_source_origins (
                 authority_kind, avionics_manufacturer_identity_id, https_origin,
                 evidence_source_url, evidence_source_title, evidence_text,
                 approval_basis, approval_reason
               ) VALUES (
                 'manufacturer_primary', ?, 'https://static.garmin.com',
                 'https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf',
                 'Garmin GIA 63/GIA 63W Installation Manual',
                 'GIA 63W Unit Only, manufacturer model number GIA 63W',
                 'curated_bootstrap',
                 'Test fixture for exact Garmin static source authority'
               )
               ON CONFLICT DO NOTHING"#,
        )
        .bind(membership.avionics_manufacturer_identity_id)
        .execute(pool)
        .await
        .expect("Garmin source authority should seed");
        sqlx::query_scalar(
            r#"SELECT id
               FROM avionics_authoritative_source_origins
               WHERE avionics_manufacturer_identity_id = ?
                 AND https_origin = 'https://static.garmin.com'"#,
        )
        .bind(membership.avionics_manufacturer_identity_id)
        .fetch_one(pool)
        .await
        .expect("Garmin source authority should load")
    }

    async fn revoke_source_authority(db: &AppDb, source_origin_id: i64) {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let reviewer_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO users (
                 email, display_name, auth_subject
               ) VALUES (
                 'origin-revoker@example.test', 'Origin Revoker',
                 'origin-revoker'
               )
               ON CONFLICT (email) DO UPDATE SET display_name = excluded.display_name
               RETURNING id"#,
        )
        .fetch_one(pool)
        .await
        .expect("origin revocation reviewer should seed");
        sqlx::query(
            r#"INSERT INTO avionics_authoritative_source_origin_revocations (
                 avionics_authoritative_source_origin_id,
                 revoked_by_user_id, reason
               ) VALUES (?, ?, ?)"#,
        )
        .bind(source_origin_id)
        .bind(reviewer_id)
        .bind("The exact source origin is no longer trusted for product evidence.")
        .execute(pool)
        .await
        .expect("source authority should be revoked");
    }

    #[tokio::test]
    async fn direct_source_preflight_rejects_unknown_or_unapproved_origins_before_gemini() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let mut request = local_request("Garmin GIA 63W installed");
        request.model = "GIA 63W".to_string();
        request.authoritative_direct_source_urls =
            vec!["https://static.garmin.com/pumac/GIA63.pdf".to_string()];
        request.authoritative_identity_anchors = vec!["Garmin".to_string(), "GIA 63W".to_string()];

        let unknown = explicit_authoritative_direct_source_plan(&db, &request)
            .await
            .expect_err("an unknown manufacturer authority must fail closed");
        assert!(unknown.to_string().contains("direct-source admission"));

        seed_garmin_static_source_authority(&db).await;
        let authorized = explicit_authoritative_direct_source_plan(&db, &request)
            .await
            .expect("the exact curated Garmin origin should be admitted")
            .expect("the request supplied an explicit direct source");
        assert_eq!(authorized.admission.avionics_manufacturer_id, 1);
        assert_eq!(
            authorized.source_urls,
            request.authoritative_direct_source_urls
        );

        request.authoritative_direct_source_urls =
            vec!["https://support.garmin.com/en-US/aviation/".to_string()];
        let sibling = explicit_authoritative_direct_source_plan(&db, &request)
            .await
            .expect_err("an unapproved sibling origin must fail before Gemini");
        assert!(sibling.to_string().contains("direct-source admission"));

        request.authoritative_direct_source_urls =
            vec!["https://reviewer@static.garmin.com/GIA63.pdf".to_string()];
        let error = explicit_authoritative_direct_source_plan(&db, &request)
            .await
            .expect_err("a malformed direct-source URL is a caller error");
        assert!(error.to_string().contains("cannot contain credentials"));
    }

    #[tokio::test]
    async fn opportunistic_source_planner_loads_only_active_manufacturer_primary_hints() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let source_origin_id = seed_garmin_static_source_authority(&db).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let (manufacturer_id, manufacturer_identity_id): (i64, i64) = sqlx::query_as(
            r#"
            SELECT manufacturer.id, membership.avionics_manufacturer_identity_id
            FROM avionics_manufacturers manufacturer
            JOIN avionics_manufacturer_identity_memberships membership
              ON membership.avionics_manufacturer_id = manufacturer.id
            WHERE manufacturer.normalized_name = 'garmin'
            "#,
        )
        .fetch_one(pool)
        .await
        .expect("curated Garmin identity should load");
        let product_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name
            ) VALUES (?, 'GIA 63W', 'gia 63w')
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .expect("GIA 63W should seed");
        for capability in ["GPS", "NAV", "COM"] {
            sqlx::query(
                "INSERT INTO avionics_types (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
            )
            .bind(capability)
            .bind(capability.to_ascii_lowercase())
            .execute(pool)
            .await
            .expect("capability should seed");
            sqlx::query(
                r#"
                INSERT INTO avionics_model_types (
                  avionics_model_id, avionics_type_id
                )
                SELECT ?, id
                FROM avionics_types
                WHERE normalized_name = ?
                "#,
            )
            .bind(product_id)
            .bind(capability.to_ascii_lowercase())
            .execute(pool)
            .await
            .expect("product capability should seed");
        }

        let mut request = local_request("Garmin GIA 63W GPS NAV COM");
        request.model = "GIA 63W".to_string();
        request.avionics_types = vec!["GPS".to_string(), "NAV".to_string(), "COM".to_string()];
        let review_catalog = load_review_catalog_candidates(&db)
            .await
            .expect("catalog should load");
        let plan = opportunistic_authoritative_direct_source_plan(
            &db,
            &request,
            &request.avionics_types,
            Some(manufacturer_identity_id),
            &review_catalog,
        )
        .await
        .expect("planner should succeed")
        .expect("the exact active source should be selected");
        assert_eq!(plan.requirement, DirectSourceRequirement::Opportunistic);
        assert_eq!(
            plan.source_urls,
            vec!["https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf".to_string()]
        );

        revoke_source_authority(&db, source_origin_id).await;
        assert!(
            opportunistic_authoritative_direct_source_plan(
                &db,
                &request,
                &request.avionics_types,
                Some(manufacturer_identity_id),
                &review_catalog,
            )
            .await
            .expect("revoked source should be treated as unavailable")
            .is_none(),
            "the active-source view must remove revoked retrieval hints"
        );
    }

    #[test]
    fn authorized_direct_source_response_is_bound_to_one_exact_requested_origin() {
        let plan = AuthoritativeDirectSourcePlan {
            source_urls: vec![
                "https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf".to_string(),
            ],
            identity_anchors: vec!["Garmin".to_string(), "GIA 63W".to_string()],
            admission: ManufacturerSourceOriginAdmission {
                avionics_manufacturer_id: 1,
                effective_manufacturer_identity_id: 2,
                canonical_origins: vec!["https://static.garmin.com".to_string()],
            },
            requirement: DirectSourceRequirement::Explicit,
        };

        let safe_nonpositive =
            direct_source_response(&["https://static.garmin.com/pumac/GIA63W.pdf"]);
        assert!(safe_nonpositive.grounding_sources.is_empty());
        assert!(safe_nonpositive.source_evidence_proofs.is_empty());
        validate_authorized_direct_source_response(&plan, &safe_nonpositive).expect(
            "verified fetch-window URLs must retain provenance when a nonpositive response uses no claim evidence",
        );

        for final_url in [
            "https://www.garmin.com/pumac/GIA63W.pdf",
            "https://support.garmin.com/pumac/GIA63W.pdf",
            "https://static.garmin.com.evil.example/pumac/GIA63W.pdf",
        ] {
            let error = validate_authorized_direct_source_response(
                &plan,
                &direct_source_response(&[final_url]),
            )
            .expect_err("a sibling, redirect, or spoofed origin must remain unbound");
            assert!(
                error.to_string().contains("unbound final URL"),
                "{final_url}"
            );
        }

        let error = validate_authorized_direct_source_response(&plan, &direct_source_response(&[]))
            .expect_err("a verified direct response must expose its final source URL");
        assert!(error.to_string().contains("did not expose"));
    }

    #[tokio::test]
    async fn ordinary_search_response_rejects_a_revoked_exact_evidence_origin() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let source_origin_id = seed_garmin_static_source_authority(&db).await;
        revoke_source_authority(&db, source_origin_id).await;
        let mut response = direct_source_response(&["https://static.garmin.com/pumac/GIA63W.pdf"]);
        response.google_search_used = true;
        response.authoritative_direct_source_verified = false;
        response.value = json!({
            "identity_source_url": "https://static.garmin.com/pumac/GIA63W.pdf"
        });

        let error = require_response_evidence_source_urls_not_revoked(&db, &response)
            .await
            .expect_err("ordinary Search evidence must honor append-only origin revocation");
        assert!(error.to_string().contains("has been revoked"));
    }

    #[tokio::test]
    async fn direct_source_admission_is_revalidated_after_the_model_call() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let source_origin_id = seed_garmin_static_source_authority(&db).await;
        let mut request = local_request("Garmin GIA 63W installed");
        request.model = "GIA 63W".to_string();
        request.authoritative_direct_source_urls =
            vec!["https://static.garmin.com/pumac/GIA63W.pdf".to_string()];
        request.authoritative_identity_anchors = vec!["Garmin".to_string(), "GIA 63W".to_string()];
        let plan = explicit_authoritative_direct_source_plan(&db, &request)
            .await
            .expect("the direct source should initially be admitted")
            .expect("the request supplied an explicit direct source");
        let response =
            direct_source_response(&["https://static.garmin.com/pumac/GIA63W-final.pdf"]);
        validate_authorized_direct_source_response(&plan, &response)
            .expect("the cached admission alone still appears valid");

        revoke_source_authority(&db, source_origin_id).await;
        let error = revalidate_direct_source_admission_state(
            &db,
            "Garmin",
            &IdentityGroundingPlan::Direct(plan),
            &response,
        )
        .await
        .expect_err("post-model validation must observe in-flight revocation");
        assert!(error.to_string().contains("changed or was revoked"));
    }

    #[tokio::test]
    async fn direct_source_revalidation_does_not_preempt_model_url_correction() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let mut request = local_request("Garmin GIA 63W installed");
        request.model = "GIA 63W".to_string();
        request.authoritative_direct_source_urls =
            vec!["https://static.garmin.com/pumac/GIA63W.pdf".to_string()];
        request.authoritative_identity_anchors = vec!["Garmin".to_string(), "GIA 63W".to_string()];
        let plan = explicit_authoritative_direct_source_plan(&db, &request)
            .await
            .expect("the direct source should be admitted")
            .expect("the request supplied an explicit direct source");
        let mut response =
            direct_source_response(&["https://static.garmin.com/pumac/GIA63W-final.pdf"]);
        response.value = json!({
            "candidate_source_url": "http://static.garmin.com/pumac/GIA63W-final.pdf"
        });

        revalidate_direct_source_admission_state(
            &db,
            "Garmin",
            &IdentityGroundingPlan::Direct(plan),
            &response,
        )
        .await
        .expect("server-owned direct-source provenance must be checked independently");
        let error = require_response_evidence_source_urls_not_revoked(&db, &response)
            .await
            .expect_err("the model-owned HTTP URL must remain invalid after domain correction");
        assert!(error.to_string().contains("HTTPS"));
    }

    #[tokio::test]
    async fn revocation_invalidates_reuse_but_preserves_approved_catalog_identity() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let source_origin_id = seed_garmin_static_source_authority(&db).await;
        let identity = verified_identity();
        let stored =
            persist_approved_identity(&db, None, &[], &identity, &catalog_fingerprint(&[]))
                .await
                .expect("approved product should seed before revocation");
        let request =
            local_request("Garmin GTX345R manufacturer part number 011-03520-00 installed");
        assert!(resolve_verified_local_avionics_identity(&db, &request)
            .await
            .expect("local lookup should succeed")
            .is_some());

        revoke_source_authority(&db, source_origin_id).await;
        assert!(
            resolve_verified_local_avionics_identity(&db, &request)
                .await
                .expect("local lookup should fail closed")
                .is_none(),
            "a revoked attestation origin must disable no-Gemini reuse"
        );
        assert!(
            !load_known_approved_candidates(&db)
                .await
                .expect("approved candidates should load")
                .iter()
                .any(|candidate| candidate.id == stored.id),
            "a revoked product must leave the bounded approved-reuse loader"
        );
        assert!(
            load_catalog_candidates(&db)
                .await
                .expect("catalog should load")
                .iter()
                .any(|candidate| candidate.id == stored.id),
            "the approved identity remains in the general catalog"
        );
    }

    #[tokio::test]
    async fn capability_enrichment_rechecks_revocation_inside_its_write_transaction() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let source_origin_id = seed_garmin_static_source_authority(&db).await;
        let identity = verified_identity();
        let stored =
            persist_approved_identity(&db, None, &[], &identity, &catalog_fingerprint(&[]))
                .await
                .expect("approved product should seed before revocation");
        let catalog = load_catalog_candidates(&db)
            .await
            .expect("catalog should load before revocation");
        let reviewed = catalog
            .iter()
            .find(|candidate| candidate.id == stored.id)
            .expect("approved product should be reviewable")
            .clone();
        let fingerprint = catalog_fingerprint(&catalog);
        let mut enriched = identity;
        enriched.canonical_types = vec!["GPS".to_string(), "Transponder".to_string()];

        revoke_source_authority(&db, source_origin_id).await;
        let error = persist_approved_capability_enrichment(&db, &reviewed, &enriched, &fingerprint)
            .await
            .expect_err("capability enrichment must recheck revocation under its write lock");
        assert!(error.to_string().contains("revoked before persistence"));
    }

    fn gma_1347_known_candidate() -> KnownApprovedAvionicsCandidate {
        let mut candidate = known_candidate(2, "GMA 1347");
        candidate.avionics_types = vec!["Audio Panel".to_string()];
        candidate.manufacturer_identifier = "011-00809-00".to_string();
        candidate.canonical_identifier_key =
            normalize_avionics_identifier(&candidate.manufacturer_identifier);
        candidate
    }

    fn gma_1347_catalog_candidate(status: &str) -> AvionicsCatalogCandidate {
        AvionicsCatalogCandidate {
            id: 2,
            manufacturer: "Garmin".to_string(),
            model: "GMA 1347".to_string(),
            avionics_types: vec!["Audio Panel".to_string()],
            manufacturer_identifier_kind: "manufacturer_part_number".to_string(),
            manufacturer_identifier: "011-00809-00".to_string(),
            catalog_status: status.to_string(),
        }
    }

    fn gma_1347_model_only_request() -> AvionicsIdentityRequest {
        AvionicsIdentityRequest {
            model: "GMA-1347".to_string(),
            avionics_types: vec!["Audio Panel".to_string()],
            listing_context: "GMA-1347 Digital Audio Panel w/ Intercom".to_string(),
            ..local_request("")
        }
    }

    #[test]
    fn exact_local_label_uses_a_current_attested_catalog_product_without_a_part_number() {
        let request = local_request("Garmin GTX345R installed");
        let candidates = vec![known_candidate(7, "GTX 345R")];
        let approved = known_approved_local_match(
            &request,
            &["Transponder".to_string()],
            1,
            &candidates,
            &[candidate(7, "GTX 345R", "approved")],
        )
        .expect("an exact label may reuse an independently attested catalog product");
        assert_eq!(approved.id, 7);
        assert!(approved
            .reason
            .contains("exact retained manufacturer/model"));
    }

    #[test]
    fn exact_label_does_not_require_proving_that_an_unwritten_variant_was_absent() {
        let request = local_request("Garmin avionics package; GTX345R installed");
        let selected = known_candidate(7, "GTX 345R");
        let mut selected_catalog = candidate(7, "GTX 345R", "approved");
        selected_catalog.manufacturer_identifier = selected.manufacturer_identifier.clone();
        let longer_neighbor = candidate(8, "GTX 345RD", "unreviewed");
        let approved = known_approved_local_match(
            &request,
            &["Transponder".to_string()],
            1,
            &[selected],
            &[selected_catalog, longer_neighbor],
        )
        .expect("the literal exact model wins when the listing does not name the neighbor");
        assert_eq!(approved.id, 7);
    }

    #[test]
    fn globally_unique_exact_model_uses_the_resolved_manufacturer_identity() {
        let approved = known_approved_local_match(
            &gma_1347_model_only_request(),
            &["Audio Panel".to_string()],
            1,
            &[gma_1347_known_candidate()],
            &[gma_1347_catalog_candidate("approved")],
        )
        .expect("the resolved maker plus a unique exact product label is sufficient");
        assert_eq!(approved.id, 2);
    }

    #[test]
    fn model_only_match_requires_the_resolved_request_manufacturer_identity() {
        assert!(
            known_approved_local_match(
                &gma_1347_model_only_request(),
                &["Audio Panel".to_string()],
                99,
                &[gma_1347_known_candidate()],
                &[gma_1347_catalog_candidate("approved")],
            )
            .is_none(),
            "the retained model cannot substitute for the request's resolved manufacturer identity"
        );
    }

    #[test]
    fn model_only_match_requires_an_exact_complete_model_occurrence() {
        let request = AvionicsIdentityRequest {
            listing_context: "GMA-1347D Digital Audio Panel".to_string(),
            ..gma_1347_model_only_request()
        };
        assert!(
            known_approved_local_match(
                &request,
                &["Audio Panel".to_string()],
                1,
                &[gma_1347_known_candidate()],
                &[gma_1347_catalog_candidate("approved")],
            )
            .is_none(),
            "a longer token cannot satisfy the exact compact full-model boundary"
        );
    }

    #[test]
    fn model_only_match_rejects_exact_model_or_stable_identifier_duplicates() {
        let known = [gma_1347_known_candidate()];
        let approved = gma_1347_catalog_candidate("approved");

        let mut duplicate_model = gma_1347_catalog_candidate("unreviewed");
        duplicate_model.id = 90;
        duplicate_model.manufacturer_identifier = "011-DIFFERENT-90".to_string();
        assert!(
            known_approved_local_match(
                &gma_1347_model_only_request(),
                &["Audio Panel".to_string()],
                1,
                &known,
                &[approved.clone(), duplicate_model],
            )
            .is_none(),
            "an unreviewed exact-model duplicate must block manufacturer-free reuse"
        );

        let mut duplicate_identifier = gma_1347_catalog_candidate("unreviewed");
        duplicate_identifier.id = 91;
        duplicate_identifier.model = "Different Audio Panel".to_string();
        duplicate_identifier.manufacturer_identifier_kind = "sku".to_string();
        assert!(
            known_approved_local_match(
                &gma_1347_model_only_request(),
                &["Audio Panel".to_string()],
                1,
                &known,
                &[approved, duplicate_identifier],
            )
            .is_none(),
            "a cross-kind stable-identifier collision must block manufacturer-free reuse"
        );
    }

    #[test]
    fn model_only_match_rejects_a_cross_manufacturer_model_collision() {
        let mut cross_manufacturer = gma_1347_catalog_candidate("unreviewed");
        cross_manufacturer.id = 92;
        cross_manufacturer.manufacturer = "Other Avionics".to_string();
        cross_manufacturer.manufacturer_identifier = "OA-1347".to_string();
        assert!(
            known_approved_local_match(
                &gma_1347_model_only_request(),
                &["Audio Panel".to_string()],
                1,
                &[gma_1347_known_candidate()],
                &[gma_1347_catalog_candidate("approved"), cross_manufacturer,],
            )
            .is_none(),
            "an omitted manufacturer is unsafe when another manufacturer uses the exact model"
        );
    }

    #[test]
    fn exact_longer_model_is_not_ambiguous_with_a_shorter_family_member() {
        let mut selected = known_candidate(22, "GDL 69A");
        selected.avionics_types = vec!["Datalink".to_string()];
        let mut selected_catalog = candidate(22, "GDL 69A", "approved");
        selected_catalog.avionics_types = vec!["Datalink".to_string()];
        selected_catalog.manufacturer_identifier = selected.manufacturer_identifier.clone();
        let mut shorter_known = known_candidate(93, "GDL 69");
        shorter_known.avionics_types = vec!["Datalink".to_string()];
        let mut shorter_neighbor = candidate(93, "GDL 69", "approved");
        shorter_neighbor.avionics_types = vec!["Datalink".to_string()];

        let request = AvionicsIdentityRequest {
            model: "GDL-69A".to_string(),
            avionics_types: vec!["Datalink".to_string()],
            listing_context: "GDL-69A satellite weather datalink".to_string(),
            ..local_request("")
        };
        let approved = known_approved_local_match(
            &request,
            &["Datalink".to_string()],
            1,
            &[selected, shorter_known],
            &[selected_catalog, shorter_neighbor],
        )
        .expect("the listing names the exact longer catalog product");
        assert_eq!(approved.id, 22);
    }

    #[test]
    fn exact_catalog_model_can_include_a_verbose_marketing_name() {
        let mut selected = known_candidate(24, "G1000 Integrated Flight Deck");
        selected.avionics_types = vec!["Integrated Flight Deck".to_string()];
        selected.manufacturer_identifier_kind = "manufacturer_model_number".to_string();
        selected.manufacturer_identifier = "G1000 Integrated Flight Deck".to_string();
        selected.canonical_identifier_key = selected.canonical_product_key.clone();
        let mut selected_catalog = candidate(24, "G1000 Integrated Flight Deck", "approved");
        selected_catalog.avionics_types = vec!["Integrated Flight Deck".to_string()];
        selected_catalog.manufacturer_identifier_kind =
            selected.manufacturer_identifier_kind.clone();
        selected_catalog.manufacturer_identifier = selected.manufacturer_identifier.clone();
        let mut shorter_neighbor = candidate(25, "G1000", "unreviewed");
        shorter_neighbor.avionics_types = vec!["Integrated Flight Deck".to_string()];

        let request = AvionicsIdentityRequest {
            model: selected.model.clone(),
            avionics_types: selected.avionics_types.clone(),
            listing_context: "G1000 Integrated Flight Deck".to_string(),
            ..local_request("")
        };
        let approved = known_approved_local_match(
            &request,
            &["Integrated Flight Deck".to_string()],
            1,
            &[selected],
            &[selected_catalog, shorter_neighbor],
        )
        .expect("the complete approved catalog model is literally present");
        assert_eq!(approved.id, 24);
    }

    #[test]
    fn legacy_manufacturer_model_number_identity_does_not_require_a_part_number() {
        let mut selected = known_candidate(26, "G1000 NXi");
        selected.avionics_types = vec!["Integrated Flight Deck".to_string()];
        selected.manufacturer_identifier_kind = "manufacturer_model_number".to_string();
        selected.manufacturer_identifier = "G1000 NXi".to_string();
        selected.canonical_identifier_key = selected.canonical_product_key.clone();
        let mut selected_catalog = candidate(26, "G1000 NXi", "approved");
        selected_catalog.avionics_types = vec!["Integrated Flight Deck".to_string()];
        selected_catalog.manufacturer_identifier_kind =
            selected.manufacturer_identifier_kind.clone();
        selected_catalog.manufacturer_identifier = selected.manufacturer_identifier.clone();
        let mut shorter_neighbor = candidate(27, "G1000", "unreviewed");
        shorter_neighbor.avionics_types = vec!["Integrated Flight Deck".to_string()];
        let request = AvionicsIdentityRequest {
            model: selected.model.clone(),
            avionics_types: selected.avionics_types.clone(),
            listing_context: "G1000 NXi Integrated Flight Deck".to_string(),
            ..local_request("")
        };

        let approved = known_approved_local_match(
            &request,
            &["Integrated Flight Deck".to_string()],
            1,
            &[selected],
            &[selected_catalog, shorter_neighbor],
        )
        .expect("an attested exact manufacturer model number is a usable legacy identity");
        assert_eq!(approved.id, 26);
    }

    #[test]
    fn exact_base_model_is_usable_when_the_listing_does_not_name_the_neighbor() {
        let mut longer_neighbor = gma_1347_catalog_candidate("unreviewed");
        longer_neighbor.id = 94;
        longer_neighbor.model = "GMA 1347D".to_string();
        longer_neighbor.manufacturer_identifier = "011-NEIGHBOR-94".to_string();
        let approved = known_approved_local_match(
            &gma_1347_model_only_request(),
            &["Audio Panel".to_string()],
            1,
            &[gma_1347_known_candidate()],
            &[gma_1347_catalog_candidate("approved"), longer_neighbor],
        )
        .expect("literal exact evidence does not have to disprove an omitted suffix");
        assert_eq!(approved.id, 2);
    }

    #[test]
    fn model_only_match_rejects_an_unverified_capability() {
        assert!(
            known_approved_local_match(
                &gma_1347_model_only_request(),
                &["Audio Panel".to_string(), "COM".to_string()],
                1,
                &[gma_1347_known_candidate()],
                &[gma_1347_catalog_candidate("approved")],
            )
            .is_none(),
            "model-only evidence cannot expand an approved product's capabilities"
        );
    }

    #[test]
    fn bounded_adjudication_handles_nonadjacent_maker_without_mutating_identity() {
        let request = AvionicsIdentityRequest {
            model: "GMA-1347".to_string(),
            avionics_types: vec!["Audio Panel".to_string()],
            listing_context: "Garmin G1000 NXi Integrated Avionics Suite\nGMA-1347 Digital Audio Panel w/ Intercom".to_string(),
            ..local_request("")
        };
        let known = vec![gma_1347_known_candidate()];
        let mut neighbor = gma_1347_catalog_candidate("unreviewed");
        neighbor.id = 99;
        neighbor.model = "GMA 1347D".to_string();
        neighbor.manufacturer_identifier = "011-UNREVIEWED-99".to_string();
        let catalog = vec![gma_1347_catalog_candidate("approved"), neighbor];
        assert_eq!(
            known_approved_local_match(
                &request,
                &["Audio Panel".to_string()],
                1,
                &known,
                &catalog,
            )
            .expect("exact local evidence should resolve before bounded adjudication")
            .id,
            2
        );

        let plan = approved_candidate_adjudication_plan(
            &request,
            &["Audio Panel".to_string()],
            1,
            &known,
            &catalog,
        )
        .expect("the graph-approved exact product should reach bounded adjudication");
        assert_eq!(
            plan.context
                .approved_candidates
                .iter()
                .map(|candidate| candidate.id)
                .collect::<Vec<_>>(),
            vec![2]
        );
        let response = json!({
            "decision": "same",
            "selected_catalog_id": 2,
            "confidence": "very_high",
            "evidence_text": "GMA-1347 Digital Audio Panel w/ Intercom",
            "reason": "The exact observed product label differs only in punctuation."
        });
        assert_eq!(
            approved_candidate_adjudication_selection(&request, &plan, &response),
            Some(2)
        );
        assert_eq!(
            plan.context.approved_candidates[0].avionics_types,
            vec!["Audio Panel"]
        );
    }

    #[test]
    fn bounded_adjudication_response_is_fail_closed() {
        let request = AvionicsIdentityRequest {
            model: "GMA-1347".to_string(),
            avionics_types: vec!["Audio Panel".to_string()],
            listing_context: "Garmin G1000 NXi\nGMA-1347 Digital Audio Panel w/ Intercom"
                .to_string(),
            ..local_request("")
        };
        let known = vec![gma_1347_known_candidate()];
        let catalog = vec![gma_1347_catalog_candidate("approved")];
        let plan = approved_candidate_adjudication_plan(
            &request,
            &["Audio Panel".to_string()],
            1,
            &known,
            &catalog,
        )
        .expect("candidate should be eligible");
        let valid = json!({
            "decision": "same",
            "selected_catalog_id": 2,
            "confidence": "very_high",
            "evidence_text": "GMA-1347 Digital Audio Panel w/ Intercom",
            "reason": "Exact supplied identity."
        });

        for invalid in [
            {
                let mut value = valid.clone();
                value["selected_catalog_id"] = json!(999);
                value
            },
            {
                let mut value = valid.clone();
                value["confidence"] = json!("high");
                value
            },
            {
                let mut value = valid.clone();
                value["evidence_text"] = json!("GMA 1347");
                value
            },
            {
                let mut value = valid.clone();
                value["evidence_text"] = json!("Digital Audio Panel");
                value
            },
            {
                let mut value = valid.clone();
                value["decision"] = json!("uncertain");
                value["selected_catalog_id"] = json!(0);
                value
            },
            {
                let mut value = valid.clone();
                value["model_supplied_mutation"] = json!("GMA 1347D");
                value
            },
        ] {
            assert_eq!(
                approved_candidate_adjudication_selection(&request, &plan, &invalid),
                None
            );
        }
    }

    #[test]
    fn bounded_adjudication_never_expands_capabilities_or_accepts_duplicates() {
        let request = AvionicsIdentityRequest {
            model: "GMA-1347".to_string(),
            avionics_types: vec!["Audio Panel".to_string(), "COM".to_string()],
            listing_context: "Garmin avionics\nGMA-1347 Audio Panel".to_string(),
            ..local_request("")
        };
        let known = vec![gma_1347_known_candidate()];
        assert!(
            approved_candidate_adjudication_plan(
                &request,
                &["Audio Panel".to_string(), "COM".to_string()],
                1,
                &known,
                &[gma_1347_catalog_candidate("approved")],
            )
            .is_none(),
            "an observed capability outside the approved set must use grounding"
        );

        let mut duplicate = gma_1347_catalog_candidate("unreviewed");
        duplicate.id = 99;
        duplicate.model = "GMA-1347".to_string();
        assert!(
            approved_candidate_adjudication_plan(
                &AvionicsIdentityRequest {
                    avionics_types: vec!["Audio Panel".to_string()],
                    ..request
                },
                &["Audio Panel".to_string()],
                1,
                &known,
                &[gma_1347_catalog_candidate("approved"), duplicate],
            )
            .is_none(),
            "an active exact catalog collision must block cheap acceptance"
        );
    }

    #[test]
    fn known_approved_loader_requires_graph_identity_membership() {
        assert!(
            KNOWN_APPROVED_SELECT_SQL.contains("JOIN avionics_approved_product_identities"),
            "catalog_status alone must never make a row eligible for source-free matching"
        );
        assert!(
            KNOWN_APPROVED_SELECT_SQL.contains("approved_identity.avionics_model_id = model.id")
        );
    }

    #[test]
    fn local_match_rejects_a_missing_variant_but_accepts_typography_only_equality() {
        let candidates = vec![known_candidate(7, "GTX 345R")];
        let mut request = local_request("Garmin GTX 345 installed");
        request.model = "GTX 345".to_string();
        assert!(
            known_approved_local_match(
                &request,
                &["Transponder".to_string()],
                1,
                &candidates,
                &[candidate(7, "GTX 345R", "approved")],
            )
            .is_none(),
            "a missing remote-unit suffix must fall through to grounding"
        );

        request.model = "GTX345R".to_string();
        request.listing_context = "Garmin GTX 345R installed".to_string();
        assert_eq!(
            known_approved_local_match(
                &request,
                &["Transponder".to_string()],
                1,
                &candidates,
                &[candidate(7, "GTX 345R", "approved")],
            )
            .expect("spacing differences normalize to the complete same model")
            .id,
            7
        );
    }

    #[test]
    fn exact_stable_identifier_can_prove_a_local_match() {
        let mut request = local_request("Garmin remote transponder P/N 011-KNOWN-7 installed");
        request.model = "011-KNOWN-7".to_string();
        let candidates = vec![known_candidate(7, "GTX 345R")];
        let approved = known_approved_local_match(
            &request,
            &["Transponder".to_string()],
            1,
            &candidates,
            &[candidate(7, "GTX 345R", "approved")],
        )
        .expect("an exact stable identifier is independent product evidence");
        assert_eq!(approved.id, 7);
    }

    #[test]
    fn local_match_refuses_ambiguity_and_duplicate_approved_identities() {
        let request = local_request("Garmin GTX345R installed");
        let mut duplicate = known_candidate(8, "GTX-345R");
        duplicate.manufacturer_identifier = "011-KNOWN-8".to_string();
        let candidates = vec![known_candidate(7, "GTX 345R"), duplicate];
        assert!(
            known_approved_local_match(
                &request,
                &["Transponder".to_string()],
                1,
                &candidates,
                &[
                    candidate(7, "GTX 345R", "approved"),
                    candidate(8, "GTX-345R", "approved"),
                ],
            )
            .is_none(),
            "duplicate approved identities must be consolidated before fast matching"
        );
    }

    #[test]
    fn exact_base_product_is_accepted_but_an_explicit_longer_variant_is_not() {
        let request = AvionicsIdentityRequest {
            model: "G1000".to_string(),
            listing_context: "Garmin G1000 installed".to_string(),
            ..local_request("")
        };
        let mut base = known_candidate(7, "G1000");
        base.manufacturer_identifier = "G1000-BASE-ID".to_string();
        base.canonical_identifier_key =
            normalize_avionics_identifier(&base.manufacturer_identifier);
        let nxi = known_candidate(8, "G1000 NXi");
        let candidates = vec![base.clone(), nxi];
        let catalog = vec![
            candidate(7, "G1000", "approved"),
            candidate(8, "G1000 NXi", "approved"),
        ];

        assert_eq!(
            known_approved_local_match(
                &request,
                &["Transponder".to_string()],
                1,
                &candidates,
                &catalog,
            )
            .expect("the listing literally names the base product")
            .id,
            7
        );

        let qualified_request = AvionicsIdentityRequest {
            listing_context: "Garmin G1000 NXi installed".to_string(),
            ..request.clone()
        };
        assert!(
            known_approved_local_match(
                &qualified_request,
                &["Transponder".to_string()],
                1,
                &candidates,
                &catalog,
            )
            .is_none(),
            "an explicitly named longer variant must not collapse into the base product"
        );

        let identified_request = AvionicsIdentityRequest {
            model: "G1000-BASE-ID".to_string(),
            listing_context: "Garmin G1000 NXi airframe with unit G1000-BASE-ID installed"
                .to_string(),
            ..request
        };
        let selected = known_approved_local_match(
            &identified_request,
            &["Transponder".to_string()],
            1,
            &candidates,
            &catalog,
        )
        .expect("an exact stable identifier may disambiguate a prefix collision");
        assert_eq!(selected.id, 7);
    }

    #[test]
    fn local_match_falls_through_on_an_unverified_extra_capability() {
        let request = local_request("Garmin GTX345R installed");
        let candidates = vec![known_candidate(7, "GTX 345R")];
        assert!(
            known_approved_local_match(
                &request,
                &["Transponder".to_string(), "GPS".to_string()],
                1,
                &candidates,
                &[candidate(7, "GTX 345R", "approved")],
            )
            .is_none(),
            "the local path cannot enrich an approved product"
        );
    }

    #[test]
    fn expanded_curated_capabilities_are_accepted_and_deduplicated() {
        for capability in [
            "ELT",
            "ADF",
            "DME",
            "AHRS",
            "Air Data Computer",
            "Navigation Indicator",
            "Weather Radar",
            "Lightning Detection",
            "Radar Altimeter",
            "Magnetometer",
            "Clock/Timer",
        ] {
            let response = json!({"canonical_types": [capability, capability]});
            assert_eq!(
                canonical_types_from_response(&response, "canonical_types")
                    .expect("curated capability should validate"),
                vec![capability.to_string()]
            );
        }
        assert_eq!(
            canonical_avionics_types_for_label("CDI/HSI"),
            vec!["Navigation Indicator"]
        );
        assert_eq!(
            canonical_avionics_types_for_label("attitude and heading reference system"),
            vec!["AHRS"]
        );
        assert_eq!(
            canonical_avionics_types_for_label("emergency locator transmitter"),
            vec!["ELT"]
        );
        assert_eq!(
            canonical_avionics_types_for_label("NAV/COM"),
            vec!["NAV", "COM"]
        );
        assert!(canonical_types_from_response(
            &json!({"canonical_types": ["NAV/COM"]}),
            "canonical_types"
        )
        .is_err());
    }

    #[test]
    fn gnx_375_is_one_identity_with_gps_and_transponder_capabilities() {
        let response = json!({
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GNX 375",
            "canonical_types": ["Transponder", "GPS", "GPS"],
            "manufacturer_identifier_kind": "manufacturer_model_number",
            "manufacturer_identifier": "GNX 375",
            "manufacturer_identifier_scope": "exact_catalog_product",
            "identity_source_url": "https://www.garmin.com/gnx-375",
            "identity_source_title": "Garmin GNX 375",
            "identity_evidence": "Garmin identifies GNX 375 as one GPS navigator with a transponder.",
            "reason": "The manufacturer documents both functions on one product."
        });
        let identity = verified_identity_from_response(&response)
            .expect("multifunction identity should validate");
        assert_eq!(
            identity.canonical_types,
            vec!["GPS".to_string(), "Transponder".to_string()]
        );
    }

    #[test]
    fn official_model_number_identity_accepts_one_specific_model_occurrence() {
        let source_url = "https://static.garmin.com/manuals/gia63w.pdf";
        let evidence = "Garmin GIA 63W";
        let mut issues = Vec::new();

        validate_evidence_values(
            source_url,
            "GIA 63W manual",
            evidence,
            "Garmin",
            "GIA 63W",
            "GIA 63W",
            "https://listing.example/aircraft/1",
            &[],
            &mut issues,
        );

        assert!(
            issues.iter().any(|issue| issue.contains(
                "exact server-fetched publisher span bound to the final source URL and content digest"
            )),
            "grounding-only evidence must not approve a product identity: {issues:?}"
        );

        let proof = direct_source_proof(source_url, evidence);
        let mut direct_source_issues = Vec::new();
        validate_evidence_values(
            source_url,
            "GIA 63W manual",
            evidence,
            "Garmin",
            "GIA 63W",
            "GIA 63W",
            "https://listing.example/aircraft/1",
            &[proof],
            &mut direct_source_issues,
        );
        assert!(
            direct_source_issues.is_empty(),
            "unexpected direct-source issues: {direct_source_issues:?}"
        );
    }

    #[test]
    fn short_incidental_model_token_is_not_sufficient_identity_evidence() {
        let source_url = "https://static.garmin.com/manuals/g5.pdf";
        let evidence = "Garmin G5";
        let proof = direct_source_proof(source_url, evidence);
        let mut issues = Vec::new();

        validate_evidence_values(
            source_url,
            "G5 manual",
            evidence,
            "Garmin",
            "G5",
            "G5",
            "https://listing.example/aircraft/1",
            &[proof],
            &mut issues,
        );

        assert!(issues
            .iter()
            .any(|issue| issue.contains("specific supporting fact")));
    }

    #[test]
    fn identity_evidence_rejects_non_https_source_claims() {
        let source_url = "http://static.garmin.com/manuals/gia63w.pdf";
        let evidence = "Garmin GIA 63W";
        let mut issues = Vec::new();

        validate_evidence_values(
            source_url,
            "GIA 63W manual",
            evidence,
            "Garmin",
            "GIA 63W",
            "GIA 63W",
            "https://listing.example/aircraft/1",
            &[],
            &mut issues,
        );

        assert!(issues.iter().any(|issue| issue.contains("final HTTPS URL")));
    }

    #[test]
    fn model_designation_cannot_be_mislabeled_as_a_part_number() {
        let response = json!({
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GIA 63W",
            "canonical_types": ["NAV", "COM"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "GIA 63W",
            "manufacturer_identifier_scope": "exact_catalog_product",
            "identity_source_url": "https://static.garmin.com/manuals/gia63w.pdf",
            "identity_source_title": "GIA 63W manual",
            "identity_evidence": "Garmin identifies the GIA 63W model.",
            "reason": "The official manual identifies the exact model."
        });

        let error = verified_identity_from_response(&response)
            .expect_err("a model designation is not an OEM part number");
        assert!(error
            .to_string()
            .contains("must use manufacturer_model_number"));
    }

    #[test]
    fn approved_match_requires_exact_server_identity_and_authoritative_evidence() {
        let context = context(vec![candidate(1, "GTX 345R", "approved")]);
        let response = json!({
            "status": "existing_match",
            "catalog_id": 1,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-TEST-1",
            "manufacturer_identifier_scope": "exact_catalog_product",
            "rejection_basis": "none",
            "confidence": "very_high",
            "identity_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
            "identity_source_title": "GTX 345R installation manual",
            "identity_evidence": "The manual identifies the remote GTX 345R as part number 011-TEST-1.",
            "reason": "Official installation manual establishes the identity."
        });
        let (sources, supports) = grounding(
            "https://static.garmin.com/manuals/gtx345r.pdf",
            "GTX 345R installation manual",
            "The manual identifies the remote GTX 345R as part number 011-TEST-1.",
        );
        assert!(
            resolution_issues(&context, &response, true, &sources, &supports)
                .iter()
                .any(|issue| issue.contains("exact server-fetched publisher span")),
            "Gemini-authored citation prose must not authorize a positive identity"
        );
        assert!(
            resolution_issues(&context, &response, false, &sources, &supports)
                .iter()
                .any(|issue| issue.contains("verified Search + URL Context"))
        );

        let mut direct_context = context;
        direct_context.authoritative_direct_source_urls =
            vec!["https://static.garmin.com/manuals/gtx345r.pdf".to_string()];
        direct_context.authoritative_identity_anchors =
            vec!["Garmin".to_string(), "GTX 345R".to_string()];
        assert!(
            resolution_issues(&direct_context, &response, false, &sources, &supports)
                .iter()
                .any(|issue| issue.contains("verified Search + URL Context")),
            "a raw caller URL must not authorize a catalog identity"
        );
        let direct_source_proofs = [direct_source_proof(
            "https://static.garmin.com/manuals/gtx345r.pdf",
            "The manual identifies the remote GTX 345R as part number 011-TEST-1.",
        )];
        assert!(
            resolution_issues_with_direct_source_proofs(
                &direct_context,
                &response,
                true,
                &sources,
                &[],
                &direct_source_proofs,
            )
            .is_empty(),
            "an exact server-fetched publisher span is sufficient when Gemini omits support metadata"
        );
        assert!(
            resolution_issues_with_direct_source_proofs(
                &direct_context,
                &response,
                false,
                &sources,
                &[],
                &direct_source_proofs,
            )
            .iter()
            .any(|issue| issue.contains("verified Search + URL Context")),
            "publisher proof cannot replace the verified-grounding provenance gate"
        );
    }

    #[test]
    fn positive_resolution_rejects_component_of_catalog_product_identifier_scope() {
        let context = context(vec![]);
        let response = json!({
            "status": "propose_new",
            "catalog_id": 0,
            "canonical_manufacturer": "3M",
            "canonical_model": "WX-10A Stormscope",
            "canonical_types": ["Lightning Detection"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "78-8060-5900-8",
            "manufacturer_identifier_scope": "component_of_catalog_product",
            "rejection_basis": "none",
            "confidence": "very_high",
            "identity_source_url": "https://manufacturer.example/wx-10a-manual.pdf",
            "identity_source_title": "WX-10A service manual",
            "identity_evidence": "The cited part number identifies one component in the WX-10A system.",
            "reason": "The identifier does not scope the complete multi-box product."
        });

        let issues = resolution_issues(&context, &response, true, &[], &[]);
        assert!(issues.iter().any(|issue| {
            issue.contains("manufacturer_identifier_scope=exact_catalog_product")
        }));
        assert!(verified_identity_from_response(&response)
            .unwrap_err()
            .to_string()
            .contains("component"));
    }

    #[test]
    fn approved_match_can_only_enrich_an_observed_capability_as_a_monotonic_union() {
        let mut approved = candidate(1, "GNX 375", "approved");
        approved.manufacturer_identifier_kind = "manufacturer_model_number".to_string();
        approved.manufacturer_identifier = "GNX 375".to_string();
        let mut context = context(vec![approved]);
        context.listing_context = "Garmin GNX 375 navigator/transponder installed".to_string();
        context.candidate.model = "GNX 375".to_string();
        context.candidate.avionics_types = vec!["GPS".to_string()];
        let mut response = json!({
            "status": "existing_match",
            "catalog_id": 1,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GNX 375",
            "canonical_types": ["GPS", "Transponder"],
            "manufacturer_identifier_kind": "manufacturer_model_number",
            "manufacturer_identifier": "GNX 375",
            "manufacturer_identifier_scope": "exact_catalog_product",
            "rejection_basis": "none",
            "confidence": "very_high",
            "identity_source_url": "https://static.garmin.com/manuals/gnx375.pdf",
            "identity_source_title": "Garmin GNX 375 pilot guide",
            "identity_evidence": "Garmin documents the GNX 375 as a GPS navigator with an integrated transponder.",
            "reason": "The official guide verifies both capabilities on the exact product."
        });
        let (sources, supports) = grounding(
            "https://static.garmin.com/manuals/gnx375.pdf",
            "Garmin GNX 375 pilot guide",
            "Garmin documents the GNX 375 as a GPS navigator with an integrated transponder.",
        );
        let proofs = [direct_source_proof(
            "https://static.garmin.com/manuals/gnx375.pdf",
            "Garmin documents the GNX 375 as a GPS navigator with an integrated transponder.",
        )];
        assert!(resolution_issues_with_direct_source_proofs(
            &context, &response, true, &sources, &supports, &proofs,
        )
        .is_empty());

        response["canonical_types"] = json!(["Transponder"]);
        let issues = resolution_issues_with_direct_source_proofs(
            &context, &response, true, &sources, &supports, &proofs,
        );
        assert!(issues
            .iter()
            .any(|issue| issue.contains("newly observed capability \"GPS\" or return unresolved")));
    }

    #[test]
    fn new_identity_requires_very_high_confidence() {
        let context = context(vec![]);
        let response = json!({
            "status": "propose_new",
            "catalog_id": 0,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "manufacturer_identifier_scope": "exact_catalog_product",
            "rejection_basis": "none",
            "confidence": "high",
            "identity_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
            "identity_source_title": "GTX 345R installation manual",
            "identity_evidence": "The manual identifies the part number.",
            "reason": "Official source."
        });
        assert!(resolution_issues(&context, &response, true, &[], &[])
            .iter()
            .any(|issue| issue.contains("very_high")));
    }

    #[test]
    fn new_identity_rejects_combined_model_labels() {
        let context = context(vec![]);
        let response = json!({
            "status": "propose_new",
            "catalog_id": 0,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GNS 430/530",
            "canonical_types": ["NAV", "COM"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-00000-00",
            "manufacturer_identifier_scope": "exact_catalog_product",
            "rejection_basis": "none",
            "confidence": "very_high",
            "identity_source_url": "https://static.garmin.com/manuals/gns.pdf",
            "identity_source_title": "GNS manual",
            "identity_evidence": "The document describes two separate units.",
            "reason": "Combined label."
        });
        assert!(resolution_issues(&context, &response, true, &[], &[])
            .iter()
            .any(|issue| issue.contains("combined model label")));
    }

    #[test]
    fn collision_source_url_defects_are_correctable_domain_issues() {
        let proposal_url = "https://static.garmin.com/manuals/gtx345r.pdf";
        let proposal_evidence =
            "The manufacturer manual identifies GTX 345R as part number 011-03520-00.";
        let candidate_url = "https://static.garmin.com/manuals/gtx345.pdf";
        let candidate_evidence =
            "The manufacturer manual identifies GTX 345 as part number 011-TEST-1.";
        let context = context(vec![candidate(1, "GTX 345", "approved")]);
        let proposed = verified_identity();
        let mut response = json!({
            "proposal_decision": "confirmed_same_as_input",
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "proposal_manufacturer_identifier_scope": "exact_catalog_product",
            "proposal_confidence": "very_high",
            "input_evidence_text": "GTX345R",
            "proposal_source_url": proposal_url,
            "proposal_source_title": "GTX 345R installation manual",
            "proposal_evidence": proposal_evidence,
            "proposal_reason": "The listing and manufacturer manual identify the exact unit.",
            "reviews": [{
                "catalog_id": 1,
                "decision": "different_product",
                "confidence": "very_high",
                "candidate_source_url": format!("http://{}", candidate_url.trim_start_matches("https://")),
                "candidate_source_title": "GTX 345 installation manual",
                "candidate_evidence": candidate_evidence,
                "reason": "The panel and remote units have different manufacturer identifiers."
            }]
        });
        let http_candidate_url = format!("http://{}", candidate_url.trim_start_matches("https://"));
        let initial_proofs = [
            direct_source_proof(proposal_url, proposal_evidence),
            direct_source_proof(&http_candidate_url, candidate_evidence),
        ];
        let issues = collision_response_issues(&context, &proposed, &response, &initial_proofs);
        assert!(
            issues.iter().any(|issue| issue.contains("final HTTPS URL")),
            "an HTTP candidate URL must enter bounded domain correction: {issues:?}"
        );

        response["reviews"][0]["candidate_source_url"] = json!(candidate_url);
        let corrected_proofs = [
            direct_source_proof(proposal_url, proposal_evidence),
            direct_source_proof(candidate_url, candidate_evidence),
        ];
        assert!(
            collision_response_issues(&context, &proposed, &response, &corrected_proofs).is_empty(),
            "a corrected final HTTPS candidate URL with the exact publisher proof must pass"
        );

        let http_proposal_url = format!("http://{}", proposal_url.trim_start_matches("https://"));
        response["proposal_source_url"] = json!(http_proposal_url);
        let http_proposal_proofs = [
            direct_source_proof(
                response["proposal_source_url"].as_str().unwrap(),
                proposal_evidence,
            ),
            direct_source_proof(candidate_url, candidate_evidence),
        ];
        let issues =
            collision_response_issues(&context, &proposed, &response, &http_proposal_proofs);
        assert!(
            issues.iter().any(|issue| issue.contains("final HTTPS URL")),
            "an HTTP proposal URL must remain invalid when correction does not repair it: {issues:?}"
        );
    }

    #[test]
    fn collision_review_must_cover_every_shortlist_candidate_once() {
        let context = context(vec![
            candidate(1, "GTX 345", "approved"),
            candidate(2, "GTX 345R", "unreviewed"),
        ]);
        let response = json!({
            "canonical_model": "GTX 345R",
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "reviews": [{
                "catalog_id": 1,
                "decision": "different_product",
                "confidence": "very_high",
                "candidate_source_url": "https://static.garmin.com/manuals/gtx345.pdf",
                "candidate_source_title": "GTX 345 manual",
                "candidate_evidence": "The manual identifies panel GTX 345 part number 011-TEST-1.",
                "reason": "Different form factor and part number."
            }]
        });
        let proofs = [direct_source_proof(
            "https://static.garmin.com/manuals/gtx345.pdf",
            "The manual identifies panel GTX 345 part number 011-TEST-1.",
        )];
        let error =
            collision_reviews_with_direct_source_proofs(&context, &response, &proofs).unwrap_err();
        assert!(error.to_string().contains("omitted"));
    }

    #[test]
    fn collision_review_rejects_proof_that_omits_the_reviewed_candidate_identifier() {
        let context = context(vec![candidate(1, "GTX 345", "approved")]);
        let evidence = "The manual distinguishes panel GTX 345 from remote GTX 345R manufacturer part number 011-03520-00.";
        let response = json!({
            "canonical_model": "GTX 345R",
            "manufacturer_identifier": "011-03520-00",
            "reviews": [{
                "catalog_id": 1,
                "decision": "different_product",
                "confidence": "very_high",
                "candidate_source_url": "https://static.garmin.com/manuals/gtx345.pdf",
                "candidate_source_title": "GTX 345 installation manual",
                "candidate_evidence": evidence,
                "reason": "The source identifies different panel and remote products."
            }]
        });
        let proofs = [direct_source_proof(
            "https://static.garmin.com/manuals/gtx345.pdf",
            evidence,
        )];

        let error = collision_reviews_with_direct_source_proofs(&context, &response, &proofs)
            .expect_err("proposal-only evidence cannot adjudicate a catalog candidate");
        assert!(error
            .to_string()
            .contains("canonical model and manufacturer identifier"));
    }

    #[test]
    fn collision_review_accepts_model_only_proof_for_legacy_candidate_without_identifier() {
        let source_url = "https://static.garmin.com/manuals/gia63-series.pdf";
        let candidate_evidence =
            "The manufacturer equipment table identifies the legacy GIA 63 unit.";
        let mut legacy_gia = candidate(1, "GIA 63", "unreviewed");
        legacy_gia.manufacturer_identifier_kind = "none".to_string();
        legacy_gia.manufacturer_identifier.clear();
        let context = context(vec![legacy_gia]);
        let response = json!({
            "canonical_model": "GIA 63W",
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "010-00386-00",
            "reviews": [{
                "catalog_id": 1,
                "decision": "different_product",
                "confidence": "very_high",
                "candidate_source_url": source_url,
                "candidate_source_title": "GIA 63 series equipment table",
                "candidate_evidence": candidate_evidence,
                "reason": "The candidate is a distinct legacy model without a stored identifier."
            }]
        });
        let proofs = [direct_source_proof(source_url, candidate_evidence)];

        let reviews = collision_reviews_with_direct_source_proofs(&context, &response, &proofs)
            .expect("an exact model row should prove a legacy candidate with no identifier");
        assert_eq!(reviews.len(), 1);
    }

    #[test]
    fn collision_review_accepts_gia_variants_proven_by_separate_exact_table_rows() {
        let source_url = "https://static.garmin.com/manuals/gia63-series.pdf";
        let proposal_evidence = "GIA 63W Unit Only, Garmin part number 010-00386-00.";
        let candidate_evidence = "GIA 63 Unit Only, Garmin part number 010-00386-01.";
        let mut legacy_gia = candidate(1, "GIA 63", "approved");
        legacy_gia.avionics_types = vec!["NAV".to_string(), "COM".to_string()];
        legacy_gia.manufacturer_identifier = "010-00386-01".to_string();
        let mut context = context(vec![legacy_gia]);
        context.listing_context = "Garmin GIA 63W installed".to_string();
        context.candidate.model = "GIA 63W".to_string();
        context.candidate.avionics_types = vec!["NAV".to_string(), "COM".to_string()];
        let mut proposed = verified_identity();
        proposed.canonical_model = "GIA 63W".to_string();
        proposed.canonical_types = vec!["NAV".to_string(), "COM".to_string()];
        proposed.manufacturer_identifier = "010-00386-00".to_string();
        let response = json!({
            "proposal_decision": "confirmed_same_as_input",
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GIA 63W",
            "canonical_types": ["NAV", "COM"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "010-00386-00",
            "proposal_manufacturer_identifier_scope": "exact_catalog_product",
            "proposal_confidence": "very_high",
            "input_evidence_text": "GIA 63W",
            "proposal_source_url": source_url,
            "proposal_source_title": "GIA 63 series equipment table",
            "proposal_evidence": proposal_evidence,
            "proposal_reason": "The listing and manufacturer table identify the W-suffix unit.",
            "reviews": [{
                "catalog_id": 1,
                "decision": "different_product",
                "confidence": "very_high",
                "candidate_source_url": source_url,
                "candidate_source_title": "GIA 63 series equipment table",
                "candidate_evidence": candidate_evidence,
                "reason": "The exact rows identify different suffixes and part numbers."
            }]
        });
        let proofs = [
            direct_source_proof(source_url, proposal_evidence),
            direct_source_proof(source_url, candidate_evidence),
        ];

        proposal_attestation_with_direct_source_proofs(&context, &proposed, &response, &proofs)
            .expect("the proposal row should independently prove GIA 63W");
        let reviews = collision_reviews_with_direct_source_proofs(&context, &response, &proofs)
            .expect("a separate exact candidate row should support different_product");
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].decision, "different_product");
    }

    #[test]
    fn collision_review_accepts_same_product_when_proposal_and_candidate_share_exact_signals() {
        let mut same_product = candidate(1, "GTX-345R", "unreviewed");
        same_product.manufacturer_identifier = "011 03520 00".to_string();
        let context = context(vec![same_product]);
        let evidence = "The manufacturer manual identifies GTX 345R as part number 011-03520-00.";
        let response = json!({
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "reviews": [{
                "catalog_id": 1,
                "decision": "same_product",
                "confidence": "very_high",
                "candidate_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
                "candidate_source_title": "GTX 345R installation manual",
                "candidate_evidence": evidence,
                "reason": "The candidate differs only by punctuation and spacing."
            }]
        });
        let proofs = [direct_source_proof(
            "https://static.garmin.com/manuals/gtx345r.pdf",
            evidence,
        )];

        let reviews = collision_reviews_with_direct_source_proofs(&context, &response, &proofs)
            .expect("one exact signal may prove harmless typography variants of one product");
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].decision, "same_product");
    }

    #[test]
    fn collision_review_rejects_same_product_for_distinct_exact_identifiers() {
        let context = context(vec![candidate(1, "GTX 345", "approved")]);
        let candidate_evidence =
            "The manufacturer manual identifies GTX 345 as part number 011-TEST-1.";
        let response = json!({
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "reviews": [{
                "catalog_id": 1,
                "decision": "same_product",
                "confidence": "very_high",
                "candidate_source_url": "https://static.garmin.com/manuals/gtx345.pdf",
                "candidate_source_title": "GTX 345 installation manual",
                "candidate_evidence": candidate_evidence,
                "reason": "The response claims these are one product."
            }]
        });
        let proofs = [direct_source_proof(
            "https://static.garmin.com/manuals/gtx345.pdf",
            candidate_evidence,
        )];

        let error = collision_reviews_with_direct_source_proofs(&context, &response, &proofs)
            .expect_err("different stable identifiers cannot support same_product");
        assert!(error
            .to_string()
            .contains(
                "cannot claim same_product without the same manufacturer and an exact stable-identifier match",
            ));
    }

    #[test]
    fn collision_review_rejects_different_product_for_same_exact_identifier() {
        let mut same_product = candidate(1, "GTX-345R", "unreviewed");
        same_product.manufacturer_identifier = "011 03520 00".to_string();
        let context = context(vec![same_product]);
        let candidate_evidence =
            "The manufacturer manual identifies GTX 345R as part number 011-03520-00.";
        let response = json!({
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "reviews": [{
                "catalog_id": 1,
                "decision": "different_product",
                "confidence": "very_high",
                "candidate_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
                "candidate_source_title": "GTX 345R installation manual",
                "candidate_evidence": candidate_evidence,
                "reason": "The response claims these are different products."
            }]
        });
        let proofs = [direct_source_proof(
            "https://static.garmin.com/manuals/gtx345r.pdf",
            candidate_evidence,
        )];

        let error = collision_reviews_with_direct_source_proofs(&context, &response, &proofs)
            .expect_err("one exact stable identity cannot support different_product");
        assert!(error.to_string().contains("cannot claim different_product"));
    }

    #[test]
    fn empty_shortlist_still_requires_exact_publisher_proposal_attestation() {
        let context = context(vec![]);
        let proposed = verified_identity();
        let mut response = json!({
            "proposal_decision": "confirmed_same_as_input",
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "proposal_manufacturer_identifier_scope": "exact_catalog_product",
            "proposal_confidence": "very_high",
            "input_evidence_text": "GTX345R",
            "proposal_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
            "proposal_source_title": "GTX 345R installation manual",
            "proposal_evidence": "The manufacturer manual identifies GTX 345R as part number 011-03520-00.",
            "proposal_reason": "The listing excerpt and official manual identify one exact unit.",
            "reviews": []
        });
        let error =
            proposal_attestation_with_direct_source_proofs(&context, &proposed, &response, &[])
                .expect_err("Gemini-authored citation prose must not approve a product identity");
        assert!(error.to_string().contains(
            "exact server-fetched publisher span bound to the final source URL and content digest"
        ));
        let direct_source_proofs = [direct_source_proof(
            "https://static.garmin.com/manuals/gtx345r.pdf",
            "The manufacturer manual identifies GTX 345R as part number 011-03520-00.",
        )];
        let attestation = proposal_attestation_with_direct_source_proofs(
            &context,
            &proposed,
            &response,
            &direct_source_proofs,
        )
        .expect("server-fetched publisher proof should attest the exact identity");
        assert!(attestation.confirmed);

        response["proposal_manufacturer_identifier_scope"] = json!("component_of_catalog_product");
        let error =
            proposal_attestation_with_direct_source_proofs(&context, &proposed, &response, &[])
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("proposal_manufacturer_identifier_scope=exact_catalog_product"));

        response["proposal_manufacturer_identifier_scope"] = json!("exact_catalog_product");
        response["input_evidence_text"] = json!("GTX 345R");
        let error =
            proposal_attestation_with_direct_source_proofs(&context, &proposed, &response, &[])
                .unwrap_err();
        assert!(error.to_string().contains("copied exactly"));
    }

    #[test]
    fn compact_listing_identity_requires_alphanumeric_boundaries() {
        assert!(
            exact_compact_identity_is_present(
                "Garmin GTX-345R remote transponder installed",
                &normalize_avionics_identifier("GTX345R"),
            ),
            "spaces and punctuation within an otherwise exact model identity remain valid"
        );
        assert!(
            exact_compact_identity_is_present(
                "Installed unit: GTX345R; coupled to the navigator",
                &normalize_avionics_identifier("GTX 345R"),
            ),
            "an exact model substring remains valid"
        );
        assert!(
            !exact_compact_identity_is_present(
                "Garmin G500 display installed",
                &normalize_avionics_identifier("G5"),
            ),
            "a shorter model must not match an alphanumeric prefix"
        );
        assert!(
            !exact_compact_identity_is_present(
                "Garmin GNS 430W navigator installed",
                &normalize_avionics_identifier("GNS 430"),
            ),
            "a missing alphanumeric suffix must not be inferred from listing evidence"
        );
        assert!(
            !exact_compact_identity_is_present(
                "Garmin GNS 430 navigator installed",
                &normalize_avionics_identifier("GNS 430W"),
            ),
            "a longer model must not match truncated listing evidence"
        );
    }

    #[test]
    fn exact_product_signal_is_boundary_safe_and_locally_coherent() {
        assert!(exact_product_identity_signal_is_present(
            "GIA 63W Unit Only, (011-01105-00) 010-00386-00",
            "GIA 63W",
            "010-00386-00",
        ));
        assert!(!exact_product_identity_signal_is_present(
            "GIA 63 Unit Only, (011-01105-00) 010-00386-00",
            "GIA 63W",
            "010-00386-00",
        ));

        let unrelated_multi_product_excerpt = format!(
            "The GIA 63W is listed here. {} A different product carries manufacturer identifier 010-00386-00.",
            (0..=MAX_EXACT_PRODUCT_SIGNAL_TOKEN_SPAN)
                .map(|index| format!("unrelated{index}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        assert!(!exact_product_identity_signal_is_present(
            &unrelated_multi_product_excerpt,
            "GIA 63W",
            "010-00386-00",
        ));
    }

    #[test]
    fn proposal_attestation_rejects_prefix_only_listing_evidence() {
        let mut context = context(vec![]);
        context.candidate.model = "GTX 345".to_string();
        context.listing_context = "Garmin GTX345R installed".to_string();
        let proposed = verified_identity();
        let response = json!({
            "proposal_decision": "confirmed_same_as_input",
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "proposal_manufacturer_identifier_scope": "exact_catalog_product",
            "proposal_confidence": "very_high",
            "input_evidence_text": "GTX345R",
            "proposal_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
            "proposal_source_title": "GTX 345R installation manual",
            "proposal_evidence": "The manufacturer manual identifies GTX 345R as part number 011-03520-00.",
            "proposal_reason": "The listing excerpt and official manual identify one exact unit.",
            "reviews": []
        });
        let error =
            proposal_attestation_with_direct_source_proofs(&context, &proposed, &response, &[])
                .unwrap_err();
        assert!(
            error.to_string().contains("alphanumeric boundaries"),
            "a longer listing model must not attest a truncated raw model: {error}"
        );
    }

    #[test]
    fn proposal_attestation_rejects_unmentioned_component_narrowing() {
        let mut context = context(vec![]);
        context.candidate.model = "G1000".to_string();
        context.candidate.avionics_types = vec!["Integrated Flight Deck".to_string()];
        context.listing_context = "Garmin G1000 integrated avionics installed".to_string();
        let proposed = VerifiedIdentity {
            canonical_manufacturer: "Garmin".to_string(),
            canonical_model: "GIA 63".to_string(),
            canonical_types: vec!["NAV".to_string(), "COM".to_string()],
            manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
            manufacturer_identifier: "GIA 63".to_string(),
            manufacturer_identifier_scope: "exact_catalog_product".to_string(),
            identity_source_url: "https://static.garmin.com/manuals/gia63.pdf".to_string(),
            identity_source_title: "GIA 63 manual".to_string(),
            identity_evidence: "Garmin identifies the GIA 63 model.".to_string(),
            reason: "The official manual identifies the exact LRU.".to_string(),
            grounded_claim_source_urls: vec![
                "https://static.garmin.com/manuals/gia63.pdf".to_string()
            ],
        };
        let response = json!({
            "proposal_decision": "confirmed_same_as_input",
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GIA 63",
            "canonical_types": ["NAV", "COM"],
            "manufacturer_identifier_kind": "manufacturer_model_number",
            "manufacturer_identifier": "GIA 63",
            "proposal_manufacturer_identifier_scope": "exact_catalog_product",
            "proposal_confidence": "very_high",
            "input_evidence_text": "G1000",
            "proposal_source_url": "https://static.garmin.com/manuals/gia63.pdf",
            "proposal_source_title": "GIA 63 manual",
            "proposal_evidence": "Garmin identifies the GIA 63 model.",
            "proposal_reason": "The listing names the containing suite, not the proposed LRU.",
            "reviews": []
        });

        let error =
            proposal_attestation_with_direct_source_proofs(&context, &proposed, &response, &[])
                .expect_err("a containing suite label cannot prove an unmentioned component");
        assert!(error
            .to_string()
            .contains("complete proposed canonical model"));
    }

    #[test]
    fn proposal_attestation_accepts_punctuation_only_model_variants() {
        let mut context = context(vec![]);
        context.listing_context = "Garmin GTX-345R installed".to_string();
        let proposed = verified_identity();
        let response = json!({
            "proposal_decision": "confirmed_same_as_input",
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "proposal_manufacturer_identifier_scope": "exact_catalog_product",
            "proposal_confidence": "very_high",
            "input_evidence_text": "GTX-345R",
            "proposal_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
            "proposal_source_title": "GTX 345R installation manual",
            "proposal_evidence": "The manufacturer manual identifies GTX 345R as part number 011-03520-00.",
            "proposal_reason": "The listing excerpt and official manual identify one exact unit.",
            "reviews": []
        });
        let proofs = [direct_source_proof(
            "https://static.garmin.com/manuals/gtx345r.pdf",
            "The manufacturer manual identifies GTX 345R as part number 011-03520-00.",
        )];
        let attestation =
            proposal_attestation_with_direct_source_proofs(&context, &proposed, &response, &proofs)
                .expect("punctuation-only listing typography should remain admissible");
        assert!(attestation.confirmed);
    }

    #[test]
    fn honest_negative_proposal_attestation_returns_unconfirmed_without_citations() {
        let context = context(vec![]);
        let proposed = verified_identity();
        let response = json!({
            "proposal_decision": "not_confirmed",
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "proposal_manufacturer_identifier_scope": "unknown",
            "proposal_confidence": "medium",
            "input_evidence_text": "",
            "proposal_source_url": "",
            "proposal_source_title": "",
            "proposal_evidence": "",
            "proposal_reason": "The stored listing excerpt is insufficient to prove the mapping.",
            "reviews": []
        });
        let attestation =
            proposal_attestation_with_direct_source_proofs(&context, &proposed, &response, &[])
                .expect("an honest negative should remain a normal unresolved outcome");
        assert!(!attestation.confirmed);
    }

    #[test]
    fn listing_pages_are_not_identity_evidence() {
        let context = context(vec![candidate(1, "GTX 345R", "approved")]);
        let mut response = json!({
            "status": "existing_match",
            "catalog_id": 1,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-TEST-1",
            "manufacturer_identifier_scope": "exact_catalog_product",
            "rejection_basis": "none",
            "confidence": "very_high",
            "identity_source_url": "https://broker.example/listings/1",
            "identity_source_title": "Aircraft for sale",
            "identity_evidence": "Seller says it is installed.",
            "reason": "Listing text."
        });
        assert!(resolution_issues(&context, &response, true, &[], &[])
            .iter()
            .any(|issue| issue.contains("sale listings")));
        response["identity_source_url"] = json!("https://broker.example/aircraft/1");
        assert!(resolution_issues(&context, &response, true, &[], &[])
            .iter()
            .any(|issue| issue.contains("cannot also be identity evidence")));
    }

    #[tokio::test]
    async fn verified_identity_persistence_creates_only_an_approved_stable_catalog_row() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let empty_fingerprint = catalog_fingerprint(&[]);
        let stored =
            persist_approved_identity(&db, None, &[], &verified_identity(), &empty_fingerprint)
                .await
                .expect("verified identity should persist");
        assert!(stored.id > 0);

        let catalog = load_catalog_candidates(&db)
            .await
            .expect("catalog should load");
        let row = catalog
            .iter()
            .find(|candidate| candidate.id == stored.id)
            .expect("stored identity should be in the catalog");
        assert_eq!(row.catalog_status, "approved");
        assert_eq!(row.manufacturer_identifier, "011-03520-00");

        let known = load_known_approved_candidates(&db)
            .await
            .expect("approved source records should load for cheap matching");
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].id, stored.id);
        assert_eq!(
            known[0].identity_source_url,
            "https://static.garmin.com/manuals/gtx345r.pdf"
        );
        assert_eq!(
            known[0].identity_evidence,
            "The manufacturer manual identifies the exact product and part number."
        );
        let local = resolve_verified_local_avionics_identity(
            &db,
            &local_request("Garmin GTX345R P/N 011-03520-00 installed"),
        )
        .await
        .expect("local lookup should not need an extractor")
        .expect("exact retained evidence should resolve the approved graph identity");
        assert_eq!(local.id, stored.id);
        assert_eq!(local.model, "GTX 345R");

        let mut mechanically_normalized_capability =
            local_request("Garmin GTX345R P/N 011-03520-00 installed");
        mechanically_normalized_capability.avionics_types = vec!["transponder".to_string()];
        assert!(
            resolve_verified_local_avionics_identity(&db, &mechanically_normalized_capability,)
                .await
                .expect("local lookup should remain available")
                .is_none(),
            "the local path requires exact server taxonomy values"
        );
    }

    #[tokio::test]
    async fn fresh_grounded_review_durably_attests_an_existing_historical_product() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let stored = persist_approved_identity(
            &db,
            None,
            &[],
            &verified_identity(),
            &catalog_fingerprint(&[]),
        )
        .await
        .expect("approved product should seed");
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(stored.id)
            .execute(pool)
            .await
            .expect("fixture should emulate a pre-policy historical approval");

        assert!(resolve_verified_local_avionics_identity(
            &db,
            &local_request("Garmin GTX345R P/N 011-03520-00 installed"),
        )
        .await
        .expect("local resolver should fail closed")
        .is_none());
        let mut freshly_grounded = stored.clone();
        freshly_grounded.evidence_url =
            "https://static.garmin.com/pumac/current-gtx-345r-manual.pdf".to_string();
        freshly_grounded.evidence_title = "Current Garmin GTX 345R manual".to_string();
        freshly_grounded.evidence =
            "Garmin GTX 345R manufacturer model number GTX 345R.".to_string();
        assert!(
            attest_grounded_existing_avionics_identity(&db, &freshly_grounded)
                .await
                .expect("fresh grounded conclusion should commit"),
            "the reviewed exact Garmin origin is eligible for attestation"
        );

        let refreshed_evidence: (String, String, String, String, String) = sqlx::query_as(
            r#"
            SELECT identity_source_url, identity_source_title,
                   identity_evidence_text, identity_evidence_kind,
                   identity_confidence
            FROM avionics_models
            WHERE id = ?
            "#,
        )
        .bind(stored.id)
        .fetch_one(pool)
        .await
        .expect("refreshed evidence should load");
        assert_eq!(
            refreshed_evidence,
            (
                freshly_grounded.evidence_url.clone(),
                freshly_grounded.evidence_title.clone(),
                freshly_grounded.evidence.clone(),
                "authoritative_reference".to_string(),
                "very_high".to_string(),
            )
        );

        let persisted: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?",
        )
        .bind(stored.id)
        .fetch_one(pool)
        .await
        .expect("attestation should be durable");
        assert_eq!(persisted, 1);
        assert!(
            resolve_verified_local_avionics_identity(
                &db,
                &local_request("Garmin GTX345R P/N 011-03520-00 installed"),
            )
            .await
            .expect("local resolver should load")
            .is_some(),
            "the committed current-policy attestation should enable reuse"
        );

        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(stored.id)
            .execute(pool)
            .await
            .expect("rollback fixture should clear the positive cache");
        let mut unauthorized_source = freshly_grounded.clone();
        unauthorized_source.evidence_url =
            "https://unrelated.example/current-gtx-345r-manual.pdf".to_string();
        unauthorized_source.evidence = "Unrelated source says GTX 345R.".to_string();
        assert!(
            !attest_grounded_existing_avionics_identity(&db, &unauthorized_source)
                .await
                .expect("an unauthorized source should fail closed"),
            "an uncurated origin cannot produce a positive attestation"
        );
        let rolled_back_evidence: (String, String, String) = sqlx::query_as(
            r#"
            SELECT identity_source_url, identity_source_title, identity_evidence_text
            FROM avionics_models WHERE id = ?
            "#,
        )
        .bind(stored.id)
        .fetch_one(pool)
        .await
        .expect("catalog evidence should still load");
        assert_eq!(
            rolled_back_evidence,
            (
                freshly_grounded.evidence_url.clone(),
                freshly_grounded.evidence_title.clone(),
                freshly_grounded.evidence.clone(),
            ),
            "failed attestation must roll back its tentative evidence refresh"
        );
        let mut stale_identity = freshly_grounded.clone();
        stale_identity.model = "GTX 345R stale rewrite".to_string();
        assert!(
            attest_grounded_existing_avionics_identity(&db, &stale_identity)
                .await
                .is_err(),
            "a catalog identity changed during grounding must fail its locked comparison"
        );
        let evidence_after_stale: String =
            sqlx::query_scalar("SELECT identity_evidence_text FROM avionics_models WHERE id = ?")
                .bind(stored.id)
                .fetch_one(pool)
                .await
                .expect("evidence should remain readable");
        assert_eq!(evidence_after_stale, freshly_grounded.evidence);
    }

    async fn pending_product_guard(
        db: &AppDb,
        product_id: i64,
    ) -> PendingProductAttestationCommitGuard {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let owner_user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email = ?")
            .bind(crate::db::DEVELOPER_EMAIL)
            .fetch_one(pool)
            .await
            .unwrap();
        let variant_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours, ingestion_state
            ) VALUES (?, ?, 'https://broker.example/guarded-product',
                      2020, 450000, 900, 'pending_review')
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .bind(owner_user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let payload = json!({
            "version": 1,
            "aspects": [{
                "id": "guarded-product",
                "reuse_attestation_target_id": product_id
            }]
        });
        let payload_json = serde_json::to_string(&payload).unwrap();
        let payload_sha256 = format!("{:x}", Sha256::digest(payload_json.as_bytes()));
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_pending_reviews (
              listing_id, extraction_sha256, catalog_revision_sha256,
              pending_aspect_count, review_payload_json, review_payload_sha256
            ) VALUES (?, ?, ?, 1, ?, ?)
            "#,
        )
        .bind(listing_id)
        .bind("a".repeat(64))
        .bind("b".repeat(64))
        .bind(&payload_json)
        .bind(&payload_sha256)
        .execute(pool)
        .await
        .unwrap();
        PendingProductAttestationCommitGuard {
            owner_user_id,
            listing_id,
            review_payload_sha256: payload_sha256,
            aspect_id: json!("guarded-product"),
        }
    }

    async fn guarded_source_verification(
        db: &AppDb,
        approved: ApprovedAvionicsIdentity,
    ) -> ApprovedProductSourceVerification {
        let catalog = load_review_catalog_candidates(db).await.unwrap();
        let manufacturer_identity_id = catalog
            .iter()
            .find(|candidate| candidate.candidate.id == approved.id)
            .and_then(|candidate| candidate.manufacturer_identity_id);
        ApprovedProductSourceVerification {
            manufacturer_collision_snapshot_sha256: manufacturer_collision_snapshot_sha256(
                &approved.manufacturer,
                manufacturer_identity_id,
                &catalog,
            ),
            approved,
        }
    }

    #[tokio::test]
    async fn pending_product_attestation_rejects_association_removed_after_source_fetch() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        seed_garmin_static_source_authority(&db).await;
        let approved = persist_approved_identity(
            &db,
            None,
            &[],
            &verified_identity(),
            &catalog_fingerprint(&[]),
        )
        .await
        .unwrap();
        let guard = pending_product_guard(&db, approved.id).await;
        let verification = guarded_source_verification(&db, approved.clone()).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        sqlx::query("DELETE FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?")
            .bind(guard.listing_id)
            .execute(pool)
            .await
            .unwrap();

        let error = attest_pending_review_product_identity(&db, &verification, &guard)
            .await
            .expect_err("removed pending ownership must invalidate the fetched dossier");
        assert!(error.to_string().contains("no longer owns"));
    }

    #[tokio::test]
    async fn pending_product_attestation_rejects_collision_inserted_after_source_fetch() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        seed_garmin_static_source_authority(&db).await;
        let approved = persist_approved_identity(
            &db,
            None,
            &[],
            &verified_identity(),
            &catalog_fingerprint(&[]),
        )
        .await
        .unwrap();
        let guard = pending_product_guard(&db, approved.id).await;
        let verification = guarded_source_verification(&db, approved.clone()).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let (manufacturer_id, capability_id): (i64, i64) = sqlx::query_as(
            r#"
            SELECT model.avionics_manufacturer_id, membership.avionics_type_id
            FROM avionics_models model
            JOIN avionics_model_types membership
              ON membership.avionics_model_id = model.id
            WHERE model.id = ?
            LIMIT 1
            "#,
        )
        .bind(approved.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let collision_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name, catalog_status,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier
            ) VALUES (?, 'GTX 345R Plus', 'gtx 345r plus', 'unreviewed',
                      'manufacturer_part_number', '011-NEW-COLLISION',
                      '011newcollision')
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(collision_id)
        .bind(capability_id)
        .execute(pool)
        .await
        .unwrap();

        let error = attest_pending_review_product_identity(&db, &verification, &guard)
            .await
            .expect_err("a changed manufacturer scope must invalidate the fetched dossier");
        assert!(error.to_string().contains("collision catalog changed"));
    }

    #[tokio::test]
    async fn automated_existing_match_refreshes_evidence_before_reuse_attestation() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let original = verified_identity();
        let stored =
            persist_approved_identity(&db, None, &[], &original, &catalog_fingerprint(&[]))
                .await
                .expect("approved product should seed");
        let reviewed_catalog = load_catalog_candidates(&db)
            .await
            .expect("approved catalog should load");
        let reviewed = reviewed_catalog
            .iter()
            .find(|candidate| candidate.id == stored.id)
            .expect("approved product should be reviewable")
            .clone();
        let reviewed_fingerprint = catalog_fingerprint(&reviewed_catalog);
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };

        let mut fresh = original.clone();
        fresh.identity_source_url =
            "https://static.garmin.com/pumac/current-gtx345r.pdf".to_string();
        fresh.identity_source_title = "Current Garmin GTX 345R manual".to_string();
        fresh.identity_evidence =
            "The current Garmin manual identifies the exact GTX 345R product.".to_string();
        fresh.grounded_claim_source_urls = vec![fresh.identity_source_url.clone()];
        persist_existing_reuse_attestation(&db, &reviewed, &fresh, &reviewed_fingerprint)
            .await
            .expect("fresh automated grounding should commit");

        let persisted: (String, String, String, String, String, String, String) = sqlx::query_as(
            r#"
                SELECT manufacturer.name, model.name,
                       model.manufacturer_identifier_kind,
                       model.manufacturer_identifier,
                       model.identity_source_url,
                       model.identity_source_title,
                       model.identity_evidence_text
                FROM avionics_models model
                JOIN avionics_manufacturers manufacturer
                  ON manufacturer.id = model.avionics_manufacturer_id
                WHERE model.id = ?
                "#,
        )
        .bind(stored.id)
        .fetch_one(pool)
        .await
        .expect("refreshed approved product should load");
        assert_eq!(
            persisted,
            (
                original.canonical_manufacturer.clone(),
                original.canonical_model.clone(),
                original.manufacturer_identifier_kind.clone(),
                original.manufacturer_identifier.clone(),
                fresh.identity_source_url.clone(),
                fresh.identity_source_title.clone(),
                fresh.identity_evidence.clone(),
            ),
            "automated reuse must update only grounded evidence, never canonical identity"
        );
        assert!(
            resolve_verified_local_avionics_identity(
                &db,
                &local_request("Garmin GTX345R P/N 011-03520-00 installed"),
            )
            .await
            .expect("the refreshed attestation should load")
            .is_some(),
            "the attestation fingerprint must be computed from refreshed evidence"
        );

        let mut unauthorized = fresh.clone();
        unauthorized.identity_source_url =
            "https://unrelated.example/current-gtx345r.pdf".to_string();
        unauthorized.identity_source_title = "Unrelated product page".to_string();
        unauthorized.identity_evidence = "An unrelated source mentions GTX 345R.".to_string();
        unauthorized.grounded_claim_source_urls = vec![unauthorized.identity_source_url.clone()];
        let error = persist_existing_reuse_attestation(
            &db,
            &reviewed,
            &unauthorized,
            &reviewed_fingerprint,
        )
        .await
        .expect_err("an unauthorized origin must fail closed");
        assert!(error
            .to_string()
            .contains("could not be bound to a current active exact manufacturer source origin"));
        let evidence_after_rollback: (String, String, String) = sqlx::query_as(
            r#"
            SELECT identity_source_url, identity_source_title, identity_evidence_text
            FROM avionics_models
            WHERE id = ?
            "#,
        )
        .bind(stored.id)
        .fetch_one(pool)
        .await
        .expect("approved product should survive rollback");
        assert_eq!(
            evidence_after_rollback,
            (
                fresh.identity_source_url,
                fresh.identity_source_title,
                fresh.identity_evidence,
            ),
            "failed automated attestation must roll back its evidence refresh"
        );
    }

    #[test]
    fn local_manufacturer_key_does_not_apply_aircraft_semantic_aliases() {
        assert_eq!(
            super::exact_manufacturer_name_key("Garmin, Inc."),
            super::exact_manufacturer_name_key("Garmin")
        );
        assert_ne!(
            super::exact_manufacturer_name_key("Textron Aviation"),
            super::exact_manufacturer_name_key("Cessna")
        );
    }

    #[tokio::test]
    async fn identifier_namespace_is_part_of_persisted_product_identity() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let mut part_number_product = verified_identity();
        part_number_product.canonical_model = "Part Number Product".to_string();
        part_number_product.manufacturer_identifier = "SHARED-100".to_string();
        let part_number = persist_approved_identity(
            &db,
            None,
            &[],
            &part_number_product,
            &catalog_fingerprint(&[]),
        )
        .await
        .expect("part-number identity should persist");

        let reviewed_catalog = load_catalog_candidates(&db)
            .await
            .expect("catalog should reload");
        let mut sku_product = part_number_product.clone();
        sku_product.canonical_model = "Distinct SKU Product".to_string();
        sku_product.manufacturer_identifier_kind = "sku".to_string();
        let sku = persist_approved_identity(
            &db,
            None,
            &[],
            &sku_product,
            &catalog_fingerprint(&reviewed_catalog),
        )
        .await
        .expect("the same text in a distinct identifier namespace should persist");

        assert_ne!(part_number.id, sku.id);
        assert_eq!(load_catalog_candidates(&db).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn multifunction_identity_persists_one_product_and_every_capability() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let mut identity = verified_identity();
        identity.canonical_model = "GNX 375".to_string();
        identity.canonical_types = vec!["GPS".to_string(), "Transponder".to_string()];
        identity.manufacturer_identifier = "GNX-375-TEST".to_string();
        let stored =
            persist_approved_identity(&db, None, &[], &identity, &catalog_fingerprint(&[]))
                .await
                .expect("multifunction identity should persist");

        let catalog = load_catalog_candidates(&db)
            .await
            .expect("catalog should load");
        let rows = catalog
            .iter()
            .filter(|candidate| candidate.id == stored.id)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "capabilities must not duplicate products");
        assert_eq!(
            rows[0].avionics_types,
            vec!["GPS".to_string(), "Transponder".to_string()]
        );
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let membership_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_model_types WHERE avionics_model_id = ?",
        )
        .bind(stored.id)
        .fetch_one(pool)
        .await
        .expect("capability memberships should load");
        assert_eq!(membership_count, 2);
    }

    #[tokio::test]
    async fn approved_product_enrichment_adds_capability_without_replacing_identity_or_memberships()
    {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let mut original = verified_identity();
        original.canonical_model = "GNX 375".to_string();
        original.manufacturer_identifier_kind = "manufacturer_model_number".to_string();
        original.manufacturer_identifier = "GNX 375".to_string();
        let stored =
            persist_approved_identity(&db, None, &[], &original, &catalog_fingerprint(&[]))
                .await
                .expect("initial approved product should persist");
        let reviewed_catalog = load_catalog_candidates(&db)
            .await
            .expect("approved product should load");
        let reviewed = reviewed_catalog
            .iter()
            .find(|candidate| candidate.id == stored.id)
            .expect("approved product should be reviewed")
            .clone();

        let mut enriched = original.clone();
        enriched.canonical_types = vec!["GPS".to_string(), "Transponder".to_string()];
        enriched.identity_source_url = "https://static.garmin.com/manuals/gnx375.pdf".to_string();
        enriched.identity_source_title = "Garmin GNX 375 pilot guide".to_string();
        enriched.identity_evidence =
            "Garmin documents the GNX 375 as a GPS navigator with an integrated transponder."
                .to_string();
        let result = persist_approved_capability_enrichment(
            &db,
            &reviewed,
            &enriched,
            &catalog_fingerprint(&reviewed_catalog),
        )
        .await
        .expect("independently verified capability should enrich the product");
        assert_eq!(result.id, stored.id);

        let catalog = load_catalog_candidates(&db)
            .await
            .expect("enriched catalog should load");
        assert_eq!(catalog.len(), 1, "enrichment must not create a product");
        assert_eq!(
            catalog[0].avionics_types,
            vec!["GPS".to_string(), "Transponder".to_string()]
        );
        assert_eq!(catalog[0].manufacturer_identifier, "GNX 375");
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let persisted: (String, String, String, String, String, String) = sqlx::query_as(
            r#"
            SELECT model.name, model.manufacturer_identifier_kind,
                   model.manufacturer_identifier, model.identity_source_url,
                   model.identity_source_title, model.identity_evidence_text
            FROM avionics_models model
            WHERE model.id = ?
            "#,
        )
        .bind(stored.id)
        .fetch_one(pool)
        .await
        .expect("enriched approved product should load");
        assert_eq!(
            persisted,
            (
                original.canonical_model,
                original.manufacturer_identifier_kind,
                original.manufacturer_identifier,
                enriched.identity_source_url.clone(),
                enriched.identity_source_title.clone(),
                enriched.identity_evidence.clone(),
            ),
            "capability enrichment must preserve identity while replacing grounded evidence"
        );
        let mut local_request = local_request("Garmin GNX 375 installed");
        local_request.model = "GNX 375".to_string();
        local_request.avionics_types = vec!["GPS".to_string(), "Transponder".to_string()];
        assert_eq!(
            resolve_verified_local_avionics_identity(&db, &local_request)
                .await
                .expect("enriched local identity should load")
                .expect("the exact manufacturer model number should resolve locally")
                .id,
            stored.id,
            "an attested manufacturer model number is sufficient for exact listing-label reuse"
        );
    }

    #[tokio::test]
    async fn capability_enrichment_rolls_back_membership_and_evidence_when_attestation_fails() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let original = verified_identity();
        let stored =
            persist_approved_identity(&db, None, &[], &original, &catalog_fingerprint(&[]))
                .await
                .expect("approved product should seed");
        let reviewed_catalog = load_catalog_candidates(&db)
            .await
            .expect("approved catalog should load");
        let reviewed = reviewed_catalog[0].clone();
        let mut invalid_source = original.clone();
        invalid_source.canonical_types = vec!["GPS".to_string(), "Transponder".to_string()];
        invalid_source.identity_source_url =
            "https://unrelated.example/gtx345r-manual.pdf".to_string();
        invalid_source.identity_source_title = "Unrelated GTX 345R page".to_string();
        invalid_source.identity_evidence =
            "An unrelated source claims the product adds GPS.".to_string();
        invalid_source.grounded_claim_source_urls =
            vec![invalid_source.identity_source_url.clone()];

        let error = persist_approved_capability_enrichment(
            &db,
            &reviewed,
            &invalid_source,
            &catalog_fingerprint(&reviewed_catalog),
        )
        .await
        .expect_err("an unauthorized evidence origin must not enrich an approved product");
        assert!(error
            .to_string()
            .contains("could not be bound to a current active exact manufacturer source origin"));
        let unchanged = load_catalog_candidates(&db)
            .await
            .expect("catalog should survive the rolled-back enrichment");
        assert_eq!(unchanged[0].avionics_types, original.canonical_types);
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let evidence_after_rollback: (String, String, String) = sqlx::query_as(
            r#"
            SELECT identity_source_url, identity_source_title, identity_evidence_text
            FROM avionics_models
            WHERE id = ?
            "#,
        )
        .bind(stored.id)
        .fetch_one(pool)
        .await
        .expect("approved evidence should survive rollback");
        assert_eq!(
            evidence_after_rollback,
            (
                original.identity_source_url,
                original.identity_source_title,
                original.identity_evidence,
            )
        );
    }

    #[tokio::test]
    async fn rejected_non_monotonic_enrichment_leaves_approved_capabilities_unchanged() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let stored = persist_approved_identity(
            &db,
            None,
            &[],
            &verified_identity(),
            &catalog_fingerprint(&[]),
        )
        .await
        .expect("initial approved product should persist");
        let reviewed_catalog = load_catalog_candidates(&db)
            .await
            .expect("approved product should load");
        let reviewed = reviewed_catalog[0].clone();
        let mut invalid = verified_identity();
        invalid.canonical_types = vec!["GPS".to_string()];
        let error = persist_approved_capability_enrichment(
            &db,
            &reviewed,
            &invalid,
            &catalog_fingerprint(&reviewed_catalog),
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot remove stored capability"));

        let unchanged = load_catalog_candidates(&db)
            .await
            .expect("unchanged product should load");
        assert_eq!(unchanged.len(), 1);
        assert_eq!(unchanged[0].id, stored.id);
        assert_eq!(unchanged[0].avionics_types, vec!["Transponder".to_string()]);
    }

    #[tokio::test]
    async fn capability_enrichment_rejects_a_stale_catalog_fingerprint() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let stored = persist_approved_identity(
            &db,
            None,
            &[],
            &verified_identity(),
            &catalog_fingerprint(&[]),
        )
        .await
        .expect("initial approved product should persist");
        let reviewed_catalog = load_catalog_candidates(&db)
            .await
            .expect("approved product should load");
        let reviewed = reviewed_catalog[0].clone();
        let stale_fingerprint = catalog_fingerprint(&reviewed_catalog);

        let mut second = verified_identity();
        second.canonical_model = "GMA 350c".to_string();
        second.canonical_types = vec!["Audio Panel".to_string()];
        second.manufacturer_identifier = "011-02385-20".to_string();
        persist_approved_identity(
            &db,
            None,
            &[],
            &second,
            &catalog_fingerprint(&reviewed_catalog),
        )
        .await
        .expect("concurrent catalog addition should persist");

        let mut enriched = verified_identity();
        enriched.canonical_types = vec!["GPS".to_string(), "Transponder".to_string()];
        let error =
            persist_approved_capability_enrichment(&db, &reviewed, &enriched, &stale_fingerprint)
                .await
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("changed during Gemini capability review"));
        let unchanged = load_catalog_candidates(&db)
            .await
            .expect("catalog should remain readable");
        let original = unchanged
            .iter()
            .find(|candidate| candidate.id == stored.id)
            .expect("original product should remain");
        assert_eq!(original.avionics_types, vec!["Transponder".to_string()]);
    }

    #[tokio::test]
    async fn persistence_refuses_to_approve_a_product_without_capabilities() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let mut identity = verified_identity();
        identity.canonical_types.clear();
        let error = persist_approved_identity(&db, None, &[], &identity, &catalog_fingerprint(&[]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("without a canonical capability"));
    }

    #[tokio::test]
    async fn persistence_requires_explicit_consolidation_for_multiple_legacy_matches() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let error = persist_approved_identity(
            &db,
            Some(1),
            &[2, 1, 2],
            &verified_identity(),
            &catalog_fingerprint(&[]),
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("explicit avionics consolidation"));
    }

    #[tokio::test]
    async fn persistence_rejects_a_catalog_snapshot_that_changed_during_review() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_garmin_static_source_authority(&db).await;
        let stale_empty_fingerprint = catalog_fingerprint(&[]);
        persist_approved_identity(
            &db,
            None,
            &[],
            &verified_identity(),
            &stale_empty_fingerprint,
        )
        .await
        .expect("first identity should persist");

        let mut second = verified_identity();
        second.canonical_model = "GMA 350c".to_string();
        second.canonical_types = vec!["Audio Panel".to_string()];
        second.manufacturer_identifier = "011-02385-20".to_string();
        let error = persist_approved_identity(&db, None, &[], &second, &stale_empty_fingerprint)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("changed during Gemini review"));
    }

    #[tokio::test]
    async fn legacy_promotion_invalidates_unreviewed_value_metadata() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let identity = verified_identity();
        let stored_id = seed_unreviewed_legacy_identity(
            &db,
            &identity,
            &identity.manufacturer_identifier,
            true,
        )
        .await;
        seed_garmin_static_source_authority(&db).await;

        let legacy_catalog = load_catalog_candidates(&db)
            .await
            .expect("legacy catalog should load");
        let legacy_fingerprint = catalog_fingerprint(&legacy_catalog);
        persist_approved_identity(
            &db,
            Some(stored_id),
            &[stored_id],
            &identity,
            &legacy_fingerprint,
        )
        .await
        .expect("legacy identity should promote");
        let sql = db.sql(
            "SELECT COUNT(*) FROM avionics_models WHERE id = ? AND catalog_status = 'approved' AND introduced_year IS NULL AND estimated_unit_value_usd IS NULL AND replacement_cost_usd IS NULL AND value_reference_year IS NULL AND value_source IS NULL AND value_basis = 'unreviewed' AND valuation_scope = 'unit'",
        );
        let clean_count: i64 = match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query_scalar(&sql)
                .bind(stored_id)
                .fetch_one(pool)
                .await
                .expect("promoted row should load"),
            DatabaseBackend::Postgres(_) => unreachable!("test uses SQLite"),
        };
        assert_eq!(clean_count, 1);
    }

    #[tokio::test]
    async fn legacy_promotion_refuses_to_overwrite_a_conflicting_identifier() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let identity = verified_identity();
        let stored_id =
            seed_unreviewed_legacy_identity(&db, &identity, "LEGACY-CONFLICT", false).await;
        let legacy_catalog = load_catalog_candidates(&db)
            .await
            .expect("legacy catalog should load");
        let legacy_fingerprint = catalog_fingerprint(&legacy_catalog);
        let error = persist_approved_identity(
            &db,
            Some(stored_id),
            &[stored_id],
            &identity,
            &legacy_fingerprint,
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("conflicting legacy manufacturer identifier"));
    }
}
