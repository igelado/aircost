//! Explicit avionics-catalog duplicate auditing and consolidation.
//!
//! Similarity and mechanically normalized product names are intentionally
//! absent from the destructive mutation path. A caller names one survivor and
//! every duplicate, and every pair must share an exact stable manufacturer
//! identifier kind/value within the same evidence-backed manufacturer scope.
//! Name-only groups remain audit/review candidates. The dry-run and apply paths
//! use the same planner; apply repeats the plan while holding all affected
//! tables in one transaction.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::avionics::model::{avionics_model_identity_relation, AvionicsModelIdentityRelation};
use crate::db::{AppDb, DatabaseBackend};
use crate::listing::review::{
    fingerprint_approved_catalog_rows, serialize_review_payload, CatalogFingerprintRow,
    PendingReviewAspect, ReviewProduct,
};
use crate::normalize::{
    normalize_avionics_identifier, normalize_avionics_manufacturer_name,
    normalize_avionics_model_name,
};

#[derive(Debug)]
pub enum ConsolidationError {
    Validation(String),
    Conflict(String),
    Database(String),
}

impl fmt::Display for ConsolidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Conflict(message) | Self::Database(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for ConsolidationError {}

impl From<sqlx::Error> for ConsolidationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

pub type ConsolidationResult<T> = Result<T, ConsolidationError>;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AvionicsConsolidationRequest {
    pub survivor_id: i64,
    pub duplicate_ids: Vec<i64>,
}

/// Optional listing-review provenance for a human duplicate adjudication.
///
/// The persisted authorization snapshots the current pending-review row and
/// payload digest. It intentionally does not retain a Gemini prompt, response,
/// or URL-context dossier.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct HumanReviewedConsolidationProvenance {
    pub listing_id: i64,
    pub review_aspect_id: String,
    pub expected_review_payload_sha256: String,
}

/// An explicit, evidence-backed authorization to consolidate catalog IDs that
/// represent one model-equivalent product but lack a common stable
/// manufacturer identifier.
///
/// This is deliberately separate from [`AvionicsConsolidationRequest`].
/// Automatic consolidation remains identifier-only.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct HumanReviewedAvionicsConsolidationRequest {
    pub survivor_id: i64,
    pub duplicate_ids: Vec<i64>,
    pub reviewer_user_id: i64,
    pub authoritative_source_url: String,
    pub authoritative_source_title: String,
    pub exact_evidence_text: String,
    pub provenance: Option<HumanReviewedConsolidationProvenance>,
    /// Apply-only compare-and-swap value returned by the immediately preceding
    /// preview. A changed row, effective maker identity, or review provenance
    /// produces a different authorization digest and aborts under lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_authorization_sha256: Option<String>,
    /// Approved-catalog revision accepted by the reviewer. Apply recomputes it
    /// from the locked catalog state before installing any guard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_catalog_revision_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanReviewedConsolidationMemberRole {
    Survivor,
    Duplicate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HumanReviewedConsolidationMemberSnapshot {
    pub avionics_model_id: i64,
    pub role: HumanReviewedConsolidationMemberRole,
    pub row_identity_sha256: String,
    pub avionics_manufacturer_id: i64,
    pub avionics_manufacturer_identity_id: i64,
    pub manufacturer: String,
    pub model: String,
    pub stored_manufacturer_key: String,
    pub stored_model_key: String,
    pub canonical_model_key: String,
    pub catalog_status: String,
    pub manufacturer_identifier_kind: Option<String>,
    pub manufacturer_identifier: Option<String>,
    pub normalized_manufacturer_identifier: Option<String>,
    pub identity_source_url: Option<String>,
    pub identity_source_title: Option<String>,
    pub identity_evidence_text: Option<String>,
    pub identity_evidence_kind: String,
    pub identity_confidence: Option<String>,
    pub catalog_reviewed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HumanReviewedConsolidationAuthorization {
    pub authorization_sha256: String,
    pub persisted: bool,
    pub reviewer_user_id: i64,
    pub survivor_model_id: i64,
    pub effective_manufacturer_identity_id: i64,
    pub canonical_model_key: String,
    pub authoritative_source_url: String,
    pub authoritative_source_title: String,
    pub exact_evidence_text: String,
    pub provenance_listing_id: Option<i64>,
    pub provenance_pending_review_id: Option<i64>,
    pub provenance_review_payload_sha256: Option<String>,
    pub provenance_review_aspect_id: Option<String>,
    pub members: Vec<HumanReviewedConsolidationMemberSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HumanReviewedAvionicsConsolidationReport {
    pub consolidation: AvionicsConsolidationReport,
    pub authorization: HumanReviewedConsolidationAuthorization,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityMatchBasis {
    CanonicalManufacturerAndModel,
    StableManufacturerIdentifier,
    HumanReviewedModelEquivalence,
    Both,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogIdentitySummary {
    pub id: i64,
    pub manufacturer: String,
    pub model: String,
    pub catalog_status: String,
    pub manufacturer_identifier_kind: Option<String>,
    pub manufacturer_identifier: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConsolidationIdentityMatch {
    pub duplicate: CatalogIdentitySummary,
    pub basis: IdentityMatchBasis,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ConsolidationChangeCounts {
    pub model_rows_deleted: usize,
    pub type_memberships_added: usize,
    pub listing_links_remapped: usize,
    pub listing_link_conflicts_coalesced: usize,
    pub default_links_remapped: usize,
    pub default_link_conflicts_coalesced: usize,
    pub default_candidate_links_remapped: usize,
    pub default_candidate_link_conflicts_coalesced: usize,
    pub reference_links_remapped: usize,
    pub reference_link_conflicts_coalesced: usize,
    pub suite_links_remapped: usize,
    pub suite_link_conflicts_coalesced: usize,
    pub suite_self_links_removed: usize,
    pub pending_reviews_rewritten: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AvionicsConsolidationReport {
    pub dry_run: bool,
    pub applied: bool,
    pub can_apply: bool,
    pub survivor: CatalogIdentitySummary,
    pub matches: Vec<ConsolidationIdentityMatch>,
    pub changes: ConsolidationChangeCounts,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DuplicateAuditModel {
    pub id: i64,
    pub manufacturer: String,
    pub model: String,
    pub catalog_status: String,
    pub manufacturer_identifier_kind: Option<String>,
    pub manufacturer_identifier: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DuplicateAuditGroup {
    pub manufacturer_key: String,
    pub model_key: String,
    pub models: Vec<DuplicateAuditModel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DuplicateIdentifierAuditGroup {
    /// Stable identifiers are namespaced by their evidence-backed maker.
    pub manufacturer_key: String,
    pub identifier_kind: String,
    pub identifier_key: String,
    pub models: Vec<DuplicateAuditModel>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct StableIdentifierKey {
    pub kind: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AvionicsDuplicateAudit {
    pub model_count: usize,
    /// Groups using the normalized keys physically stored in the database.
    pub exact_groups: Vec<DuplicateAuditGroup>,
    /// Groups after re-running the current avionics canonicalizers over the
    /// display names. This catches legacy typography-only manufacturer rows.
    pub canonical_groups: Vec<DuplicateAuditGroup>,
    /// Groups sharing an exact, nonblank manufacturer-identifier kind and
    /// normalized value within the same evidence-backed maker scope.
    pub identifier_groups: Vec<DuplicateIdentifierAuditGroup>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CanonicalLegacyReferenceStrength {
    pub global_reference_count: usize,
    pub reviewer_confirmed_listing_reference_count: usize,
    pub high_confidence_listing_reference_count: usize,
    pub total_reference_count: usize,
    pub capability_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CanonicalLegacyCandidate {
    pub model_id: i64,
    pub strength: CanonicalLegacyReferenceStrength,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CanonicalLegacyConsolidationPlan {
    pub manufacturer_key: String,
    /// Every exact canonical model-name key represented by this connected
    /// identity component.
    pub model_keys: Vec<String>,
    /// Every nonblank exact stable-identifier key represented by the same
    /// manufacturer-scoped component.
    pub identifier_keys: Vec<StableIdentifierKey>,
    pub request: AvionicsConsolidationRequest,
    /// Candidates in survivor preference order. Selection is lexicographic by
    /// reference strength (descending), then catalog ID (ascending).
    pub candidates: Vec<CanonicalLegacyCandidate>,
    /// A connected component is automatically applicable only when every pair
    /// has a direct exact identity edge. Transitive-but-not-direct graphs stay
    /// visible but are blocked for explicit evidence review.
    pub blockers: Vec<String>,
}

#[derive(Clone, Debug, FromRow)]
struct ModelRow {
    id: i64,
    avionics_manufacturer_id: i64,
    avionics_manufacturer_identity_id: Option<i64>,
    manufacturer: String,
    stored_manufacturer_key: String,
    model: String,
    stored_model_key: String,
    catalog_status: String,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    stored_manufacturer_identifier_key: Option<String>,
    identity_source_url: Option<String>,
    identity_source_title: Option<String>,
    identity_evidence_text: Option<String>,
    identity_evidence_kind: String,
    identity_confidence: Option<String>,
    catalog_reviewed_at: Option<String>,
}

impl ModelRow {
    fn summary(&self) -> CatalogIdentitySummary {
        CatalogIdentitySummary {
            id: self.id,
            manufacturer: self.manufacturer.clone(),
            model: self.model.clone(),
            catalog_status: self.catalog_status.clone(),
            manufacturer_identifier_kind: self.manufacturer_identifier_kind.clone(),
            manufacturer_identifier: self.manufacturer_identifier.clone(),
        }
    }

    fn audit_model(&self) -> DuplicateAuditModel {
        DuplicateAuditModel {
            id: self.id,
            manufacturer: self.manufacturer.clone(),
            model: self.model.clone(),
            catalog_status: self.catalog_status.clone(),
            manufacturer_identifier_kind: self.manufacturer_identifier_kind.clone(),
            manufacturer_identifier: self.manufacturer_identifier.clone(),
        }
    }
}

#[derive(Clone, Debug, FromRow)]
struct ModelTypeRow {
    avionics_model_id: i64,
    avionics_type_id: i64,
    capability: String,
}

#[derive(Clone, Debug, FromRow)]
struct ListingLinkRow {
    id: i64,
    aircraft_sale_listing_id: i64,
    avionics_model_id: i64,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    source_confidence: Option<String>,
    listing_ingestion_state: String,
    listing_is_verified: bool,
}

#[derive(Clone, Debug, FromRow)]
struct DefaultLinkRow {
    id: i64,
    aircraft_model_variant_id: i64,
    model_year: i64,
    avionics_model_id: i64,
    quantity: i64,
    source_url: String,
    source_title: String,
    source_notes: String,
    source_confidence: String,
}

#[derive(Clone, Debug, FromRow)]
struct DefaultCandidateLinkRow {
    id: i64,
    quarantined_default_avionics_id: Option<i64>,
    aircraft_model_variant_id: i64,
    model_year: i64,
    avionics_model_id: i64,
    quantity: i64,
    source_url: String,
    source_title: String,
    source_notes: String,
    source_confidence: String,
    pending_reason: String,
    quarantined_created_at: Option<String>,
    quarantined_updated_at: Option<String>,
    created_at: String,
}

#[derive(Clone, Debug, FromRow)]
struct ReferenceLinkRow {
    id: i64,
    aircraft_reference_configuration_version_id: i64,
    avionics_model_id: i64,
    quantity: i64,
    equipment_role: String,
    evidence_claim_id: i64,
    publication_state: String,
}

#[derive(Clone, Debug, FromRow)]
struct SuiteLinkRow {
    suite_model_id: i64,
    component_model_id: i64,
    quantity: i64,
}

#[derive(Clone, Debug, FromRow)]
struct PendingReviewRow {
    id: i64,
    listing_id: i64,
    review_payload_json: String,
    review_payload_sha256: String,
}

#[derive(Clone, Debug, FromRow)]
struct ApprovedManufacturerAliasScope {
    source_manufacturer_identity_id: i64,
    target_manufacturer_identity_id: i64,
}

#[derive(Clone, Debug, FromRow)]
struct UserIdRow {
    id: i64,
}

#[derive(Clone, Debug, Deserialize)]
struct StoredReviewPayload {
    version: u32,
    aspects: Vec<PendingReviewAspect>,
}

#[derive(Clone, Debug)]
struct CatalogState {
    models: Vec<ModelRow>,
    model_types: Vec<ModelTypeRow>,
    listing_links: Vec<ListingLinkRow>,
    default_links: Vec<DefaultLinkRow>,
    default_candidate_links: Vec<DefaultCandidateLinkRow>,
    reference_links: Vec<ReferenceLinkRow>,
    suite_links: Vec<SuiteLinkRow>,
    pending_reviews: Vec<PendingReviewRow>,
    approved_manufacturer_alias_scopes: Vec<ApprovedManufacturerAliasScope>,
    users: Vec<UserIdRow>,
}

#[derive(Clone, Debug)]
struct ListingLinkPlan {
    keeper: ListingLinkRow,
    deleted_ids: Vec<i64>,
}

#[derive(Clone, Debug)]
struct DefaultLinkPlan {
    keeper: DefaultLinkRow,
    deleted_ids: Vec<i64>,
}

#[derive(Clone, Debug)]
struct DefaultCandidateLinkPlan {
    keeper: DefaultCandidateLinkRow,
    deleted_ids: Vec<i64>,
}

#[derive(Clone, Debug)]
struct ReferenceLinkPlan {
    keeper: ReferenceLinkRow,
    remap_keeper: bool,
    deleted_ids: Vec<i64>,
}

#[derive(Clone, Debug)]
struct SuiteLinkPlan {
    original_suite_model_id: i64,
    original_component_model_id: i64,
    suite_model_id: i64,
    component_model_id: i64,
    quantity: i64,
    deleted_keys: Vec<(i64, i64)>,
}

#[derive(Clone, Debug)]
struct PendingReviewPlan {
    id: i64,
    extraction_sha256: String,
    pending_aspect_count: i64,
    review_payload_json: String,
    review_payload_sha256: String,
}

#[derive(Clone, Debug)]
struct ConsolidationPlan {
    report: AvionicsConsolidationReport,
    duplicate_ids: BTreeSet<i64>,
    type_ids_to_add: Vec<i64>,
    listing_links: Vec<ListingLinkPlan>,
    default_links: Vec<DefaultLinkPlan>,
    default_candidate_links: Vec<DefaultCandidateLinkPlan>,
    reference_links: Vec<ReferenceLinkPlan>,
    suite_original_rows_to_delete: Vec<(i64, i64)>,
    suite_links_to_upsert: Vec<SuiteLinkPlan>,
    pending_reviews: Vec<PendingReviewPlan>,
}

const MODEL_SQL: &str = r#"
    SELECT
      model.id,
      model.avionics_manufacturer_id,
      manufacturer_identity.avionics_manufacturer_identity_id,
      manufacturer.name AS manufacturer,
      manufacturer.normalized_name AS stored_manufacturer_key,
      model.name AS model,
      model.normalized_name AS stored_model_key,
      model.catalog_status,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier,
      model.normalized_manufacturer_identifier AS stored_manufacturer_identifier_key,
      model.identity_source_url,
      model.identity_source_title,
      model.identity_evidence_text,
      model.identity_evidence_kind,
      model.identity_confidence,
      model.catalog_reviewed_at
    FROM avionics_models model
    JOIN avionics_manufacturers manufacturer
      ON manufacturer.id = model.avionics_manufacturer_id
    LEFT JOIN avionics_manufacturer_effective_memberships manufacturer_identity
      ON manufacturer_identity.avionics_manufacturer_id
        = model.avionics_manufacturer_id
    ORDER BY model.id
"#;

macro_rules! load_catalog_state {
    ($db:expr, $executor:expr) => {{
        let models_sql = $db.sql(MODEL_SQL);
        let types_sql = $db.sql(
            r#"SELECT membership.avionics_model_id,
                      membership.avionics_type_id,
                      capability.name AS capability
               FROM avionics_model_types membership
               JOIN avionics_types capability
                 ON capability.id = membership.avionics_type_id
               ORDER BY membership.avionics_model_id,
                        capability.normalized_name,
                        capability.id"#,
        );
        let listing_sql = $db.sql(
            r#"SELECT link.id, link.aircraft_sale_listing_id,
                      link.avionics_model_id, link.quantity,
                      link.source, link.source_notes, link.configuration_action,
                      link.replaces_avionics_model_id, link.source_confidence,
                      listing.ingestion_state AS listing_ingestion_state,
                      listing.is_verified AS listing_is_verified
               FROM aircraft_sale_listing_avionics link
               JOIN aircraft_sale_listings listing
                 ON listing.id = link.aircraft_sale_listing_id
               ORDER BY link.id"#,
        );
        let defaults_sql = $db.sql(
            r#"SELECT id, aircraft_model_variant_id, model_year, avionics_model_id,
                      quantity, source_url, source_title, source_notes, source_confidence
               FROM aircraft_model_variant_default_avionics ORDER BY id"#,
        );
        let default_candidates_sql = $db.sql(
            r#"SELECT id, quarantined_default_avionics_id,
                      aircraft_model_variant_id, model_year, avionics_model_id,
                      quantity, source_url, source_title, source_notes,
                      source_confidence, pending_reason,
                      quarantined_created_at, quarantined_updated_at, created_at
               FROM aircraft_model_variant_default_avionics_candidates
               ORDER BY id"#,
        );
        let references_sql = $db.sql(
            r#"SELECT link.id, link.aircraft_reference_configuration_version_id,
                      link.avionics_model_id, link.quantity, link.equipment_role,
                      link.evidence_claim_id,
                      version.publication_state
               FROM aircraft_reference_avionics link
               JOIN aircraft_reference_configuration_versions version
                 ON version.id = link.aircraft_reference_configuration_version_id
               ORDER BY link.id"#,
        );
        let suites_sql = $db.sql(
            "SELECT suite_model_id, component_model_id, quantity FROM avionics_suite_components ORDER BY suite_model_id, component_model_id",
        );
        let reviews_sql = $db.sql(
            "SELECT id, listing_id, review_payload_json, review_payload_sha256 FROM aircraft_sale_listing_pending_reviews ORDER BY id",
        );
        let approved_alias_scopes_sql = $db.sql(
            r#"SELECT source_identity.avionics_manufacturer_identity_id
                        AS source_manufacturer_identity_id,
                      target_identity.avionics_manufacturer_identity_id
                        AS target_manufacturer_identity_id
               FROM avionics_manufacturer_alias_candidates candidate
               JOIN avionics_manufacturer_effective_memberships source_identity
                 ON source_identity.avionics_manufacturer_id
                   = candidate.avionics_manufacturer_id
               JOIN avionics_manufacturer_effective_identities target_identity
                 ON target_identity.identity_id
                   = candidate.candidate_manufacturer_identity_id
               WHERE candidate.review_status = 'approved'
                 AND candidate.decision_evidence_source_url IS NOT NULL
                 AND length(trim(candidate.decision_evidence_source_url)) > 0
                 AND candidate.decision_evidence_source_title IS NOT NULL
                 AND length(trim(candidate.decision_evidence_source_title)) > 0
                 AND candidate.decision_evidence_text IS NOT NULL
                 AND length(trim(candidate.decision_evidence_text)) > 0
                 AND candidate.reviewed_by_user_id IS NOT NULL
                 AND candidate.reviewed_at IS NOT NULL
               ORDER BY source_identity.avionics_manufacturer_identity_id,
                        target_identity.avionics_manufacturer_identity_id"#,
        );
        let users_sql = $db.sql("SELECT id FROM users ORDER BY id");
        CatalogState {
            models: sqlx::query_as::<_, ModelRow>(&models_sql)
                .fetch_all($executor)
                .await?,
            model_types: sqlx::query_as::<_, ModelTypeRow>(&types_sql)
                .fetch_all($executor)
                .await?,
            listing_links: sqlx::query_as::<_, ListingLinkRow>(&listing_sql)
                .fetch_all($executor)
                .await?,
            default_links: sqlx::query_as::<_, DefaultLinkRow>(&defaults_sql)
                .fetch_all($executor)
                .await?,
            default_candidate_links:
                sqlx::query_as::<_, DefaultCandidateLinkRow>(&default_candidates_sql)
                    .fetch_all($executor)
                    .await?,
            reference_links: sqlx::query_as::<_, ReferenceLinkRow>(&references_sql)
                .fetch_all($executor)
                .await?,
            suite_links: sqlx::query_as::<_, SuiteLinkRow>(&suites_sql)
                .fetch_all($executor)
                .await?,
            pending_reviews: sqlx::query_as::<_, PendingReviewRow>(&reviews_sql)
                .fetch_all($executor)
                .await?,
            approved_manufacturer_alias_scopes:
                sqlx::query_as::<_, ApprovedManufacturerAliasScope>(&approved_alias_scopes_sql)
                    .fetch_all($executor)
                    .await?,
            users: sqlx::query_as::<_, UserIdRow>(&users_sql)
                .fetch_all($executor)
                .await?,
        }
    }};
}

async fn load_state(db: &AppDb) -> ConsolidationResult<CatalogState> {
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(load_catalog_state!(db, pool)),
        DatabaseBackend::Postgres(pool) => Ok(load_catalog_state!(db, pool)),
    }
}

fn approved_catalog_revision_from_state(state: &CatalogState) -> String {
    let mut rows = Vec::new();
    for model in state
        .models
        .iter()
        .filter(|model| model.catalog_status == "approved")
    {
        for membership in state
            .model_types
            .iter()
            .filter(|membership| membership.avionics_model_id == model.id)
        {
            rows.push(CatalogFingerprintRow {
                id: model.id,
                manufacturer: model.manufacturer.clone(),
                model: model.model.clone(),
                capability: membership.capability.clone(),
                manufacturer_identifier_kind: model.manufacturer_identifier_kind.clone(),
                manufacturer_identifier: model.manufacturer_identifier.clone(),
            });
        }
    }
    fingerprint_approved_catalog_rows(rows)
}

/// Exhaustively group the current catalog using exact stored keys and the
/// current canonical manufacturer/model functions. This is deliberately not a
/// fuzzy duplicate detector. Canonical-name groups are review candidates, not
/// sufficient proof for destructive consolidation.
pub async fn audit_avionics_catalog_duplicates(
    db: &AppDb,
) -> ConsolidationResult<AvionicsDuplicateAudit> {
    let state = load_state(db).await?;
    Ok(audit_from_models(&state.models))
}

fn audit_from_models(models: &[ModelRow]) -> AvionicsDuplicateAudit {
    fn groups(
        keyed: impl IntoIterator<Item = ((String, String), DuplicateAuditModel)>,
    ) -> Vec<DuplicateAuditGroup> {
        let mut grouped = BTreeMap::<(String, String), Vec<DuplicateAuditModel>>::new();
        for (key, model) in keyed {
            grouped.entry(key).or_default().push(model);
        }
        grouped
            .into_iter()
            .filter_map(|((manufacturer_key, model_key), mut models)| {
                if models.len() < 2 {
                    return None;
                }
                models.sort_by_key(|model| model.id);
                Some(DuplicateAuditGroup {
                    manufacturer_key,
                    model_key,
                    models,
                })
            })
            .collect()
    }

    let exact_groups = groups(models.iter().map(|model| {
        (
            (
                model.stored_manufacturer_key.clone(),
                model.stored_model_key.clone(),
            ),
            model.audit_model(),
        )
    }));
    let canonical_groups = groups(models.iter().map(|model| {
        (
            (
                normalize_avionics_manufacturer_name(&model.manufacturer),
                normalize_avionics_model_name(&model.model),
            ),
            model.audit_model(),
        )
    }));
    let mut identifier_grouped =
        BTreeMap::<(String, StableIdentifierKey), Vec<DuplicateAuditModel>>::new();
    for model in models {
        let Some(identifier_key) = stable_identifier_key(model) else {
            continue;
        };
        identifier_grouped
            .entry((
                normalize_avionics_manufacturer_name(&model.manufacturer),
                identifier_key,
            ))
            .or_default()
            .push(model.audit_model());
    }
    let identifier_groups = identifier_grouped
        .into_iter()
        .filter_map(|((manufacturer_key, identifier_key), mut models)| {
            if models.len() < 2 {
                return None;
            }
            models.sort_by_key(|model| model.id);
            Some(DuplicateIdentifierAuditGroup {
                manufacturer_key,
                identifier_kind: identifier_key.kind,
                identifier_key: identifier_key.value,
                models,
            })
        })
        .collect();
    AvionicsDuplicateAudit {
        model_count: models.len(),
        exact_groups,
        canonical_groups,
        identifier_groups,
    }
}

fn stable_identifier_key(model: &ModelRow) -> Option<StableIdentifierKey> {
    let kind = model.manufacturer_identifier_kind.as_deref()?.trim();
    if kind.is_empty() {
        return None;
    }
    let value = model
        .manufacturer_identifier
        .as_deref()
        .map(normalize_avionics_identifier)
        .filter(|value| !value.is_empty())?;
    Some(StableIdentifierKey {
        kind: kind.to_string(),
        value,
    })
}

fn normalized_request(
    request: &AvionicsConsolidationRequest,
) -> ConsolidationResult<(i64, BTreeSet<i64>)> {
    if request.survivor_id <= 0 {
        return Err(ConsolidationError::Validation(
            "survivor_id must be positive".to_string(),
        ));
    }
    let mut duplicate_ids = BTreeSet::new();
    for duplicate_id in &request.duplicate_ids {
        if *duplicate_id <= 0 {
            return Err(ConsolidationError::Validation(
                "duplicate IDs must be positive".to_string(),
            ));
        }
        if *duplicate_id == request.survivor_id {
            return Err(ConsolidationError::Validation(
                "the survivor cannot also be a duplicate".to_string(),
            ));
        }
        if !duplicate_ids.insert(*duplicate_id) {
            return Err(ConsolidationError::Validation(format!(
                "duplicate catalog id {duplicate_id} was supplied more than once"
            )));
        }
    }
    if duplicate_ids.is_empty() {
        return Err(ConsolidationError::Validation(
            "at least one duplicate ID is required".to_string(),
        ));
    }
    Ok((request.survivor_id, duplicate_ids))
}

fn manufacturer_scope_is_authorized(
    state: &CatalogState,
    left: &ModelRow,
    right: &ModelRow,
) -> bool {
    let (Some(left_identity), Some(right_identity)) = (
        left.avionics_manufacturer_identity_id,
        right.avionics_manufacturer_identity_id,
    ) else {
        return false;
    };
    left_identity == right_identity
        || state
            .approved_manufacturer_alias_scopes
            .iter()
            .any(|scope| {
                (scope.source_manufacturer_identity_id == left_identity
                    && scope.target_manufacturer_identity_id == right_identity)
                    || (scope.source_manufacturer_identity_id == right_identity
                        && scope.target_manufacturer_identity_id == left_identity)
            })
}

fn identity_match(
    state: &CatalogState,
    survivor: &ModelRow,
    duplicate: &ModelRow,
) -> Option<IdentityMatchBasis> {
    let manufacturer_match = manufacturer_scope_is_authorized(state, survivor, duplicate);
    let survivor_identifier = stable_identifier_key(survivor);
    let duplicate_identifier = stable_identifier_key(duplicate);
    let identifiers_conflict = matches!(
        (&survivor_identifier, &duplicate_identifier),
        (Some(left), Some(right)) if left != right
    );
    let canonical_match = manufacturer_match
        && !identifiers_conflict
        && normalize_avionics_model_name(&survivor.model)
            == normalize_avionics_model_name(&duplicate.model);
    let identifier_match = match (survivor_identifier, duplicate_identifier) {
        (Some(left), Some(right)) => manufacturer_match && left == right,
        _ => false,
    };
    match (canonical_match, identifier_match) {
        (true, true) => Some(IdentityMatchBasis::Both),
        (true, false) => Some(IdentityMatchBasis::CanonicalManufacturerAndModel),
        (false, true) => Some(IdentityMatchBasis::StableManufacturerIdentifier),
        (false, false) => None,
    }
}

fn exact_identity_graph_blockers(state: &CatalogState, models: &[&ModelRow]) -> Vec<String> {
    let mut blockers = Vec::new();
    for model in models {
        if let Some(identifier) = model.manufacturer_identifier.as_deref() {
            let current_key = normalize_avionics_identifier(identifier);
            if !current_key.is_empty()
                && model.stored_manufacturer_identifier_key.as_deref() != Some(current_key.as_str())
            {
                blockers.push(format!(
                    "catalog id {} has a stale or corrupt persisted manufacturer-identifier key; repair and re-review it before consolidation",
                    model.id
                ));
            }
        }
    }
    for (index, left) in models.iter().enumerate() {
        for right in models.iter().skip(index + 1) {
            match identity_match(state, left, right) {
                Some(IdentityMatchBasis::StableManufacturerIdentifier)
                | Some(IdentityMatchBasis::Both) => {}
                Some(IdentityMatchBasis::CanonicalManufacturerAndModel) => {
                    blockers.push(format!(
                        "catalog ids {} and {} share only a mechanically normalized maker/model label; grounded same-product evidence is required before destructive consolidation",
                        left.id, right.id
                    ));
                }
                Some(IdentityMatchBasis::HumanReviewedModelEquivalence) => {
                    blockers.push(format!(
                        "catalog ids {} and {} require an explicit human-reviewed model-equivalence authorization",
                        left.id, right.id
                    ));
                }
                None => {
                    blockers.push(format!(
                        "catalog ids {} and {} are only transitively connected and share no direct exact maker-scoped stable identifier kind/value pair",
                        left.id, right.id
                    ));
                }
            }
        }
    }
    blockers
}

fn remap_model_id(id: i64, survivor_id: i64, duplicates: &BTreeSet<i64>) -> i64 {
    if duplicates.contains(&id) {
        survivor_id
    } else {
        id
    }
}

fn remap_optional_model_id(
    id: Option<i64>,
    survivor_id: i64,
    duplicates: &BTreeSet<i64>,
) -> Option<i64> {
    id.map(|id| remap_model_id(id, survivor_id, duplicates))
}

fn conservative_confidence<'a>(
    values: impl IntoIterator<Item = Option<&'a str>>,
) -> Option<String> {
    fn rank(value: &str) -> Option<u8> {
        match value {
            "low" => Some(0),
            "medium" => Some(1),
            "high" => Some(2),
            _ => None,
        }
    }
    let mut selected: Option<(&str, u8)> = None;
    for value in values {
        let value = value?;
        let value_rank = rank(value)?;
        if selected.is_none_or(|(_, selected_rank)| value_rank < selected_rank) {
            selected = Some((value, value_rank));
        }
    }
    selected.map(|(value, _)| value.to_string())
}

fn combined_lines(values: impl IntoIterator<Item = String>) -> String {
    let mut lines = Vec::new();
    for value in values {
        for line in value.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if !lines.iter().any(|known| known == line) {
                lines.push(line.to_string());
            }
        }
    }
    lines.join("\n")
}

fn merged_listing_source(rows: &[ListingLinkRow]) -> String {
    let sources = rows
        .iter()
        .map(|row| row.source.as_str())
        .collect::<BTreeSet<_>>();
    if sources.len() == 1 {
        return sources.into_iter().next().unwrap_or_default().to_string();
    }
    if sources
        .iter()
        .all(|source| matches!(*source, "listing" | "listing_review"))
        && sources.contains("listing_review")
    {
        return "listing_review".to_string();
    }
    // Mixed factory/reference and listing provenance must not accidentally
    // become valuation-eligible merely because catalog IDs were consolidated.
    "catalog_consolidation".to_string()
}

fn listing_provenance_notes(rows: &[ListingLinkRow]) -> Option<String> {
    let mut values = rows
        .iter()
        .filter_map(|row| row.source_notes.clone())
        .collect::<Vec<_>>();
    if rows
        .iter()
        .map(|row| row.source.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        > 1
    {
        values.extend(rows.iter().map(|row| {
            format!(
                "Catalog consolidation retained association {} provenance: source={}, confidence={}",
                row.id,
                row.source,
                row.source_confidence.as_deref().unwrap_or("unknown")
            )
        }));
    }
    let notes = combined_lines(values);
    (!notes.is_empty()).then_some(notes)
}

fn model_is_affected(model_id: i64, duplicate_ids: &BTreeSet<i64>) -> bool {
    duplicate_ids.contains(&model_id)
}

fn listing_link_is_affected(row: &ListingLinkRow, duplicate_ids: &BTreeSet<i64>) -> bool {
    model_is_affected(row.avionics_model_id, duplicate_ids)
        || row
            .replaces_avionics_model_id
            .is_some_and(|id| model_is_affected(id, duplicate_ids))
}

fn plan_listing_links(
    rows: &[ListingLinkRow],
    survivor_id: i64,
    duplicate_ids: &BTreeSet<i64>,
    blockers: &mut Vec<String>,
) -> (Vec<ListingLinkPlan>, HashMap<i64, i64>) {
    let mut grouped = BTreeMap::<(i64, i64), Vec<ListingLinkRow>>::new();
    for row in rows {
        let mapped_model = remap_model_id(row.avionics_model_id, survivor_id, duplicate_ids);
        grouped
            .entry((row.aircraft_sale_listing_id, mapped_model))
            .or_default()
            .push(row.clone());
    }
    let mut plans = Vec::new();
    let mut link_mapping = HashMap::new();
    for ((listing_id, mapped_model), mut group) in grouped {
        if !group
            .iter()
            .any(|row| listing_link_is_affected(row, duplicate_ids))
        {
            continue;
        }
        if group.len() > 1 {
            blockers.push(format!(
                "listing {listing_id} has multiple associations that would collapse onto avionics model {mapped_model}; reviewer evidence is required to distinguish duplicate mentions from multiple physical units"
            ));
            continue;
        }
        group.sort_by_key(|row| (usize::from(row.avionics_model_id != survivor_id), row.id));
        let mut keeper = group[0].clone();
        let mapped_actions = group
            .iter()
            .map(|row| {
                (
                    row.configuration_action.as_str(),
                    remap_optional_model_id(
                        row.replaces_avionics_model_id,
                        survivor_id,
                        duplicate_ids,
                    ),
                )
            })
            .collect::<BTreeSet<_>>();
        if mapped_actions.len() != 1 {
            blockers.push(format!(
                "listing {listing_id} has conflicting installation actions or replacement targets for models consolidated into {mapped_model}"
            ));
            continue;
        }
        let (configuration_action, replacement_id) = mapped_actions
            .into_iter()
            .next()
            .expect("listing group is non-empty");
        let invalid_action = match configuration_action {
            "installed" => replacement_id.is_some(),
            "replaces" => replacement_id.is_none() || replacement_id == Some(mapped_model),
            "removes" => replacement_id != Some(mapped_model),
            _ => true,
        };
        if invalid_action {
            blockers.push(format!(
                "listing {listing_id} would have invalid {configuration_action} subject/target semantics for avionics model {mapped_model}"
            ));
            continue;
        }
        keeper.avionics_model_id = mapped_model;
        keeper.configuration_action = configuration_action.to_string();
        keeper.replaces_avionics_model_id = replacement_id;
        keeper.quantity = group
            .iter()
            .map(|row| row.quantity.max(1))
            .max()
            .unwrap_or(1);
        keeper.source = merged_listing_source(&group);
        keeper.source_notes = listing_provenance_notes(&group);
        keeper.source_confidence =
            conservative_confidence(group.iter().map(|row| row.source_confidence.as_deref()));
        let deleted_ids = group
            .iter()
            .filter(|row| row.id != keeper.id)
            .map(|row| row.id)
            .collect::<Vec<_>>();
        for row in &group {
            link_mapping.insert(row.id, keeper.id);
        }
        plans.push(ListingLinkPlan {
            keeper,
            deleted_ids,
        });
    }
    validate_post_remap_listing_action_graph(rows, &plans, blockers);
    (plans, link_mapping)
}

fn validate_post_remap_listing_action_graph(
    rows: &[ListingLinkRow],
    plans: &[ListingLinkPlan],
    blockers: &mut Vec<String>,
) {
    let planned_by_id = plans
        .iter()
        .map(|plan| (plan.keeper.id, &plan.keeper))
        .collect::<HashMap<_, _>>();
    let deleted_ids = plans
        .iter()
        .flat_map(|plan| plan.deleted_ids.iter().copied())
        .collect::<HashSet<_>>();
    let mut by_listing = BTreeMap::<i64, Vec<&ListingLinkRow>>::new();
    for original in rows {
        if deleted_ids.contains(&original.id) {
            continue;
        }
        let effective = planned_by_id.get(&original.id).copied().unwrap_or(original);
        by_listing
            .entry(effective.aircraft_sale_listing_id)
            .or_default()
            .push(effective);
    }

    for (listing_id, links) in by_listing {
        let mut installed_subjects = HashSet::new();
        let mut displacement_targets = HashSet::new();
        let mut issue = None;
        for link in links {
            match link.configuration_action.as_str() {
                "installed" if link.replaces_avionics_model_id.is_none() => {
                    if !installed_subjects.insert(link.avionics_model_id) {
                        issue = Some(format!(
                            "avionics model {} is installed by more than one action",
                            link.avionics_model_id
                        ));
                        break;
                    }
                }
                "replaces" => {
                    let Some(target) = link.replaces_avionics_model_id else {
                        issue = Some(format!(
                            "replacement subject {} has no displaced target",
                            link.avionics_model_id
                        ));
                        break;
                    };
                    if target == link.avionics_model_id {
                        issue = Some(format!(
                            "avionics model {} replaces itself",
                            link.avionics_model_id
                        ));
                        break;
                    }
                    if !installed_subjects.insert(link.avionics_model_id) {
                        issue = Some(format!(
                            "avionics model {} is installed by more than one action",
                            link.avionics_model_id
                        ));
                        break;
                    }
                    if !displacement_targets.insert(target) {
                        issue = Some(format!(
                            "avionics model {target} is displaced more than once"
                        ));
                        break;
                    }
                }
                "removes" => {
                    if link.replaces_avionics_model_id != Some(link.avionics_model_id) {
                        issue = Some(format!(
                            "removal subject {} differs from its displaced target",
                            link.avionics_model_id
                        ));
                        break;
                    }
                    if !displacement_targets.insert(link.avionics_model_id) {
                        issue = Some(format!(
                            "avionics model {} is displaced more than once",
                            link.avionics_model_id
                        ));
                        break;
                    }
                }
                unsupported => {
                    issue = Some(format!(
                        "association {} has unsupported action {unsupported:?}",
                        link.id
                    ));
                    break;
                }
            }
        }
        if issue.is_none() {
            if let Some(model_id) = installed_subjects
                .intersection(&displacement_targets)
                .next()
            {
                issue = Some(format!(
                    "avionics model {model_id} is both installed and displaced"
                ));
            }
        }
        if let Some(issue) = issue {
            blockers.push(format!(
                "listing {listing_id} would have an invalid avionics action graph after consolidation: {issue}"
            ));
        }
    }
}

fn plan_default_links(
    rows: &[DefaultLinkRow],
    survivor_id: i64,
    duplicate_ids: &BTreeSet<i64>,
) -> Vec<DefaultLinkPlan> {
    let mut grouped = BTreeMap::<(i64, i64, i64), Vec<DefaultLinkRow>>::new();
    for row in rows {
        grouped
            .entry((
                row.aircraft_model_variant_id,
                row.model_year,
                remap_model_id(row.avionics_model_id, survivor_id, duplicate_ids),
            ))
            .or_default()
            .push(row.clone());
    }
    let mut plans = Vec::new();
    for ((_variant_id, _year, mapped_model), mut group) in grouped {
        if !group
            .iter()
            .any(|row| duplicate_ids.contains(&row.avionics_model_id))
        {
            continue;
        }
        group.sort_by_key(|row| (usize::from(row.avionics_model_id != survivor_id), row.id));
        let mut keeper = group[0].clone();
        keeper.avionics_model_id = mapped_model;
        keeper.quantity = group
            .iter()
            .map(|row| row.quantity.max(1))
            .max()
            .unwrap_or(1);
        keeper.source_confidence =
            conservative_confidence(group.iter().map(|row| Some(row.source_confidence.as_str())))
                .unwrap_or_else(|| "low".to_string());
        let mut notes = group
            .iter()
            .map(|row| row.source_notes.clone())
            .collect::<Vec<_>>();
        if group
            .iter()
            .map(|row| (row.source_url.as_str(), row.source_title.as_str()))
            .collect::<BTreeSet<_>>()
            .len()
            > 1
        {
            notes.extend(group.iter().map(|row| {
                format!(
                    "Catalog consolidation retained default-link {} evidence: {} ({})",
                    row.id, row.source_title, row.source_url
                )
            }));
        }
        keeper.source_notes = combined_lines(notes);
        let deleted_ids = group
            .iter()
            .filter(|row| row.id != keeper.id)
            .map(|row| row.id)
            .collect();
        plans.push(DefaultLinkPlan {
            keeper,
            deleted_ids,
        });
    }
    plans
}

fn default_candidate_claims_are_identical(
    left: &DefaultCandidateLinkRow,
    right: &DefaultCandidateLinkRow,
) -> bool {
    left.quarantined_default_avionics_id == right.quarantined_default_avionics_id
        && left.aircraft_model_variant_id == right.aircraft_model_variant_id
        && left.model_year == right.model_year
        && left.quantity == right.quantity
        && left.source_url == right.source_url
        && left.source_title == right.source_title
        && left.source_notes == right.source_notes
        && left.source_confidence == right.source_confidence
        && left.pending_reason == right.pending_reason
        && left.quarantined_created_at == right.quarantined_created_at
        && left.quarantined_updated_at == right.quarantined_updated_at
}

fn plan_default_candidate_links(
    rows: &[DefaultCandidateLinkRow],
    canonical_rows: &[DefaultLinkRow],
    survivor_id: i64,
    duplicate_ids: &BTreeSet<i64>,
    blockers: &mut Vec<String>,
) -> Vec<DefaultCandidateLinkPlan> {
    let mut grouped = BTreeMap::<(i64, i64, i64), Vec<DefaultCandidateLinkRow>>::new();
    for row in rows {
        grouped
            .entry((
                row.aircraft_model_variant_id,
                row.model_year,
                remap_model_id(row.avionics_model_id, survivor_id, duplicate_ids),
            ))
            .or_default()
            .push(row.clone());
    }

    let mut plans = Vec::new();
    for ((variant_id, model_year, mapped_model_id), mut group) in grouped {
        if !group
            .iter()
            .any(|row| duplicate_ids.contains(&row.avionics_model_id))
        {
            continue;
        }
        if canonical_rows.iter().any(|row| {
            row.aircraft_model_variant_id == variant_id
                && row.model_year == model_year
                && row.avionics_model_id == mapped_model_id
        }) {
            blockers.push(format!(
                "pending default avionics candidate would collide with canonical default for aircraft variant {variant_id}, year {model_year}, model {mapped_model_id}"
            ));
            continue;
        }

        group.sort_by_key(|row| (usize::from(row.avionics_model_id != survivor_id), row.id));
        let keeper = &group[0];
        if group
            .iter()
            .skip(1)
            .any(|candidate| !default_candidate_claims_are_identical(keeper, candidate))
        {
            blockers.push(format!(
                "pending default avionics candidates for aircraft variant {variant_id}, year {model_year}, model {mapped_model_id} contain conflicting claims and cannot be coalesced"
            ));
            continue;
        }

        let mut remapped_keeper = keeper.clone();
        remapped_keeper.avionics_model_id = mapped_model_id;
        plans.push(DefaultCandidateLinkPlan {
            keeper: remapped_keeper,
            deleted_ids: group.iter().map(|row| row.id).collect(),
        });
    }
    plans
}

fn plan_reference_links(
    rows: &[ReferenceLinkRow],
    survivor_id: i64,
    duplicate_ids: &BTreeSet<i64>,
    blockers: &mut Vec<String>,
) -> Vec<ReferenceLinkPlan> {
    let mut grouped = BTreeMap::<(i64, i64), Vec<ReferenceLinkRow>>::new();
    for row in rows {
        grouped
            .entry((
                row.aircraft_reference_configuration_version_id,
                remap_model_id(row.avionics_model_id, survivor_id, duplicate_ids),
            ))
            .or_default()
            .push(row.clone());
    }
    let mut plans = Vec::new();
    for ((version_id, mapped_model), mut group) in grouped {
        if !group
            .iter()
            .any(|row| duplicate_ids.contains(&row.avionics_model_id))
        {
            continue;
        }
        let evidence = group
            .iter()
            .map(|row| (row.equipment_role.as_str(), row.evidence_claim_id))
            .collect::<BTreeSet<_>>();
        if evidence.len() != 1 {
            blockers.push(format!(
                "reference configuration version {version_id} has conflicting roles or evidence claims for models consolidated into {mapped_model}"
            ));
            continue;
        }
        if group.len() > 1 && group.iter().any(|row| row.publication_state != "building") {
            blockers.push(format!(
                "reference configuration version {version_id} is published or superseded and contains multiple rows that would collide on model {mapped_model}; historical reference facts cannot be deleted"
            ));
            continue;
        }
        let maximum_quantity = group
            .iter()
            .map(|row| row.quantity.max(1))
            .max()
            .unwrap_or(1);
        group.sort_by_key(|row| {
            (
                usize::from(row.quantity.max(1) != maximum_quantity),
                usize::from(row.avionics_model_id != survivor_id),
                row.id,
            )
        });
        let mut keeper = group[0].clone();
        let remap_keeper = keeper.avionics_model_id != survivor_id;
        keeper.avionics_model_id = mapped_model;
        // Reference facts are immutable. Choosing the row with the largest
        // existing quantity preserves the conservative coalescing policy
        // without mutating that quantity.
        keeper.quantity = maximum_quantity;
        let deleted_ids = group
            .iter()
            .filter(|row| row.id != keeper.id)
            .map(|row| row.id)
            .collect();
        plans.push(ReferenceLinkPlan {
            keeper,
            remap_keeper,
            deleted_ids,
        });
    }
    plans
}

fn plan_suite_links(
    rows: &[SuiteLinkRow],
    survivor_id: i64,
    duplicate_ids: &BTreeSet<i64>,
) -> (Vec<(i64, i64)>, Vec<SuiteLinkPlan>, usize) {
    let mut grouped = BTreeMap::<(i64, i64), Vec<SuiteLinkRow>>::new();
    let mut self_rows = Vec::new();
    for row in rows {
        let mapped_suite = remap_model_id(row.suite_model_id, survivor_id, duplicate_ids);
        let mapped_component = remap_model_id(row.component_model_id, survivor_id, duplicate_ids);
        if mapped_suite == mapped_component {
            if duplicate_ids.contains(&row.suite_model_id)
                || duplicate_ids.contains(&row.component_model_id)
            {
                self_rows.push((row.suite_model_id, row.component_model_id));
            }
            continue;
        }
        grouped
            .entry((mapped_suite, mapped_component))
            .or_default()
            .push(row.clone());
    }
    let mut delete_rows = self_rows.clone();
    let mut plans = Vec::new();
    for ((mapped_suite, mapped_component), mut group) in grouped {
        if !group.iter().any(|row| {
            duplicate_ids.contains(&row.suite_model_id)
                || duplicate_ids.contains(&row.component_model_id)
        }) {
            continue;
        }
        group.sort_by_key(|row| {
            (
                usize::from(
                    row.suite_model_id != mapped_suite
                        || row.component_model_id != mapped_component,
                ),
                row.suite_model_id,
                row.component_model_id,
            )
        });
        let keeper = &group[0];
        let deleted_keys = group
            .iter()
            .skip(1)
            .map(|row| (row.suite_model_id, row.component_model_id))
            .collect::<Vec<_>>();
        delete_rows.extend(deleted_keys.iter().copied());
        plans.push(SuiteLinkPlan {
            original_suite_model_id: keeper.suite_model_id,
            original_component_model_id: keeper.component_model_id,
            suite_model_id: mapped_suite,
            component_model_id: mapped_component,
            quantity: group
                .iter()
                .map(|row| row.quantity.max(1))
                .max()
                .unwrap_or(1),
            deleted_keys,
        });
    }
    (delete_rows, plans, self_rows.len())
}

fn review_product_references_duplicates(
    product: Option<&ReviewProduct>,
    duplicate_ids: &BTreeSet<i64>,
) -> bool {
    product
        .and_then(|product| product.id)
        .is_some_and(|id| duplicate_ids.contains(&id))
}

#[derive(Clone, Debug)]
struct HumanConsolidationScope {
    effective_manufacturer_identity_id: i64,
    member_model_keys: BTreeSet<String>,
}

fn canonical_model_key(value: &str) -> String {
    normalize_avionics_model_name(value)
}

fn idless_review_product_matches_equivalence_scope(
    state: &CatalogState,
    product: Option<&ReviewProduct>,
    scope: Option<&HumanConsolidationScope>,
) -> bool {
    let (Some(product), Some(scope)) = (product, scope) else {
        return false;
    };
    if product.id.is_some()
        || !scope
            .member_model_keys
            .contains(&canonical_model_key(&product.model))
    {
        return false;
    }
    let proposed_manufacturer_key = normalize_avionics_manufacturer_name(&product.manufacturer);
    let matching_identities = state
        .models
        .iter()
        .filter(|model| {
            normalize_avionics_manufacturer_name(&model.manufacturer) == proposed_manufacturer_key
        })
        .filter_map(|model| model.avionics_manufacturer_identity_id)
        .collect::<BTreeSet<_>>();
    matching_identities.len() == 1
        && matching_identities.contains(&scope.effective_manufacturer_identity_id)
}

fn pending_review_references_duplicates(
    state: &CatalogState,
    payload: &StoredReviewPayload,
    duplicate_ids: &BTreeSet<i64>,
    link_mapping: &HashMap<i64, i64>,
    scope: Option<&HumanConsolidationScope>,
) -> bool {
    payload.aspects.iter().any(|aspect| {
        review_product_references_duplicates(aspect.suggested_product.as_ref(), duplicate_ids)
            || review_product_references_duplicates(aspect.proposed_product.as_ref(), duplicate_ids)
            || idless_review_product_matches_equivalence_scope(
                state,
                aspect.proposed_product.as_ref(),
                scope,
            )
            || aspect
                .replaces_product_id
                .is_some_and(|id| duplicate_ids.contains(&id))
            || aspect.covered_associations.iter().any(|association| {
                duplicate_ids.contains(&association.avionics_model_id)
                    || link_mapping
                        .get(&association.listing_link_id)
                        .is_some_and(|mapped| *mapped != association.listing_link_id)
            })
    })
}

const LEGACY_DUPLICATE_CONSOLIDATION_REASON_PREFIX: &str =
    "independent collision review confirmed multiple legacy catalog rows as the same product";
const LEGACY_DUPLICATE_CONSOLIDATION_REASON_SUFFIX: &str =
    "explicitly consolidate them before approving the identity";
const CONSOLIDATED_PENDING_IDENTITY_REASON: &str =
    "catalog_collision_consolidated_pending_identity_verification";

fn rewrite_consolidated_pending_identity_reason(
    aspect: &mut PendingReviewAspect,
    model_equivalence_remapped: bool,
) {
    let reason = aspect.reason.trim();
    if model_equivalence_remapped
        && reason.starts_with(LEGACY_DUPLICATE_CONSOLIDATION_REASON_PREFIX)
        && reason.ends_with(LEGACY_DUPLICATE_CONSOLIDATION_REASON_SUFFIX)
    {
        aspect.reason = CONSOLIDATED_PENDING_IDENTITY_REASON.to_string();
    }
}

fn remap_pending_review(
    state: &CatalogState,
    row: &PendingReviewRow,
    survivor: &ModelRow,
    duplicate_ids: &BTreeSet<i64>,
    link_mapping: &HashMap<i64, i64>,
    scope: Option<&HumanConsolidationScope>,
) -> Result<Option<PendingReviewPlan>, String> {
    let mut payload: StoredReviewPayload = serde_json::from_str(&row.review_payload_json)
        .map_err(|error| format!("pending review {} contains invalid JSON: {error}", row.id))?;
    if payload.version != 1 {
        return Err(format!(
            "pending review {} uses unsupported payload version {}",
            row.id, payload.version
        ));
    }
    if !pending_review_references_duplicates(state, &payload, duplicate_ids, link_mapping, scope) {
        return Ok(None);
    }

    for aspect in &mut payload.aspects {
        let mut model_equivalence_remapped = false;
        if let Some(product) = &mut aspect.suggested_product {
            if product.id.is_some_and(|id| duplicate_ids.contains(&id)) {
                product.id = Some(survivor.id);
                model_equivalence_remapped = true;
            }
        }
        if let Some(product) = &mut aspect.proposed_product {
            let idless_equivalence_match =
                idless_review_product_matches_equivalence_scope(state, Some(product), scope);
            if product.id.is_some_and(|id| duplicate_ids.contains(&id)) || idless_equivalence_match
            {
                model_equivalence_remapped = true;
                if survivor.catalog_status == "approved" {
                    if aspect
                        .suggested_product
                        .as_ref()
                        .is_some_and(|suggested| suggested.id.is_some_and(|id| id != survivor.id))
                    {
                        return Err(format!(
                            "pending review {} aspect {} already suggests a different approved product",
                            row.id, aspect.id
                        ));
                    }
                    let mut suggested = product.clone();
                    suggested.id = Some(survivor.id);
                    aspect.suggested_product = Some(suggested);
                    // An approved survivor is no longer an in-place legacy
                    // promotion target. Keeping this proposal ID would make a
                    // corrected Create decision stale instead of independent.
                    product.id = None;
                } else {
                    product.id = Some(survivor.id);
                }
            }
        }
        if scope.is_some() {
            rewrite_consolidated_pending_identity_reason(aspect, model_equivalence_remapped);
        }
        aspect.replaces_product_id =
            remap_optional_model_id(aspect.replaces_product_id, survivor.id, duplicate_ids);
        for association in &mut aspect.covered_associations {
            association.avionics_model_id =
                remap_model_id(association.avionics_model_id, survivor.id, duplicate_ids);
            if let Some(mapped) = link_mapping.get(&association.listing_link_id) {
                association.listing_link_id = *mapped;
            }
        }
        aspect.covered_associations.sort();
        aspect.covered_associations.dedup();
    }
    let serialized = serialize_review_payload(&payload.aspects).map_err(|error| {
        format!(
            "pending review {} cannot preserve exact coverage after consolidation: {error}",
            row.id
        )
    })?;
    Ok(Some(PendingReviewPlan {
        id: row.id,
        extraction_sha256: serialized.extraction_sha256,
        pending_aspect_count: serialized.pending_aspect_count,
        review_payload_json: serialized.review_payload_json,
        review_payload_sha256: serialized.review_payload_sha256,
    }))
}

#[derive(Clone, Copy)]
enum ConsolidationIdentityAuthority<'a> {
    StableIdentifier,
    HumanReviewedModelEquivalence(&'a HumanConsolidationScope),
}

fn build_plan_with_authority(
    state: &CatalogState,
    request: &AvionicsConsolidationRequest,
    authority: ConsolidationIdentityAuthority<'_>,
) -> ConsolidationResult<ConsolidationPlan> {
    let (survivor_id, duplicate_ids) = normalized_request(request)?;
    let by_id = state
        .models
        .iter()
        .map(|model| (model.id, model))
        .collect::<HashMap<_, _>>();
    let survivor = by_id.get(&survivor_id).copied().ok_or_else(|| {
        ConsolidationError::Validation(format!(
            "survivor avionics catalog id {survivor_id} does not exist"
        ))
    })?;
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
    let mut matches = Vec::new();
    for duplicate_id in &duplicate_ids {
        let duplicate = by_id.get(duplicate_id).copied().ok_or_else(|| {
            ConsolidationError::Validation(format!(
                "duplicate avionics catalog id {duplicate_id} does not exist"
            ))
        })?;
        let identity_basis = identity_match(state, survivor, duplicate).or_else(|| {
            let ConsolidationIdentityAuthority::HumanReviewedModelEquivalence(scope) = authority
            else {
                return None;
            };
            (duplicate.avionics_manufacturer_identity_id
                == Some(scope.effective_manufacturer_identity_id)
                && matches!(
                    current_model_identity_relation(state, survivor, duplicate),
                    Some(AvionicsModelIdentityRelation::TypographyExact)
                        | Some(AvionicsModelIdentityRelation::DescriptiveExpansion)
                ))
            .then_some(IdentityMatchBasis::HumanReviewedModelEquivalence)
        });
        match identity_basis {
            Some(basis) => matches.push(ConsolidationIdentityMatch {
                duplicate: duplicate.summary(),
                basis,
            }),
            None => {}
        }
        let survivor_identifier = stable_identifier_key(survivor);
        let duplicate_identifier = stable_identifier_key(duplicate);
        if survivor_identifier.is_some()
            && duplicate_identifier.is_some()
            && survivor_identifier != duplicate_identifier
        {
            blockers.push(format!(
                "catalog ids {survivor_id} and {duplicate_id} have conflicting stable identifier kind/value pairs; grounded adjudication is required before consolidation"
            ));
        }
    }
    let selected_models = std::iter::once(survivor)
        .chain(duplicate_ids.iter().filter_map(|id| by_id.get(id).copied()))
        .collect::<Vec<_>>();
    if matches!(authority, ConsolidationIdentityAuthority::StableIdentifier) {
        blockers.extend(exact_identity_graph_blockers(state, &selected_models));
        for omitted in state
            .models
            .iter()
            .filter(|model| model.id != survivor_id && !duplicate_ids.contains(&model.id))
        {
            if let Some(basis) = identity_match(state, survivor, omitted) {
                blockers.push(format!(
                    "catalog id {} is an omitted {:?} identity collision with survivor {survivor_id}; every exact duplicate must be included in one consolidation request",
                    omitted.id, basis
                ));
            }
        }
    }

    if matches!(
        authority,
        ConsolidationIdentityAuthority::HumanReviewedModelEquivalence(_)
    ) && selected_models
        .iter()
        .any(|model| model.catalog_status != "unreviewed")
    {
        blockers.push(
            "human-reviewed model-equivalence consolidation permits only unreviewed catalog rows"
                .to_string(),
        );
    }
    if survivor.catalog_status == "rejected" {
        blockers
            .push("a rejected catalog product cannot be the consolidation survivor".to_string());
    }
    if survivor.catalog_status == "unreviewed"
        && duplicate_ids.iter().any(|id| {
            by_id
                .get(id)
                .is_some_and(|model| model.catalog_status != "unreviewed")
        })
    {
        blockers
            .push("an unreviewed survivor may consolidate only unreviewed legacy rows".to_string());
    }
    if duplicate_ids.iter().any(|id| {
        by_id
            .get(id)
            .is_some_and(|model| model.catalog_status == "rejected")
    }) {
        blockers.push("a rejected catalog product cannot be consolidated".to_string());
    }

    let survivor_type_ids = state
        .model_types
        .iter()
        .filter(|row| row.avionics_model_id == survivor_id)
        .map(|row| row.avionics_type_id)
        .collect::<HashSet<_>>();
    let duplicate_type_ids = state
        .model_types
        .iter()
        .filter(|row| duplicate_ids.contains(&row.avionics_model_id))
        .map(|row| row.avionics_type_id)
        .filter(|type_id| !survivor_type_ids.contains(type_id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let type_ids_to_add = if survivor.catalog_status == "approved" {
        if !duplicate_type_ids.is_empty() {
            blockers.push(format!(
                "approved survivor {survivor_id} is missing duplicate capability IDs {:?}; curate those capabilities with authoritative evidence before consolidation",
                duplicate_type_ids
            ));
        }
        Vec::new()
    } else {
        duplicate_type_ids
    };

    let (listing_links, link_mapping) = plan_listing_links(
        &state.listing_links,
        survivor_id,
        &duplicate_ids,
        &mut blockers,
    );
    let default_links = plan_default_links(&state.default_links, survivor_id, &duplicate_ids);
    let default_candidate_links = plan_default_candidate_links(
        &state.default_candidate_links,
        &state.default_links,
        survivor_id,
        &duplicate_ids,
        &mut blockers,
    );
    let reference_links = plan_reference_links(
        &state.reference_links,
        survivor_id,
        &duplicate_ids,
        &mut blockers,
    );
    let (suite_original_rows_to_delete, suite_links_to_upsert, suite_self_links_removed) =
        plan_suite_links(&state.suite_links, survivor_id, &duplicate_ids);

    for row in state
        .listing_links
        .iter()
        .filter(|row| listing_link_is_affected(row, &duplicate_ids))
    {
        if row.listing_ingestion_state == "ready" || row.listing_is_verified {
            blockers.push(format!(
                "listing {} is ready or verified and its avionics associations are immutable",
                row.aircraft_sale_listing_id
            ));
        }
    }
    if survivor.catalog_status == "unreviewed" {
        warnings.push(
            "legacy unreviewed consolidation resets the survivor's value and suite-scope metadata"
                .to_string(),
        );
    }

    let mut pending_reviews = Vec::new();
    let human_scope = match authority {
        ConsolidationIdentityAuthority::StableIdentifier => None,
        ConsolidationIdentityAuthority::HumanReviewedModelEquivalence(scope) => Some(scope),
    };
    for row in &state.pending_reviews {
        match remap_pending_review(
            state,
            row,
            survivor,
            &duplicate_ids,
            &link_mapping,
            human_scope,
        ) {
            Ok(Some(plan)) => pending_reviews.push(plan),
            Ok(None) => {}
            Err(error) => blockers.push(error),
        }
    }

    blockers.sort();
    blockers.dedup();
    warnings.sort();
    warnings.dedup();
    matches.sort_by_key(|item| item.duplicate.id);
    let changes = ConsolidationChangeCounts {
        model_rows_deleted: duplicate_ids.len(),
        type_memberships_added: type_ids_to_add.len(),
        listing_links_remapped: state
            .listing_links
            .iter()
            .filter(|row| listing_link_is_affected(row, &duplicate_ids))
            .count(),
        listing_link_conflicts_coalesced: listing_links
            .iter()
            .map(|plan| plan.deleted_ids.len())
            .sum(),
        default_links_remapped: state
            .default_links
            .iter()
            .filter(|row| duplicate_ids.contains(&row.avionics_model_id))
            .count(),
        default_link_conflicts_coalesced: default_links
            .iter()
            .map(|plan| plan.deleted_ids.len())
            .sum(),
        default_candidate_links_remapped: state
            .default_candidate_links
            .iter()
            .filter(|row| duplicate_ids.contains(&row.avionics_model_id))
            .count(),
        default_candidate_link_conflicts_coalesced: default_candidate_links
            .iter()
            .map(|plan| plan.deleted_ids.len().saturating_sub(1))
            .sum(),
        reference_links_remapped: state
            .reference_links
            .iter()
            .filter(|row| duplicate_ids.contains(&row.avionics_model_id))
            .count(),
        reference_link_conflicts_coalesced: reference_links
            .iter()
            .map(|plan| plan.deleted_ids.len())
            .sum(),
        suite_links_remapped: state
            .suite_links
            .iter()
            .filter(|row| {
                duplicate_ids.contains(&row.suite_model_id)
                    || duplicate_ids.contains(&row.component_model_id)
            })
            .count(),
        suite_link_conflicts_coalesced: suite_links_to_upsert
            .iter()
            .map(|plan| plan.deleted_keys.len())
            .sum(),
        suite_self_links_removed,
        pending_reviews_rewritten: pending_reviews.len(),
    };
    let can_apply = blockers.is_empty();
    let report = AvionicsConsolidationReport {
        dry_run: true,
        applied: false,
        can_apply,
        survivor: survivor.summary(),
        matches,
        changes,
        blockers,
        warnings,
    };
    Ok(ConsolidationPlan {
        report,
        duplicate_ids,
        type_ids_to_add,
        listing_links,
        default_links,
        default_candidate_links,
        reference_links,
        suite_original_rows_to_delete,
        suite_links_to_upsert,
        pending_reviews,
    })
}

fn build_plan(
    state: &CatalogState,
    request: &AvionicsConsolidationRequest,
) -> ConsolidationResult<ConsolidationPlan> {
    build_plan_with_authority(
        state,
        request,
        ConsolidationIdentityAuthority::StableIdentifier,
    )
}

fn feed_fingerprint(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_string().as_bytes());
    hasher.update(b":");
    hasher.update(value.as_bytes());
    hasher.update(b";");
}

fn feed_optional_fingerprint(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update(b"some:");
            feed_fingerprint(hasher, value);
        }
        None => hasher.update(b"none;"),
    }
}

fn row_identity_sha256(
    model: &ModelRow,
    effective_manufacturer_identity_id: i64,
    canonical_model_key: &str,
    capabilities: &[String],
) -> String {
    let mut hasher = Sha256::new();
    for value in [
        "aircost:human-reviewed-avionics-consolidation-row:v2",
        &model.id.to_string(),
        &model.avionics_manufacturer_id.to_string(),
        &effective_manufacturer_identity_id.to_string(),
        model.manufacturer.as_str(),
        model.stored_manufacturer_key.as_str(),
        model.model.as_str(),
        model.stored_model_key.as_str(),
        canonical_model_key,
        model.catalog_status.as_str(),
        model.identity_evidence_kind.as_str(),
    ] {
        feed_fingerprint(&mut hasher, value);
    }
    for value in [
        model.manufacturer_identifier_kind.as_deref(),
        model.manufacturer_identifier.as_deref(),
        model.stored_manufacturer_identifier_key.as_deref(),
        model.identity_source_url.as_deref(),
        model.identity_source_title.as_deref(),
        model.identity_evidence_text.as_deref(),
        model.identity_confidence.as_deref(),
        model.catalog_reviewed_at.as_deref(),
    ] {
        feed_optional_fingerprint(&mut hasher, value);
    }
    let mut capabilities = capabilities.to_vec();
    capabilities.sort();
    capabilities.dedup();
    feed_fingerprint(&mut hasher, &capabilities.len().to_string());
    for capability in capabilities {
        feed_fingerprint(&mut hasher, &capability);
    }
    format!("{:x}", hasher.finalize())
}

fn validate_authoritative_source_url(value: &str) -> ConsolidationResult<()> {
    if value.trim() != value {
        return Err(ConsolidationError::Validation(
            "authoritative_source_url must not contain surrounding whitespace".to_string(),
        ));
    }
    let parsed = url::Url::parse(value).map_err(|_| {
        ConsolidationError::Validation(
            "authoritative_source_url must be a valid authoritative HTTPS URL".to_string(),
        )
    })?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        return Err(ConsolidationError::Validation(
            "authoritative_source_url must use HTTPS and include a host".to_string(),
        ));
    }
    let lower = value.to_ascii_lowercase();
    if [
        "/listing/",
        "/listings/",
        "/aircraft-for-sale/",
        "/classifieds/",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return Err(ConsolidationError::Validation(
            "authoritative_source_url must cite authoritative product evidence, not a sale listing"
                .to_string(),
        ));
    }
    Ok(())
}

fn valid_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn human_request_base(
    request: &HumanReviewedAvionicsConsolidationRequest,
) -> ConsolidationResult<AvionicsConsolidationRequest> {
    let base = AvionicsConsolidationRequest {
        survivor_id: request.survivor_id,
        duplicate_ids: request.duplicate_ids.clone(),
    };
    normalized_request(&base)?;
    if request.reviewer_user_id <= 0 {
        return Err(ConsolidationError::Validation(
            "reviewer_user_id must be positive".to_string(),
        ));
    }
    validate_authoritative_source_url(&request.authoritative_source_url)?;
    if request.authoritative_source_title.trim().is_empty() {
        return Err(ConsolidationError::Validation(
            "authoritative_source_title is required".to_string(),
        ));
    }
    if request.exact_evidence_text.trim().is_empty() {
        return Err(ConsolidationError::Validation(
            "exact_evidence_text is required".to_string(),
        ));
    }
    for (field, digest) in [
        (
            "expected_authorization_sha256",
            request.expected_authorization_sha256.as_deref(),
        ),
        (
            "expected_catalog_revision_sha256",
            request.expected_catalog_revision_sha256.as_deref(),
        ),
    ] {
        if digest.is_some_and(|digest| !valid_lower_sha256(digest)) {
            return Err(ConsolidationError::Validation(format!(
                "{field} must be a lowercase SHA-256 digest when supplied"
            )));
        }
    }
    if let Some(provenance) = &request.provenance {
        if provenance.listing_id <= 0
            || provenance.review_aspect_id.trim().is_empty()
            || !valid_lower_sha256(&provenance.expected_review_payload_sha256)
        {
            return Err(ConsolidationError::Validation(
                "review provenance requires a positive listing_id, nonblank review_aspect_id, and lowercase SHA-256 review payload digest".to_string(),
            ));
        }
    }
    Ok(base)
}

fn current_model_capabilities(state: &CatalogState, model_id: i64) -> Vec<String> {
    state
        .model_types
        .iter()
        .filter(|membership| membership.avionics_model_id == model_id)
        .map(|membership| membership.capability.clone())
        .collect()
}

fn current_model_identity_relation(
    state: &CatalogState,
    left: &ModelRow,
    right: &ModelRow,
) -> Option<AvionicsModelIdentityRelation> {
    avionics_model_identity_relation(
        &left.manufacturer,
        &left.model,
        &current_model_capabilities(state, left.id),
        &right.manufacturer,
        &right.model,
        &current_model_capabilities(state, right.id),
    )
}

fn snapshot_human_authorization(
    state: &CatalogState,
    request: &HumanReviewedAvionicsConsolidationRequest,
) -> ConsolidationResult<(
    AvionicsConsolidationRequest,
    HumanConsolidationScope,
    HumanReviewedConsolidationAuthorization,
)> {
    let base = human_request_base(request)?;
    if !state
        .users
        .iter()
        .any(|user| user.id == request.reviewer_user_id)
    {
        return Err(ConsolidationError::Validation(format!(
            "reviewer user {} does not exist",
            request.reviewer_user_id
        )));
    }
    let (survivor_id, duplicate_ids) = normalized_request(&base)?;
    let selected_ids = std::iter::once(survivor_id)
        .chain(duplicate_ids.iter().copied())
        .collect::<BTreeSet<_>>();
    let by_id = state
        .models
        .iter()
        .map(|model| (model.id, model))
        .collect::<HashMap<_, _>>();
    let survivor = by_id.get(&survivor_id).copied().ok_or_else(|| {
        ConsolidationError::Validation(format!(
            "survivor avionics catalog id {survivor_id} does not exist"
        ))
    })?;
    let manufacturer_identity_id = survivor
        .avionics_manufacturer_identity_id
        .ok_or_else(|| {
            ConsolidationError::Validation(format!(
                "survivor catalog id {survivor_id} has no evidence-backed effective manufacturer identity"
            ))
        })?;
    let survivor_model_key = canonical_model_key(&survivor.model);
    if survivor_model_key.is_empty() || survivor.stored_model_key != survivor_model_key {
        return Err(ConsolidationError::Validation(format!(
            "survivor catalog id {survivor_id} has a stale or invalid canonical model key"
        )));
    }

    let equivalence_ids = state
        .models
        .iter()
        .filter(|model| {
            model.avionics_manufacturer_identity_id == Some(manufacturer_identity_id)
                && matches!(
                    current_model_identity_relation(state, survivor, model),
                    Some(AvionicsModelIdentityRelation::TypographyExact)
                        | Some(AvionicsModelIdentityRelation::DescriptiveExpansion)
                )
        })
        .map(|model| model.id)
        .collect::<BTreeSet<_>>();
    if equivalence_ids != selected_ids {
        let omitted = equivalence_ids
            .difference(&selected_ids)
            .copied()
            .collect::<Vec<_>>();
        let unrelated = selected_ids
            .difference(&equivalence_ids)
            .copied()
            .collect::<Vec<_>>();
        for model_id in &unrelated {
            let model = by_id.get(model_id).copied().ok_or_else(|| {
                ConsolidationError::Validation(format!(
                    "avionics catalog id {model_id} does not exist"
                ))
            })?;
            if model.avionics_manufacturer_identity_id == Some(manufacturer_identity_id)
                && current_model_identity_relation(state, survivor, model)
                    == Some(AvionicsModelIdentityRelation::MeaningfulVariant)
            {
                return Err(ConsolidationError::Validation(format!(
                    "catalog id {model_id} is a meaningful product variant of survivor {survivor_id}, not a model-equivalent label; variants such as G1000 NXi must remain distinct"
                )));
            }
        }
        return Err(ConsolidationError::Validation(format!(
            "human-reviewed consolidation must name the complete manufacturer-scoped model-equivalence set; omitted={omitted:?}, nonmatching={unrelated:?}"
        )));
    }

    let mut identifiers = BTreeSet::new();
    let mut member_model_keys = BTreeSet::new();
    let mut members = Vec::with_capacity(selected_ids.len());
    for model_id in &selected_ids {
        let model = by_id.get(model_id).copied().ok_or_else(|| {
            ConsolidationError::Validation(format!("avionics catalog id {model_id} does not exist"))
        })?;
        if model.catalog_status != "unreviewed" {
            return Err(ConsolidationError::Validation(format!(
                "human-reviewed model-equivalence consolidation permits only unreviewed rows; catalog id {model_id} is {}",
                model.catalog_status
            )));
        }
        if model.avionics_manufacturer_identity_id != Some(manufacturer_identity_id) {
            return Err(ConsolidationError::Validation(format!(
                "catalog id {model_id} does not share effective manufacturer identity {manufacturer_identity_id}"
            )));
        }
        if normalize_avionics_manufacturer_name(&model.stored_manufacturer_key)
            != normalize_avionics_manufacturer_name(&model.manufacturer)
        {
            return Err(ConsolidationError::Validation(format!(
                "catalog id {model_id} has a stale or non-exact manufacturer identity key"
            )));
        }
        let member_model_key = canonical_model_key(&model.model);
        if member_model_key.is_empty() || model.stored_model_key != member_model_key {
            return Err(ConsolidationError::Validation(format!(
                "catalog id {model_id} has a stale or invalid current canonical model key"
            )));
        }
        member_model_keys.insert(member_model_key.clone());
        if let Some(identifier) = stable_identifier_key(model) {
            identifiers.insert(identifier);
        }
        let role = if *model_id == survivor_id {
            HumanReviewedConsolidationMemberRole::Survivor
        } else {
            HumanReviewedConsolidationMemberRole::Duplicate
        };
        let capabilities = current_model_capabilities(state, *model_id);
        members.push(HumanReviewedConsolidationMemberSnapshot {
            avionics_model_id: *model_id,
            role,
            row_identity_sha256: row_identity_sha256(
                model,
                manufacturer_identity_id,
                &member_model_key,
                &capabilities,
            ),
            avionics_manufacturer_id: model.avionics_manufacturer_id,
            avionics_manufacturer_identity_id: manufacturer_identity_id,
            manufacturer: model.manufacturer.clone(),
            model: model.model.clone(),
            stored_manufacturer_key: model.stored_manufacturer_key.clone(),
            stored_model_key: model.stored_model_key.clone(),
            canonical_model_key: member_model_key,
            catalog_status: model.catalog_status.clone(),
            manufacturer_identifier_kind: model.manufacturer_identifier_kind.clone(),
            manufacturer_identifier: model.manufacturer_identifier.clone(),
            normalized_manufacturer_identifier: model.stored_manufacturer_identifier_key.clone(),
            identity_source_url: model.identity_source_url.clone(),
            identity_source_title: model.identity_source_title.clone(),
            identity_evidence_text: model.identity_evidence_text.clone(),
            identity_evidence_kind: model.identity_evidence_kind.clone(),
            identity_confidence: model.identity_confidence.clone(),
            catalog_reviewed_at: model.catalog_reviewed_at.clone(),
        });
    }
    if identifiers.len() > 1 {
        return Err(ConsolidationError::Validation(format!(
            "human-reviewed model-equivalence set has conflicting nonblank stable identifiers: {identifiers:?}"
        )));
    }
    members.sort_by_key(|member| member.avionics_model_id);

    let (
        provenance_listing_id,
        provenance_pending_review_id,
        provenance_review_payload_sha256,
        provenance_review_aspect_id,
    ) = if let Some(provenance) = &request.provenance {
        let review = state
            .pending_reviews
            .iter()
            .find(|review| review.listing_id == provenance.listing_id)
            .ok_or_else(|| {
                ConsolidationError::Validation(format!(
                    "listing {} has no current pending review for provenance",
                    provenance.listing_id
                ))
            })?;
        if review.review_payload_sha256 != provenance.expected_review_payload_sha256 {
            return Err(ConsolidationError::Conflict(format!(
                "pending review {} changed; expected payload {}, found {}",
                review.id, provenance.expected_review_payload_sha256, review.review_payload_sha256
            )));
        }
        let payload: StoredReviewPayload = serde_json::from_str(&review.review_payload_json)
            .map_err(|error| {
                ConsolidationError::Validation(format!(
                    "pending review {} contains invalid JSON: {error}",
                    review.id
                ))
            })?;
        if !payload
            .aspects
            .iter()
            .any(|aspect| aspect.id.to_string() == provenance.review_aspect_id)
        {
            return Err(ConsolidationError::Validation(format!(
                "pending review {} has no aspect {:?}",
                review.id, provenance.review_aspect_id
            )));
        }
        (
            Some(provenance.listing_id),
            Some(review.id),
            Some(review.review_payload_sha256.clone()),
            Some(provenance.review_aspect_id.clone()),
        )
    } else {
        (None, None, None, None)
    };

    let mut authorization_hasher = Sha256::new();
    for value in [
        "aircost:human-reviewed-avionics-consolidation:v2",
        &request.reviewer_user_id.to_string(),
        &survivor_id.to_string(),
        &manufacturer_identity_id.to_string(),
        survivor_model_key.as_str(),
        request.authoritative_source_url.as_str(),
        request.authoritative_source_title.as_str(),
        request.exact_evidence_text.as_str(),
    ] {
        feed_fingerprint(&mut authorization_hasher, value);
    }
    for value in [
        provenance_listing_id.map(|value| value.to_string()),
        provenance_pending_review_id.map(|value| value.to_string()),
        provenance_review_payload_sha256.clone(),
        provenance_review_aspect_id.clone(),
    ] {
        feed_optional_fingerprint(&mut authorization_hasher, value.as_deref());
    }
    for member in &members {
        feed_fingerprint(
            &mut authorization_hasher,
            &member.avionics_model_id.to_string(),
        );
        feed_fingerprint(
            &mut authorization_hasher,
            match member.role {
                HumanReviewedConsolidationMemberRole::Survivor => "survivor",
                HumanReviewedConsolidationMemberRole::Duplicate => "duplicate",
            },
        );
        feed_fingerprint(&mut authorization_hasher, &member.row_identity_sha256);
    }
    let authorization = HumanReviewedConsolidationAuthorization {
        authorization_sha256: format!("{:x}", authorization_hasher.finalize()),
        persisted: false,
        reviewer_user_id: request.reviewer_user_id,
        survivor_model_id: survivor_id,
        effective_manufacturer_identity_id: manufacturer_identity_id,
        canonical_model_key: survivor_model_key,
        authoritative_source_url: request.authoritative_source_url.clone(),
        authoritative_source_title: request.authoritative_source_title.clone(),
        exact_evidence_text: request.exact_evidence_text.clone(),
        provenance_listing_id,
        provenance_pending_review_id,
        provenance_review_payload_sha256,
        provenance_review_aspect_id,
        members,
    };
    Ok((
        base,
        HumanConsolidationScope {
            effective_manufacturer_identity_id: manufacturer_identity_id,
            member_model_keys,
        },
        authorization,
    ))
}

/// Build the complete transactional change report without mutating the DB.
pub async fn preview_avionics_model_consolidation(
    db: &AppDb,
    request: &AvionicsConsolidationRequest,
) -> ConsolidationResult<AvionicsConsolidationReport> {
    Ok(build_plan(&load_state(db).await?, request)?.report)
}

/// Preview a human-reviewed model-equivalence consolidation without storing
/// its authorization or touching the catalog.
pub async fn preview_human_reviewed_avionics_model_consolidation(
    db: &AppDb,
    request: &HumanReviewedAvionicsConsolidationRequest,
) -> ConsolidationResult<HumanReviewedAvionicsConsolidationReport> {
    let state = load_state(db).await?;
    let (base, scope, authorization) = snapshot_human_authorization(&state, request)?;
    let consolidation = build_plan_with_authority(
        &state,
        &base,
        ConsolidationIdentityAuthority::HumanReviewedModelEquivalence(&scope),
    )?
    .report;
    Ok(HumanReviewedAvionicsConsolidationReport {
        consolidation,
        authorization,
    })
}

fn reference_strength(state: &CatalogState, model_id: i64) -> CanonicalLegacyReferenceStrength {
    let listing_rows = state
        .listing_links
        .iter()
        .filter(|row| {
            row.avionics_model_id == model_id || row.replaces_avionics_model_id == Some(model_id)
        })
        .collect::<Vec<_>>();
    let default_count = state
        .default_links
        .iter()
        .filter(|row| row.avionics_model_id == model_id)
        .count();
    let default_candidate_count = state
        .default_candidate_links
        .iter()
        .filter(|row| row.avionics_model_id == model_id)
        .count();
    let reference_count = state
        .reference_links
        .iter()
        .filter(|row| row.avionics_model_id == model_id)
        .count();
    let suite_count = state
        .suite_links
        .iter()
        .filter(|row| row.suite_model_id == model_id || row.component_model_id == model_id)
        .count();
    let global_reference_count =
        default_count + default_candidate_count + reference_count + suite_count;
    CanonicalLegacyReferenceStrength {
        global_reference_count,
        reviewer_confirmed_listing_reference_count: listing_rows
            .iter()
            .filter(|row| row.source == "listing_review")
            .count(),
        high_confidence_listing_reference_count: listing_rows
            .iter()
            .filter(|row| row.source_confidence.as_deref() == Some("high"))
            .count(),
        total_reference_count: global_reference_count + listing_rows.len(),
        capability_count: state
            .model_types
            .iter()
            .filter(|row| row.avionics_model_id == model_id)
            .count(),
    }
}

fn survivor_preference(
    candidate: &CanonicalLegacyCandidate,
) -> (usize, usize, usize, usize, usize) {
    (
        candidate.strength.global_reference_count,
        candidate
            .strength
            .reviewer_confirmed_listing_reference_count,
        candidate.strength.high_confidence_listing_reference_count,
        candidate.strength.total_reference_count,
        candidate.strength.capability_count,
    )
}

/// Deterministically propose consolidation requests for connected components
/// formed only by current maker-scoped stable-identifier equality, including
/// identifier kind as part of the key.
///
/// Fuzzy similarity is never an edge. Approved and rejected rows are excluded,
/// so executing an applicable request keeps its survivor unreviewed. A
/// transitive component that is not pairwise exact remains in the output with
/// blockers and is also rejected by the transactional planner. Mechanically
/// equal maker/model labels without identifier proof remain in the audit for
/// grounded review and are never proposed automatically.
pub async fn plan_canonical_legacy_duplicates(
    db: &AppDb,
) -> ConsolidationResult<Vec<CanonicalLegacyConsolidationPlan>> {
    let state = load_state(db).await?;
    let mut models = state
        .models
        .iter()
        .filter(|model| model.catalog_status == "unreviewed")
        .collect::<Vec<_>>();
    models.sort_by_key(|model| model.id);

    let mut visited = vec![false; models.len()];
    let mut plans = Vec::new();
    for start in 0..models.len() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut component_indexes = vec![start];
        let mut cursor = 0;
        while cursor < component_indexes.len() {
            let current = component_indexes[cursor];
            for candidate in 0..models.len() {
                let automatic_match = matches!(
                    identity_match(&state, models[current], models[candidate]),
                    Some(IdentityMatchBasis::StableManufacturerIdentifier)
                        | Some(IdentityMatchBasis::Both)
                );
                if !visited[candidate] && automatic_match {
                    visited[candidate] = true;
                    component_indexes.push(candidate);
                }
            }
            cursor += 1;
        }
        if component_indexes.len() < 2 {
            continue;
        }
        let mut component = component_indexes
            .into_iter()
            .map(|index| models[index])
            .collect::<Vec<_>>();
        component.sort_by_key(|model| model.id);
        let manufacturer_key = normalize_avionics_manufacturer_name(&component[0].manufacturer);
        let model_keys = component
            .iter()
            .map(|model| normalize_avionics_model_name(&model.model))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let identifier_keys = component
            .iter()
            .filter_map(|model| stable_identifier_key(model))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let blockers = exact_identity_graph_blockers(&state, &component);
        let mut candidates = component
            .iter()
            .map(|model| CanonicalLegacyCandidate {
                model_id: model.id,
                strength: reference_strength(&state, model.id),
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            survivor_preference(right)
                .cmp(&survivor_preference(left))
                .then_with(|| left.model_id.cmp(&right.model_id))
        });
        let survivor_id = candidates[0].model_id;
        plans.push(CanonicalLegacyConsolidationPlan {
            manufacturer_key,
            model_keys,
            identifier_keys,
            request: AvionicsConsolidationRequest {
                survivor_id,
                duplicate_ids: candidates
                    .iter()
                    .skip(1)
                    .map(|candidate| candidate.model_id)
                    .collect(),
            },
            candidates,
            blockers,
        });
    }
    plans.sort_by(|left, right| {
        left.manufacturer_key
            .cmp(&right.manufacturer_key)
            .then_with(|| {
                left.candidates
                    .iter()
                    .map(|candidate| candidate.model_id)
                    .min()
                    .cmp(
                        &right
                            .candidates
                            .iter()
                            .map(|candidate| candidate.model_id)
                            .min(),
                    )
            })
    });
    Ok(plans)
}

fn plan_conflict(plan: &ConsolidationPlan) -> ConsolidationError {
    ConsolidationError::Conflict(format!(
        "avionics catalog consolidation is blocked: {}",
        plan.report.blockers.join("; ")
    ))
}

/// Shared locked mutation path. Human-reviewed model-equivalence consolidation
/// is kept out of the automatic API by accepting its authorization only from
/// the explicit public wrapper below.
async fn consolidate_avionics_models_internal(
    db: &AppDb,
    request: &AvionicsConsolidationRequest,
    human_review: Option<&HumanReviewedAvionicsConsolidationRequest>,
) -> ConsolidationResult<(
    AvionicsConsolidationReport,
    Option<HumanReviewedConsolidationAuthorization>,
)> {
    // Validate cheap request-shape errors before opening a write transaction.
    normalized_request(request)?;
    if let Some(human_review) = human_review {
        let base = human_request_base(human_review)?;
        if base != *request {
            return Err(ConsolidationError::Validation(
                "human-review request IDs do not match the consolidation request".to_string(),
            ));
        }
    }
    let lock_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = ?",
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            "LOCK TABLE avionics_models, avionics_manufacturers, avionics_manufacturer_canonical_keys, avionics_approved_product_identities, avionics_model_types, aircraft_sale_listings, aircraft_sale_listing_avionics, aircraft_model_variant_default_avionics, aircraft_model_variant_default_avionics_candidates, aircraft_reference_configuration_versions, aircraft_reference_avionics, avionics_suite_components, aircraft_sale_listing_pending_reviews, avionics_catalog_consolidation_guard, avionics_catalog_human_consolidation_authorizations, avionics_catalog_human_consolidation_members, avionics_catalog_human_consolidation_guard, avionics_catalog_human_consolidation_claim IN SHARE ROW EXCLUSIVE MODE",
        ),
    };
    let insert_guard = db.sql(
        "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id, survivor_model_id) VALUES (?, ?)",
    );
    let insert_human_authorization = db.sql(
        r#"INSERT INTO avionics_catalog_human_consolidation_authorizations (
             authorization_sha256, reviewer_user_id, survivor_model_id_snapshot,
             effective_manufacturer_identity_id_snapshot,
             canonical_model_key_snapshot, expected_member_count,
             authoritative_source_url, authoritative_source_title,
             exact_evidence_text, provenance_listing_id_snapshot,
             provenance_pending_review_id_snapshot,
             provenance_review_payload_sha256, provenance_review_aspect_id
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    );
    let insert_human_member = db.sql(
        r#"INSERT INTO avionics_catalog_human_consolidation_members (
             authorization_sha256, avionics_model_id_snapshot, member_role,
             row_identity_sha256, avionics_manufacturer_id_snapshot,
             effective_manufacturer_identity_id_snapshot,
             manufacturer_name_snapshot, stored_manufacturer_key_snapshot,
             model_name_snapshot, stored_model_key_snapshot,
             canonical_model_key_snapshot, catalog_status_snapshot,
             manufacturer_identifier_kind_snapshot,
             manufacturer_identifier_snapshot,
             normalized_manufacturer_identifier_snapshot,
             identity_source_url_snapshot, identity_source_title_snapshot,
             identity_evidence_text_snapshot, identity_evidence_kind_snapshot,
             identity_confidence_snapshot, catalog_reviewed_at_snapshot
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    );
    let insert_human_guard = db.sql(
        r#"INSERT INTO avionics_catalog_human_consolidation_guard (
             duplicate_model_id, survivor_model_id, authorization_sha256
           ) VALUES (?, ?, ?)"#,
    );
    let insert_human_claim = db.sql(
        r#"INSERT INTO avionics_catalog_human_consolidation_claim (
             authorization_sha256, survivor_model_id
           ) VALUES (?, ?)"#,
    );
    let delete_human_claim = db.sql(
        "DELETE FROM avionics_catalog_human_consolidation_claim WHERE authorization_sha256 = ?",
    );
    let count_all_guards = db.sql(
        r#"SELECT
             (SELECT COUNT(*) FROM avionics_catalog_consolidation_guard)
             + (SELECT COUNT(*) FROM avionics_catalog_human_consolidation_guard)
             + (SELECT COUNT(*) FROM avionics_catalog_human_consolidation_claim)"#,
    );
    let insert_type = db.sql(
        "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?) ON CONFLICT (avionics_model_id, avionics_type_id) DO NOTHING",
    );
    let delete_listing = db.sql("DELETE FROM aircraft_sale_listing_avionics WHERE id = ?");
    let update_listing = db.sql(
        r#"UPDATE aircraft_sale_listing_avionics
           SET avionics_model_id = ?, quantity = ?, source = ?, source_notes = ?,
               configuration_action = ?, replaces_avionics_model_id = ?,
               source_confidence = ?, updated_at = CURRENT_TIMESTAMP
           WHERE id = ?"#,
    );
    let delete_default = db.sql("DELETE FROM aircraft_model_variant_default_avionics WHERE id = ?");
    let update_default = db.sql(
        r#"UPDATE aircraft_model_variant_default_avionics
           SET avionics_model_id = ?, quantity = ?, source_notes = ?,
               source_confidence = ?, updated_at = CURRENT_TIMESTAMP
           WHERE id = ?"#,
    );
    let delete_default_candidate =
        db.sql("DELETE FROM aircraft_model_variant_default_avionics_candidates WHERE id = ?");
    let insert_default_candidate = db.sql(
        r#"INSERT INTO aircraft_model_variant_default_avionics_candidates (
             id, quarantined_default_avionics_id, aircraft_model_variant_id,
             model_year, avionics_model_id, quantity, source_url, source_title,
             source_notes, source_confidence, pending_reason,
             quarantined_created_at, quarantined_updated_at, created_at
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    );
    let delete_reference = db.sql("DELETE FROM aircraft_reference_avionics WHERE id = ?");
    let update_reference =
        db.sql("UPDATE aircraft_reference_avionics SET avionics_model_id = ? WHERE id = ?");
    let delete_suite = db.sql(
        "DELETE FROM avionics_suite_components WHERE suite_model_id = ? AND component_model_id = ?",
    );
    let update_suite = db.sql(
        r#"UPDATE avionics_suite_components
           SET suite_model_id = ?, component_model_id = ?, quantity = ?
           WHERE suite_model_id = ? AND component_model_id = ?"#,
    );
    let update_review = db.sql(
        r#"UPDATE aircraft_sale_listing_pending_reviews
           SET extraction_sha256 = ?, pending_aspect_count = ?,
               review_payload_json = ?, review_payload_sha256 = ?,
               updated_at = CURRENT_TIMESTAMP
           WHERE id = ?"#,
    );
    let remap_alias_candidate = db.sql(
        r#"UPDATE avionics_manufacturer_alias_candidates
           SET matched_avionics_model_id = ?
           WHERE matched_avionics_model_id = ?"#,
    );
    let reset_survivor_value = db.sql(
        r#"UPDATE avionics_models
           SET introduced_year = NULL,
               discontinued_year = NULL,
               estimated_unit_value_usd = NULL,
               value_basis = 'unreviewed',
               replacement_cost_usd = NULL,
               value_reference_year = NULL,
               value_source = NULL,
               valuation_scope = 'unit',
               updated_at = CURRENT_TIMESTAMP
           WHERE id = ? AND catalog_status = 'unreviewed'"#,
    );
    let delete_model = db.sql("DELETE FROM avionics_models WHERE id = ?");
    let count_duplicate = db.sql("SELECT COUNT(*) FROM avionics_models WHERE id = ?");
    let invalid_ready_links = db.sql(
        r#"SELECT COUNT(*)
           FROM aircraft_sale_listing_avionics link
           JOIN aircraft_sale_listings listing
             ON listing.id = link.aircraft_sale_listing_id
           JOIN avionics_models installed ON installed.id = link.avionics_model_id
           LEFT JOIN avionics_models replaced ON replaced.id = link.replaces_avionics_model_id
           WHERE (listing.ingestion_state = 'ready' OR listing.is_verified = TRUE)
             AND (
               installed.catalog_status <> 'approved'
               OR (link.replaces_avionics_model_id IS NOT NULL
                   AND (replaced.id IS NULL OR replaced.catalog_status <> 'approved'))
             )"#,
    );

    macro_rules! apply_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            if matches!(db.backend(), DatabaseBackend::Sqlite(_)) {
                sqlx::query(&lock_sql)
                    .bind(request.survivor_id)
                    .execute(&mut *transaction)
                    .await?;
            } else {
                sqlx::query(&lock_sql)
                    .execute(&mut *transaction)
                    .await?;
            }
            let state = load_catalog_state!(db, &mut *transaction);
            let (mut plan, mut authorization) = if let Some(human_review) = human_review {
                // Re-snapshot the exact pending-review digest and catalog rows
                // under the same lock used for mutation. This is the apply-side
                // TOCTOU boundary; a preview authorization is never trusted.
                let (locked_base, scope, authorization) =
                    snapshot_human_authorization(&state, human_review)?;
                if locked_base != *request {
                    return Err(ConsolidationError::Conflict(
                        "human-review request IDs changed before apply".to_string(),
                    ));
                }
                let expected_authorization = human_review
                    .expected_authorization_sha256
                    .as_deref()
                    .ok_or_else(|| {
                        ConsolidationError::Validation(
                            "human-reviewed consolidation apply requires the exact preview authorization digest"
                                .to_string(),
                        )
                    })?;
                if authorization.authorization_sha256 != expected_authorization {
                    return Err(ConsolidationError::Conflict(format!(
                        "human consolidation preview is stale; expected authorization {expected_authorization}, locked snapshot is {}",
                        authorization.authorization_sha256
                    )));
                }
                let expected_catalog_revision = human_review
                    .expected_catalog_revision_sha256
                    .as_deref()
                    .ok_or_else(|| {
                        ConsolidationError::Validation(
                            "human-reviewed consolidation apply requires the accepted approved-catalog revision"
                                .to_string(),
                        )
                    })?;
                let locked_catalog_revision = approved_catalog_revision_from_state(&state);
                if locked_catalog_revision != expected_catalog_revision {
                    return Err(ConsolidationError::Conflict(format!(
                        "approved avionics catalog changed before consolidation; expected {expected_catalog_revision}, locked revision is {locked_catalog_revision}"
                    )));
                }
                (
                    build_plan_with_authority(
                        &state,
                        request,
                        ConsolidationIdentityAuthority::HumanReviewedModelEquivalence(&scope),
                    )?,
                    Some(authorization),
                )
            } else {
                (build_plan(&state, request)?, None)
            };
            if !plan.report.can_apply {
                return Err(plan_conflict(&plan));
            }
            let preexisting_guard_count: i64 = sqlx::query_scalar(&count_all_guards)
                .fetch_one(&mut *transaction)
                .await?;
            if preexisting_guard_count != 0 {
                return Err(ConsolidationError::Conflict(format!(
                    "refusing consolidation while {preexisting_guard_count} preexisting catalog authorization rows are present"
                )));
            }
            if let Some(authorization) = authorization.as_ref() {
                let inserted = sqlx::query(&insert_human_authorization)
                    .bind(authorization.authorization_sha256.as_str())
                    .bind(authorization.reviewer_user_id)
                    .bind(authorization.survivor_model_id)
                    .bind(authorization.effective_manufacturer_identity_id)
                    .bind(authorization.canonical_model_key.as_str())
                    .bind(i64::try_from(authorization.members.len()).map_err(|_| {
                        ConsolidationError::Validation(
                            "human consolidation member count exceeds database range".to_string(),
                        )
                    })?)
                    .bind(authorization.authoritative_source_url.as_str())
                    .bind(authorization.authoritative_source_title.as_str())
                    .bind(authorization.exact_evidence_text.as_str())
                    .bind(authorization.provenance_listing_id)
                    .bind(authorization.provenance_pending_review_id)
                    .bind(authorization.provenance_review_payload_sha256.as_deref())
                    .bind(authorization.provenance_review_aspect_id.as_deref())
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if inserted != 1 {
                    return Err(ConsolidationError::Conflict(format!(
                        "human consolidation authorization {} could not be persisted",
                        authorization.authorization_sha256
                    )));
                }
                for member in &authorization.members {
                    let role = match member.role {
                        HumanReviewedConsolidationMemberRole::Survivor => "survivor",
                        HumanReviewedConsolidationMemberRole::Duplicate => "duplicate",
                    };
                    let inserted = sqlx::query(&insert_human_member)
                        .bind(authorization.authorization_sha256.as_str())
                        .bind(member.avionics_model_id)
                        .bind(role)
                        .bind(member.row_identity_sha256.as_str())
                        .bind(member.avionics_manufacturer_id)
                        .bind(member.avionics_manufacturer_identity_id)
                        .bind(member.manufacturer.as_str())
                        .bind(member.stored_manufacturer_key.as_str())
                        .bind(member.model.as_str())
                        .bind(member.stored_model_key.as_str())
                        .bind(member.canonical_model_key.as_str())
                        .bind(member.catalog_status.as_str())
                        .bind(member.manufacturer_identifier_kind.as_deref())
                        .bind(member.manufacturer_identifier.as_deref())
                        .bind(member.normalized_manufacturer_identifier.as_deref())
                        .bind(member.identity_source_url.as_deref())
                        .bind(member.identity_source_title.as_deref())
                        .bind(member.identity_evidence_text.as_deref())
                        .bind(member.identity_evidence_kind.as_str())
                        .bind(member.identity_confidence.as_deref())
                        .bind(member.catalog_reviewed_at.as_deref())
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if inserted != 1 {
                        return Err(ConsolidationError::Conflict(format!(
                            "human consolidation member snapshot {} could not be persisted",
                            member.avionics_model_id
                        )));
                    }
                }
                for duplicate_id in &plan.duplicate_ids {
                    let inserted = sqlx::query(&insert_human_guard)
                        .bind(*duplicate_id)
                        .bind(request.survivor_id)
                        .bind(authorization.authorization_sha256.as_str())
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if inserted != 1 {
                        return Err(ConsolidationError::Conflict(format!(
                            "human catalog authorization pair {duplicate_id}->{} could not be installed",
                            request.survivor_id
                        )));
                    }
                }
                let inserted = sqlx::query(&insert_human_claim)
                    .bind(authorization.authorization_sha256.as_str())
                    .bind(request.survivor_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if inserted != 1 {
                    return Err(ConsolidationError::Conflict(format!(
                        "complete human consolidation claim {} could not be activated",
                        authorization.authorization_sha256
                    )));
                }
            } else {
                for duplicate_id in &plan.duplicate_ids {
                    let inserted = sqlx::query(&insert_guard)
                        .bind(*duplicate_id)
                        .bind(request.survivor_id)
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if inserted != 1 {
                        return Err(ConsolidationError::Conflict(format!(
                            "catalog authorization pair {duplicate_id}->{} could not be installed",
                            request.survivor_id
                        )));
                    }
                }
            }

            for type_id in &plan.type_ids_to_add {
                sqlx::query(&insert_type)
                    .bind(request.survivor_id)
                    .bind(*type_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            for link in &plan.listing_links {
                for deleted_id in &link.deleted_ids {
                    sqlx::query(&delete_listing)
                        .bind(*deleted_id)
                        .execute(&mut *transaction)
                        .await?;
                }
                let changed = sqlx::query(&update_listing)
                    .bind(link.keeper.avionics_model_id)
                    .bind(link.keeper.quantity)
                    .bind(link.keeper.source.as_str())
                    .bind(link.keeper.source_notes.as_deref())
                    .bind(link.keeper.configuration_action.as_str())
                    .bind(link.keeper.replaces_avionics_model_id)
                    .bind(link.keeper.source_confidence.as_deref())
                    .bind(link.keeper.id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if changed != 1 {
                    return Err(ConsolidationError::Conflict(format!(
                        "listing avionics link {} changed during consolidation",
                        link.keeper.id
                    )));
                }
            }
            for link in &plan.default_links {
                for deleted_id in &link.deleted_ids {
                    sqlx::query(&delete_default)
                        .bind(*deleted_id)
                        .execute(&mut *transaction)
                        .await?;
                }
                let changed = sqlx::query(&update_default)
                    .bind(link.keeper.avionics_model_id)
                    .bind(link.keeper.quantity)
                    .bind(link.keeper.source_notes.as_str())
                    .bind(link.keeper.source_confidence.as_str())
                    .bind(link.keeper.id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if changed != 1 {
                    return Err(ConsolidationError::Conflict(format!(
                        "default avionics link {} changed during consolidation",
                        link.keeper.id
                    )));
                }
            }
            for link in &plan.default_candidate_links {
                for deleted_id in &link.deleted_ids {
                    let deleted = sqlx::query(&delete_default_candidate)
                        .bind(*deleted_id)
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if deleted != 1 {
                        return Err(ConsolidationError::Conflict(format!(
                            "pending default avionics candidate {deleted_id} changed during consolidation"
                        )));
                    }
                }
                let inserted = sqlx::query(&insert_default_candidate)
                    .bind(link.keeper.id)
                    .bind(link.keeper.quarantined_default_avionics_id)
                    .bind(link.keeper.aircraft_model_variant_id)
                    .bind(link.keeper.model_year)
                    .bind(link.keeper.avionics_model_id)
                    .bind(link.keeper.quantity)
                    .bind(link.keeper.source_url.as_str())
                    .bind(link.keeper.source_title.as_str())
                    .bind(link.keeper.source_notes.as_str())
                    .bind(link.keeper.source_confidence.as_str())
                    .bind(link.keeper.pending_reason.as_str())
                    .bind(link.keeper.quarantined_created_at.as_deref())
                    .bind(link.keeper.quarantined_updated_at.as_deref())
                    .bind(link.keeper.created_at.as_str())
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if inserted != 1 {
                    return Err(ConsolidationError::Conflict(format!(
                        "pending default avionics candidate {} could not be remapped",
                        link.keeper.id
                    )));
                }
            }
            for link in &plan.reference_links {
                for deleted_id in &link.deleted_ids {
                    sqlx::query(&delete_reference)
                        .bind(*deleted_id)
                        .execute(&mut *transaction)
                        .await?;
                }
                if link.remap_keeper {
                    let changed = sqlx::query(&update_reference)
                        .bind(link.keeper.avionics_model_id)
                        .bind(link.keeper.id)
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if changed != 1 {
                        return Err(ConsolidationError::Conflict(format!(
                            "reference avionics link {} changed during consolidation",
                            link.keeper.id
                        )));
                    }
                }
            }
            for (suite_id, component_id) in &plan.suite_original_rows_to_delete {
                sqlx::query(&delete_suite)
                    .bind(*suite_id)
                    .bind(*component_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            for link in &plan.suite_links_to_upsert {
                let changed = sqlx::query(&update_suite)
                    .bind(link.suite_model_id)
                    .bind(link.component_model_id)
                    .bind(link.quantity)
                    .bind(link.original_suite_model_id)
                    .bind(link.original_component_model_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if changed != 1 {
                    return Err(ConsolidationError::Conflict(format!(
                        "suite membership ({}, {}) changed during consolidation",
                        link.original_suite_model_id, link.original_component_model_id
                    )));
                }
            }
            for review in &plan.pending_reviews {
                let changed = sqlx::query(&update_review)
                    .bind(review.extraction_sha256.as_str())
                    .bind(review.pending_aspect_count)
                    .bind(review.review_payload_json.as_str())
                    .bind(review.review_payload_sha256.as_str())
                    .bind(review.id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if changed != 1 {
                    return Err(ConsolidationError::Conflict(format!(
                        "pending review {} changed during consolidation",
                        review.id
                    )));
                }
            }
            // Preserve immutable alias-review history by moving only its
            // catalog pointer through the exact active guard pair. The DB
            // trigger rejects every other candidate-field mutation.
            for duplicate_id in &plan.duplicate_ids {
                sqlx::query(&remap_alias_candidate)
                    .bind(request.survivor_id)
                    .bind(*duplicate_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            if plan.report.survivor.catalog_status == "unreviewed" {
                sqlx::query(&reset_survivor_value)
                    .bind(request.survivor_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            for duplicate_id in &plan.duplicate_ids {
                let deleted = sqlx::query(&delete_model)
                    .bind(*duplicate_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if deleted != 1 {
                    return Err(ConsolidationError::Conflict(format!(
                        "duplicate avionics model {duplicate_id} changed during consolidation"
                    )));
                }
            }
            if let Some(authorization) = authorization.as_ref() {
                let deleted = sqlx::query(&delete_human_claim)
                    .bind(authorization.authorization_sha256.as_str())
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if deleted != 1 {
                    return Err(ConsolidationError::Conflict(format!(
                        "human consolidation claim {} was not released",
                        authorization.authorization_sha256
                    )));
                }
            }
            let guard_count: i64 = sqlx::query_scalar(&count_all_guards)
                .fetch_one(&mut *transaction)
                .await?;
            if guard_count != 0 {
                return Err(ConsolidationError::Conflict(
                    "catalog consolidation guard or claim remained active at transaction end"
                        .to_string(),
                ));
            }
            for duplicate_id in &plan.duplicate_ids {
                let remaining: i64 = sqlx::query_scalar(&count_duplicate)
                    .bind(*duplicate_id)
                    .fetch_one(&mut *transaction)
                    .await?;
                if remaining != 0 {
                    return Err(ConsolidationError::Conflict(format!(
                        "duplicate avionics model {duplicate_id} survived consolidation"
                    )));
                }
            }
            let invalid_ready: i64 = sqlx::query_scalar(&invalid_ready_links)
                .fetch_one(&mut *transaction)
                .await?;
            if invalid_ready != 0 {
                return Err(ConsolidationError::Conflict(format!(
                    "consolidation would leave {invalid_ready} unapproved avionics associations on ready or verified listings"
                )));
            }
            let final_state = load_catalog_state!(db, &mut *transaction);
            let final_survivor = final_state
                .models
                .iter()
                .find(|model| model.id == request.survivor_id)
                .ok_or_else(|| {
                    ConsolidationError::Conflict(
                        "consolidation survivor disappeared before commit".to_string(),
                    )
                })?;
            if final_survivor.catalog_status != plan.report.survivor.catalog_status {
                return Err(ConsolidationError::Conflict(format!(
                    "consolidation changed survivor {} catalog status from {} to {}",
                    final_survivor.id,
                    plan.report.survivor.catalog_status,
                    final_survivor.catalog_status
                )));
            }
            if let Some(collision) = final_state.models.iter().find(|model| {
                model.id != final_survivor.id
                    && identity_match(&final_state, final_survivor, model).is_some()
            }) {
                return Err(ConsolidationError::Conflict(format!(
                    "post-consolidation identity audit found catalog id {} still duplicates survivor {}",
                    collision.id, final_survivor.id
                )));
            }
            transaction.commit().await?;
            plan.report.dry_run = false;
            plan.report.applied = true;
            if let Some(authorization) = authorization.as_mut() {
                authorization.persisted = true;
            }
            Ok::<
                (
                    AvionicsConsolidationReport,
                    Option<HumanReviewedConsolidationAuthorization>,
                ),
                ConsolidationError,
            >((plan.report, authorization))
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => apply_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => apply_in_transaction!(pool),
    }
}

/// Atomically consolidate explicitly named stable-identifier duplicates into
/// one survivor.
///
/// This automatic path remains strict: every pair must have the same nonblank
/// stable manufacturer identifier in one evidence-backed manufacturer scope.
pub async fn consolidate_avionics_models(
    db: &AppDb,
    request: &AvionicsConsolidationRequest,
) -> ConsolidationResult<AvionicsConsolidationReport> {
    let (report, authorization) = consolidate_avionics_models_internal(db, request, None).await?;
    debug_assert!(authorization.is_none());
    Ok(report)
}

/// Apply a separately authorized, evidence-backed human decision for one
/// complete manufacturer-scoped model-equivalence set.
pub async fn consolidate_avionics_models_with_human_review(
    db: &AppDb,
    request: &HumanReviewedAvionicsConsolidationRequest,
) -> ConsolidationResult<HumanReviewedAvionicsConsolidationReport> {
    let base = human_request_base(request)?;
    let (consolidation, authorization) =
        consolidate_avionics_models_internal(db, &base, Some(request)).await?;
    let authorization = authorization.ok_or_else(|| {
        ConsolidationError::Conflict(
            "human-reviewed consolidation completed without an authorization audit".to_string(),
        )
    })?;
    Ok(HumanReviewedAvionicsConsolidationReport {
        consolidation,
        authorization,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use sqlx::SqlitePool;

    use super::*;
    use crate::avionics::manufacturer::{
        approve_manufacturer_alias_candidate, ensure_manufacturer_identity,
        finalize_approved_manufacturer_identity_merge, stage_manufacturer_alias_candidate,
        AliasCandidateBasis, ApproveManufacturerAliasCandidateRequest,
        ManufacturerIdentityEvidence, StageManufacturerAliasCandidateRequest,
    };
    use crate::listing::review::{
        stage_pending_review, ListingAssociationRole, PendingReviewAspect,
    };
    use crate::normalize::normalize_name;

    async fn test_db() -> AppDb {
        AppDb::connect("sqlite::memory:").await.unwrap()
    }

    fn pool(db: &AppDb) -> &SqlitePool {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test database must be SQLite");
        };
        pool
    }

    fn listing_link(
        id: i64,
        listing_id: i64,
        subject_id: i64,
        action: &str,
        displaced_id: Option<i64>,
    ) -> ListingLinkRow {
        ListingLinkRow {
            id,
            aircraft_sale_listing_id: listing_id,
            avionics_model_id: subject_id,
            quantity: 1,
            source: "listing".to_string(),
            source_notes: None,
            configuration_action: action.to_string(),
            replaces_avionics_model_id: displaced_id,
            source_confidence: Some("high".to_string()),
            listing_ingestion_state: "incomplete".to_string(),
            listing_is_verified: false,
        }
    }

    fn identity_test_model(
        id: i64,
        manufacturer_identity_id: Option<i64>,
        model: &str,
    ) -> ModelRow {
        ModelRow {
            id,
            avionics_manufacturer_id: 1,
            avionics_manufacturer_identity_id: manufacturer_identity_id,
            manufacturer: "Garmin".to_string(),
            stored_manufacturer_key: "garmin".to_string(),
            model: model.to_string(),
            stored_model_key: normalize_avionics_model_name(model),
            catalog_status: "unreviewed".to_string(),
            manufacturer_identifier_kind: Some("manufacturer_part_number".to_string()),
            manufacturer_identifier: Some("010-01750-01".to_string()),
            stored_manufacturer_identifier_key: Some("0100175001".to_string()),
            identity_source_url: None,
            identity_source_title: None,
            identity_evidence_text: None,
            identity_evidence_kind: "unreviewed".to_string(),
            identity_confidence: None,
            catalog_reviewed_at: None,
        }
    }

    fn identity_test_state(models: Vec<ModelRow>) -> CatalogState {
        CatalogState {
            models,
            model_types: Vec::new(),
            listing_links: Vec::new(),
            default_links: Vec::new(),
            default_candidate_links: Vec::new(),
            reference_links: Vec::new(),
            suite_links: Vec::new(),
            pending_reviews: Vec::new(),
            approved_manufacturer_alias_scopes: Vec::new(),
            users: Vec::new(),
        }
    }

    #[test]
    fn model_equivalence_consolidation_replaces_only_the_obsolete_collision_instruction() {
        let stale_reason = format!(
            "{LEGACY_DUPLICATE_CONSOLIDATION_REASON_PREFIX} ([4, 239]); \
             {LEGACY_DUPLICATE_CONSOLIDATION_REASON_SUFFIX}"
        );
        let mut remapped = PendingReviewAspect::avionics(
            "gia-63w",
            "avionics",
            "GIA 63W",
            "Garmin GIA 63W",
            &stale_reason,
            1,
            "installed",
            None,
            None,
        );
        rewrite_consolidated_pending_identity_reason(&mut remapped, true);
        assert_eq!(remapped.reason, CONSOLIDATED_PENDING_IDENTITY_REASON);
        assert!(!remapped.reason.contains("[4, 239]"));
        assert!(!remapped.reason.contains("explicitly consolidate"));

        let mut unaffected = PendingReviewAspect::avionics(
            "gma-1347",
            "avionics",
            "GMA 1347",
            "Garmin GMA 1347",
            &stale_reason,
            1,
            "installed",
            None,
            None,
        );
        rewrite_consolidated_pending_identity_reason(&mut unaffected, false);
        assert_eq!(unaffected.reason, stale_reason);

        let mut independent_reason = PendingReviewAspect::avionics(
            "gia-63w",
            "avionics",
            "GIA 63W",
            "Garmin GIA 63W",
            "listing_link_confidence_not_high",
            1,
            "installed",
            None,
            None,
        );
        rewrite_consolidated_pending_identity_reason(&mut independent_reason, true);
        assert_eq!(
            independent_reason.reason,
            "listing_link_confidence_not_high"
        );
    }

    #[test]
    fn shared_raw_manufacturer_row_does_not_authorize_consolidation_without_identity_evidence() {
        let left = identity_test_model(1, None, "GTX 345");
        let right = identity_test_model(2, None, "GTX 345 Remote");
        let state = identity_test_state(vec![left.clone(), right.clone()]);

        assert!(!manufacturer_scope_is_authorized(&state, &left, &right));
        assert_eq!(identity_match(&state, &left, &right), None);

        let evidenced_left = identity_test_model(1, Some(11), "GTX 345");
        let evidenced_right = identity_test_model(2, Some(11), "GTX 345 Remote");
        let evidenced_state =
            identity_test_state(vec![evidenced_left.clone(), evidenced_right.clone()]);
        assert_eq!(
            identity_match(&evidenced_state, &evidenced_left, &evidenced_right),
            Some(IdentityMatchBasis::StableManufacturerIdentifier)
        );
    }

    #[test]
    fn listing_consolidation_preserves_pure_removal_semantics() {
        let rows = vec![listing_link(1, 10, 42, "removes", Some(42))];
        let mut blockers = Vec::new();

        let (plans, _) = plan_listing_links(&rows, 7, &BTreeSet::from([42]), &mut blockers);

        assert!(blockers.is_empty(), "{blockers:?}");
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].keeper.avionics_model_id, 7);
        assert_eq!(plans[0].keeper.configuration_action, "removes");
        assert_eq!(plans[0].keeper.replaces_avionics_model_id, Some(7));
    }

    #[test]
    fn listing_consolidation_validates_full_post_remap_action_graph() {
        let rows = vec![
            listing_link(1, 10, 7, "installed", None),
            listing_link(2, 10, 99, "replaces", Some(42)),
        ];
        let mut blockers = Vec::new();

        let _ = plan_listing_links(&rows, 7, &BTreeSet::from([42]), &mut blockers);

        assert!(blockers.iter().any(|blocker| {
            blocker.contains("invalid avionics action graph")
                && blocker.contains("both installed and displaced")
        }));
    }

    #[test]
    fn listing_consolidation_rejects_post_remap_duplicate_displacement_targets() {
        let rows = vec![
            listing_link(1, 10, 98, "replaces", Some(7)),
            listing_link(2, 10, 99, "replaces", Some(42)),
        ];
        let mut blockers = Vec::new();

        let _ = plan_listing_links(&rows, 7, &BTreeSet::from([42]), &mut blockers);

        assert!(blockers.iter().any(|blocker| {
            blocker.contains("invalid avionics action graph")
                && blocker.contains("displaced more than once")
        }));
    }

    async fn insert_legacy_model(
        db: &AppDb,
        manufacturer: &str,
        stored_manufacturer_key: &str,
        model: &str,
        capabilities: &[&str],
    ) -> (i64, i64) {
        let pool = pool(db);
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(manufacturer)
        .bind(stored_manufacturer_key)
        .execute(pool)
        .await
        .unwrap();
        let manufacturer_id: i64 =
            sqlx::query_scalar("SELECT id FROM avionics_manufacturers WHERE normalized_name = ?")
                .bind(stored_manufacturer_key)
                .fetch_one(pool)
                .await
                .unwrap();
        let model_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO avionics_models (
                 avionics_manufacturer_id, name, normalized_name,
                 estimated_unit_value_usd, replacement_cost_usd,
                 value_reference_year, value_source, valuation_scope
               ) VALUES (?, ?, ?, 1234, 5678, 2024, 'legacy import', 'integrated_suite')
               RETURNING id"#,
        )
        .bind(manufacturer_id)
        .bind(model)
        .bind(normalize_avionics_model_name(model))
        .fetch_one(pool)
        .await
        .unwrap();
        for capability in capabilities {
            let key = normalize_name(capability);
            sqlx::query(
                "INSERT INTO avionics_types (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
            )
            .bind(capability)
            .bind(&key)
            .execute(pool)
            .await
            .unwrap();
            let type_id: i64 =
                sqlx::query_scalar("SELECT id FROM avionics_types WHERE normalized_name = ?")
                    .bind(&key)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            sqlx::query(
                "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
            )
            .bind(model_id)
            .bind(type_id)
            .execute(pool)
            .await
            .unwrap();
        }
        (manufacturer_id, model_id)
    }

    async fn insert_listing(db: &AppDb) -> i64 {
        let pool = pool(db);
        let user_id: i64 = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES ('Cessna', 'cessna') ON CONFLICT (normalized_name) DO NOTHING",
        )
        .execute(pool)
        .await
        .unwrap();
        let manufacturer_id: i64 = sqlx::query_scalar(
            "SELECT id FROM aircraft_manufacturers WHERE normalized_name = 'cessna'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name) VALUES (?, '182T', '182t') ON CONFLICT (aircraft_manufacturer_id, normalized_name) DO NOTHING",
        )
        .bind(manufacturer_id)
        .execute(pool)
        .await
        .unwrap();
        let model_id: i64 = sqlx::query_scalar(
            "SELECT id FROM aircraft_models WHERE aircraft_manufacturer_id = ? AND normalized_name = '182t'",
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name) VALUES (?, 'Standard', 'standard') ON CONFLICT (aircraft_model_id, normalized_name) DO NOTHING",
        )
        .bind(model_id)
        .execute(pool)
        .await
        .unwrap();
        let pending_variant_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query_scalar(
            r#"INSERT INTO aircraft_sale_listings (
                 aircraft_model_variant_id, created_by_user_id, source_url,
                 model_year, asking_price_usd, airframe_hours
               ) VALUES (?, ?, 'https://listing.example/legacy', 2004, 300000, 1000)
               RETURNING id"#,
        )
        .bind(pending_variant_id)
        .bind(user_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn set_legacy_identifier(
        db: &AppDb,
        model_id: i64,
        identifier: &str,
        stored_identifier_key: &str,
    ) {
        set_legacy_identifier_kind(
            db,
            model_id,
            "manufacturer_part_number",
            identifier,
            stored_identifier_key,
        )
        .await;
    }

    async fn set_legacy_identifier_kind(
        db: &AppDb,
        model_id: i64,
        identifier_kind: &str,
        identifier: &str,
        stored_identifier_key: &str,
    ) {
        sqlx::query(
            r#"UPDATE avionics_models
               SET manufacturer_identifier_kind = ?,
                   manufacturer_identifier = ?,
                   normalized_manufacturer_identifier = ?
               WHERE id = ?"#,
        )
        .bind(identifier_kind)
        .bind(identifier)
        .bind(stored_identifier_key)
        .bind(model_id)
        .execute(pool(db))
        .await
        .unwrap();
    }

    async fn establish_evidence_backed_manufacturer_identity(db: &AppDb, manufacturer_id: i64) {
        ensure_manufacturer_identity(
            db,
            manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: "https://manufacturer.example/identity".to_string(),
                source_title: "Authoritative manufacturer identity".to_string(),
                evidence_text: "The manufacturer identifies this exact corporate product brand."
                    .to_string(),
            },
        )
        .await
        .unwrap();
    }

    async fn allow_legacy_fixture_inserts(db: &AppDb) {
        let pool = pool(db);
        for trigger in [
            "aircraft_sale_listing_avionics_approved_insert",
            "aircraft_sale_listing_avionics_semantic_unique_insert",
            "aircraft_model_variant_default_avionics_approved_insert",
            "avionics_suite_components_approved_insert",
            "aircraft_reference_versions_require_approval",
            "aircraft_reference_avionics_building_insert",
        ] {
            sqlx::query(&format!("DROP TRIGGER IF EXISTS {trigger}"))
                .execute(pool)
                .await
                .unwrap();
        }
    }

    async fn human_consolidation_request(
        db: &AppDb,
        survivor_id: i64,
        duplicate_ids: Vec<i64>,
    ) -> HumanReviewedAvionicsConsolidationRequest {
        HumanReviewedAvionicsConsolidationRequest {
            survivor_id,
            duplicate_ids,
            reviewer_user_id: sqlx::query_scalar("SELECT id FROM users LIMIT 1")
                .fetch_one(pool(db))
                .await
                .unwrap(),
            authoritative_source_url: "https://static.garmin.com/product-manual.pdf".to_string(),
            authoritative_source_title: "Garmin product manual".to_string(),
            exact_evidence_text:
                "Garmin identifies Integrated Flight Deck as a description of the G1000 product."
                    .to_string(),
            provenance: None,
            expected_authorization_sha256: None,
            expected_catalog_revision_sha256: Some(
                crate::listing::review::approved_catalog_revision_sha256(db)
                    .await
                    .unwrap(),
            ),
        }
    }

    #[tokio::test]
    async fn human_reviewed_descriptive_model_equivalence_applies() {
        let db = test_db().await;
        let (manufacturer_id, survivor_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "G1000",
            &["Integrated Flight Deck"],
        )
        .await;
        let (_, duplicate_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "G1000 Integrated Flight Deck",
            &["Integrated Flight Deck"],
        )
        .await;
        establish_evidence_backed_manufacturer_identity(&db, manufacturer_id).await;

        let listing_id = insert_listing(&db).await;
        let aspect = PendingReviewAspect::avionics(
            "g1000",
            "avionics",
            "G1000 Integrated Flight Deck",
            "Garmin G1000 Integrated Flight Deck",
            "model-equivalence review required",
            1,
            "installed",
            Some("Garmin G1000 Integrated Flight Deck".to_string()),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "G1000 Integrated Flight Deck",
            vec!["Integrated Flight Deck".to_string()],
        ));
        stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();

        let mut request = human_consolidation_request(&db, survivor_id, vec![duplicate_id]).await;
        let preview = preview_human_reviewed_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(preview.consolidation.can_apply, "{preview:?}");
        assert_eq!(preview.consolidation.matches.len(), 1);
        assert_eq!(preview.consolidation.matches[0].duplicate.id, duplicate_id);
        assert_eq!(
            preview.consolidation.matches[0].basis,
            IdentityMatchBasis::HumanReviewedModelEquivalence
        );
        assert_eq!(preview.authorization.canonical_model_key, "g1000");
        assert_eq!(
            preview
                .authorization
                .members
                .iter()
                .map(|member| member.canonical_model_key.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["g1000", "g1000 integrated flight deck"])
        );
        request.expected_authorization_sha256 =
            Some(preview.authorization.authorization_sha256.clone());

        let report = consolidate_avionics_models_with_human_review(&db, &request)
            .await
            .unwrap();
        assert!(report.consolidation.applied);
        assert_eq!(report.consolidation.changes.model_rows_deleted, 1);
        let review_json: String = sqlx::query_scalar(
            "SELECT review_payload_json FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        let review: Value = serde_json::from_str(&review_json).unwrap();
        assert_eq!(
            review["aspects"][0]["proposed_product"]["id"].as_i64(),
            Some(survivor_id)
        );
    }

    #[tokio::test]
    async fn human_reviewed_meaningful_variant_is_rejected() {
        let db = test_db().await;
        let (manufacturer_id, survivor_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "G1000",
            &["Integrated Flight Deck"],
        )
        .await;
        let (_, nxi_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "G1000 NXi",
            &["Integrated Flight Deck"],
        )
        .await;
        establish_evidence_backed_manufacturer_identity(&db, manufacturer_id).await;

        let request = human_consolidation_request(&db, survivor_id, vec![nxi_id]).await;
        let error = preview_human_reviewed_avionics_model_consolidation(&db, &request)
            .await
            .expect_err("G1000 NXi must remain a distinct product");
        assert!(
            error.to_string().contains("meaningful product variant"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn human_reviewed_model_equivalence_requires_complete_closure() {
        let db = test_db().await;
        let (manufacturer_id, survivor_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "G1000",
            &["Integrated Flight Deck"],
        )
        .await;
        let (_, typography_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "Garmin G1000",
            &["Integrated Flight Deck"],
        )
        .await;
        let (_, omitted_descriptive_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "G1000 Integrated Flight Deck",
            &["Integrated Flight Deck"],
        )
        .await;
        establish_evidence_backed_manufacturer_identity(&db, manufacturer_id).await;

        let request = human_consolidation_request(&db, survivor_id, vec![typography_id]).await;
        let error = preview_human_reviewed_avionics_model_consolidation(&db, &request)
            .await
            .expect_err("an omitted descriptive member must block consolidation");
        assert!(
            error
                .to_string()
                .contains(&format!("omitted=[{omitted_descriptive_id}]")),
            "{error}"
        );
    }

    async fn insert_default_candidate(
        db: &AppDb,
        id: i64,
        avionics_model_id: i64,
        source_notes: &str,
    ) {
        let variant_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool(db))
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO aircraft_model_variant_default_avionics_candidates (
                 id, aircraft_model_variant_id, model_year, avionics_model_id,
                 quantity, source_url, source_title, source_notes,
                 source_confidence, pending_reason, created_at
               ) VALUES (?, ?, 2004, ?, 1, 'https://oem.example/manual',
                         'OEM manual', ?, 'high',
                         'factory_default_claim_unverified',
                         '2026-07-30 12:00:00')"#,
        )
        .bind(id)
        .bind(variant_id)
        .bind(avionics_model_id)
        .bind(source_notes)
        .execute(pool(db))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn consolidation_remaps_default_candidate_without_mutating_its_claim() {
        let db = test_db().await;
        let (manufacturer_id, survivor_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "G1000",
            &["Integrated Flight Deck"],
        )
        .await;
        let (_, duplicate_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "G1000 Integrated Flight Deck",
            &["Integrated Flight Deck"],
        )
        .await;
        establish_evidence_backed_manufacturer_identity(&db, manufacturer_id).await;
        insert_default_candidate(&db, 137, duplicate_id, "exact retained claim").await;

        let mut request = human_consolidation_request(&db, survivor_id, vec![duplicate_id]).await;
        let preview = preview_human_reviewed_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(preview.consolidation.can_apply, "{preview:?}");
        assert_eq!(
            preview
                .consolidation
                .changes
                .default_candidate_links_remapped,
            1
        );
        assert_eq!(
            preview
                .consolidation
                .changes
                .default_candidate_link_conflicts_coalesced,
            0
        );
        request.expected_authorization_sha256 = Some(preview.authorization.authorization_sha256);

        consolidate_avionics_models_with_human_review(&db, &request)
            .await
            .unwrap();
        let preserved: (i64, i64, String, String) = sqlx::query_as(
            r#"SELECT id, avionics_model_id, source_notes, created_at
               FROM aircraft_model_variant_default_avionics_candidates
               WHERE id = 137"#,
        )
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(
            preserved,
            (
                137,
                survivor_id,
                "exact retained claim".to_string(),
                "2026-07-30 12:00:00".to_string()
            )
        );
    }

    #[tokio::test]
    async fn consolidation_rejects_conflicting_default_candidates() {
        let db = test_db().await;
        let (manufacturer_id, survivor_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "G1000",
            &["Integrated Flight Deck"],
        )
        .await;
        let (_, duplicate_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "G1000 Integrated Flight Deck",
            &["Integrated Flight Deck"],
        )
        .await;
        establish_evidence_backed_manufacturer_identity(&db, manufacturer_id).await;
        insert_default_candidate(&db, 136, survivor_id, "survivor claim").await;
        insert_default_candidate(&db, 137, duplicate_id, "conflicting duplicate claim").await;

        let mut request = human_consolidation_request(&db, survivor_id, vec![duplicate_id]).await;
        let preview = preview_human_reviewed_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(!preview.consolidation.can_apply);
        assert_eq!(
            preview
                .consolidation
                .changes
                .default_candidate_links_remapped,
            1
        );
        assert!(preview
            .consolidation
            .blockers
            .iter()
            .any(|blocker| blocker.contains("conflicting claims")));
        request.expected_authorization_sha256 = Some(preview.authorization.authorization_sha256);
        assert!(matches!(
            consolidate_avionics_models_with_human_review(&db, &request).await,
            Err(ConsolidationError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn human_reviewed_three_way_consolidation_preserves_audit_and_rewrites_idless_review() {
        let db = test_db().await;
        let (manufacturer_id, survivor_id) =
            insert_legacy_model(&db, "Garmin", "garmin", "GIA-63W", &["GPS"]).await;
        let (_, first_duplicate_id) =
            insert_legacy_model(&db, "Garmin", "garmin", "GIA 63W", &["COM"]).await;
        let (_, second_duplicate_id) =
            insert_legacy_model(&db, "Garmin", "garmin", "GIA63W", &["NAV"]).await;
        establish_evidence_backed_manufacturer_identity(&db, manufacturer_id).await;

        let listing_id = insert_listing(&db).await;
        let aspect = PendingReviewAspect::avionics(
            "gia-63w",
            "avionics",
            "GIA 63W",
            "Garmin GIA 63W",
            "exact catalog collision needs reviewer evidence",
            1,
            "installed",
            Some("The listing identifies a Garmin GIA 63W.".to_string()),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GIA 63W",
            vec!["GPS".to_string(), "COM".to_string(), "NAV".to_string()],
        ));
        let staged = stage_pending_review(&db, listing_id, None, &[aspect.clone()])
            .await
            .unwrap();
        let reviewer_user_id: i64 = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
            .fetch_one(pool(&db))
            .await
            .unwrap();
        let catalog_revision = crate::listing::review::approved_catalog_revision_sha256(&db)
            .await
            .unwrap();
        let mut request = HumanReviewedAvionicsConsolidationRequest {
            survivor_id,
            duplicate_ids: vec![second_duplicate_id, first_duplicate_id],
            reviewer_user_id,
            authoritative_source_url: "https://static.garmin.com/pumac/GIA63W_IM.pdf".to_string(),
            authoritative_source_title: "Garmin GIA 63W installation manual".to_string(),
            exact_evidence_text:
                "Garmin identifies GIA 63W as the product model; punctuation is not a distinct product."
                    .to_string(),
            provenance: Some(HumanReviewedConsolidationProvenance {
                listing_id,
                review_aspect_id: "gia-63w".to_string(),
                expected_review_payload_sha256: staged.review_payload_sha256.clone(),
            }),
            expected_authorization_sha256: None,
            expected_catalog_revision_sha256: Some(catalog_revision),
        };

        let preview = preview_human_reviewed_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(preview.consolidation.can_apply, "{preview:?}");
        assert!(!preview.authorization.persisted);
        assert_eq!(preview.authorization.members.len(), 3);
        request.expected_authorization_sha256 =
            Some(preview.authorization.authorization_sha256.clone());

        // A preview is not an authorization token. Restaging even the same
        // aspect with a changed review payload must invalidate the apply-side
        // expected digest under the transaction lock.
        let mut changed_aspect = aspect;
        changed_aspect.reason = format!(
            "{LEGACY_DUPLICATE_CONSOLIDATION_REASON_PREFIX} \
             ([{first_duplicate_id}, {second_duplicate_id}]); \
             {LEGACY_DUPLICATE_CONSOLIDATION_REASON_SUFFIX}"
        );
        let restaged = stage_pending_review(&db, listing_id, None, &[changed_aspect])
            .await
            .unwrap();
        assert_ne!(staged.review_payload_sha256, restaged.review_payload_sha256);
        assert!(matches!(
            consolidate_avionics_models_with_human_review(&db, &request).await,
            Err(ConsolidationError::Conflict(_))
        ));
        request
            .provenance
            .as_mut()
            .unwrap()
            .expected_review_payload_sha256 = restaged.review_payload_sha256;
        let refreshed_preview = preview_human_reviewed_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        request.expected_authorization_sha256 =
            Some(refreshed_preview.authorization.authorization_sha256);

        let report = consolidate_avionics_models_with_human_review(&db, &request)
            .await
            .unwrap();
        assert!(report.consolidation.applied);
        assert!(report.authorization.persisted);
        assert_eq!(report.consolidation.changes.model_rows_deleted, 2);
        assert_eq!(report.consolidation.changes.type_memberships_added, 2);
        assert_eq!(report.consolidation.changes.pending_reviews_rewritten, 1);

        let surviving_ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM avionics_models ORDER BY id")
                .fetch_all(pool(&db))
                .await
                .unwrap();
        assert_eq!(surviving_ids, vec![survivor_id]);
        let survivor_type_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_model_types WHERE avionics_model_id = ?",
        )
        .bind(survivor_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(survivor_type_count, 3);

        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_catalog_human_consolidation_authorizations",
        )
        .fetch_one(pool(&db))
        .await
        .unwrap();
        let member_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_catalog_human_consolidation_members")
                .fetch_one(pool(&db))
                .await
                .unwrap();
        let transient_count: i64 = sqlx::query_scalar(
            r#"SELECT
                 (SELECT COUNT(*) FROM avionics_catalog_human_consolidation_guard)
                 + (SELECT COUNT(*) FROM avionics_catalog_human_consolidation_claim)"#,
        )
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(authorization_count, 1);
        assert_eq!(member_count, 3);
        assert_eq!(transient_count, 0);

        let review_json: String = sqlx::query_scalar(
            "SELECT review_payload_json FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        let review: Value = serde_json::from_str(&review_json).unwrap();
        assert_eq!(
            review["aspects"][0]["proposed_product"]["id"].as_i64(),
            Some(survivor_id)
        );
        assert_eq!(
            review["aspects"][0]["reason"].as_str(),
            Some(CONSOLIDATED_PENDING_IDENTITY_REASON)
        );
    }

    #[tokio::test]
    async fn exhaustive_audit_and_batch_plan_use_current_canonical_keys() {
        let db = test_db().await;
        let (first_mfr, first) =
            insert_legacy_model(&db, "Bendix King", "bendix king", "KAP 140", &["Autopilot"]).await;
        let (_second_mfr, second) =
            insert_legacy_model(&db, "BendixKing", "bendixking", "KAP-140", &["Autopilot"]).await;
        let (_, other_maker) =
            insert_legacy_model(&db, "Honeywell", "honeywell", "Unrelated", &["Autopilot"]).await;
        set_legacy_identifier(&db, first, "KAP-140", "kap140").await;
        set_legacy_identifier(&db, second, "KAP 140", "kap140").await;
        set_legacy_identifier(&db, other_maker, "KAP/140", "kap140").await;
        establish_evidence_backed_manufacturer_identity(&db, first_mfr).await;
        let listing_id = insert_listing(&db).await;
        allow_legacy_fixture_inserts(&db).await;
        sqlx::query(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id, source, source_confidence) VALUES (?, ?, 'listing_review', 'high')",
        )
        .bind(listing_id)
        .bind(second)
        .execute(pool(&db))
        .await
        .unwrap();

        let audit = audit_avionics_catalog_duplicates(&db).await.unwrap();
        assert!(audit.exact_groups.is_empty());
        assert_eq!(audit.canonical_groups.len(), 1);
        assert_eq!(audit.identifier_groups.len(), 1);
        assert_eq!(
            audit.canonical_groups[0]
                .models
                .iter()
                .map(|model| model.id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert_eq!(
            audit.identifier_groups[0]
                .models
                .iter()
                .map(|model| model.id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        let plans = plan_canonical_legacy_duplicates(&db).await.unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].request.survivor_id, second);
        assert_eq!(plans[0].request.duplicate_ids, vec![first]);
        assert_eq!(plans[0].model_keys, vec!["kap140".to_string()]);
        assert_eq!(
            plans[0].identifier_keys,
            vec![StableIdentifierKey {
                kind: "manufacturer_part_number".to_string(),
                value: "kap140".to_string(),
            }]
        );
        assert!(plans[0].blockers.is_empty());

        let cross_manufacturer = preview_avionics_model_consolidation(
            &db,
            &AvionicsConsolidationRequest {
                survivor_id: second,
                duplicate_ids: vec![other_maker],
            },
        )
        .await
        .unwrap();
        assert!(!cross_manufacturer.can_apply);
        assert!(cross_manufacturer.matches.is_empty());

        consolidate_avionics_models(&db, &plans[0].request)
            .await
            .unwrap();
        let final_audit = audit_avionics_catalog_duplicates(&db).await.unwrap();
        assert!(final_audit.canonical_groups.is_empty());
        assert!(final_audit.identifier_groups.is_empty());
    }

    #[tokio::test]
    async fn consolidation_refuses_ambiguous_same_listing_associations() {
        let db = test_db().await;
        let (survivor_manufacturer_id, survivor_id) =
            insert_legacy_model(&db, "BendixKing", "bendixking", "KMA 24", &["Audio Panel"]).await;
        let (duplicate_manufacturer_id, duplicate_id) =
            insert_legacy_model(&db, "Bendix King", "bendix king", "KMA-24", &["COM"]).await;
        assert_ne!(survivor_manufacturer_id, duplicate_manufacturer_id);
        set_legacy_identifier(&db, survivor_id, "KMA-24", "kma24").await;
        set_legacy_identifier(&db, duplicate_id, "KMA 24", "kma24").await;
        establish_evidence_backed_manufacturer_identity(&db, survivor_manufacturer_id).await;
        let listing_id = insert_listing(&db).await;
        allow_legacy_fixture_inserts(&db).await;
        let first_link: i64 = sqlx::query_scalar(
            r#"INSERT INTO aircraft_sale_listing_avionics (
                 aircraft_sale_listing_id, avionics_model_id, quantity, source,
                 source_notes, source_confidence
               ) VALUES (?, ?, 1, 'listing', 'first observation', 'low')
               RETURNING id"#,
        )
        .bind(listing_id)
        .bind(survivor_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        let second_link: i64 = sqlx::query_scalar(
            r#"INSERT INTO aircraft_sale_listing_avionics (
                 aircraft_sale_listing_id, avionics_model_id, quantity, source,
                 source_notes, source_confidence
               ) VALUES (?, ?, 2, 'listing_review', 'second observation', 'high')
               RETURNING id"#,
        )
        .bind(listing_id)
        .bind(duplicate_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        let aspect = PendingReviewAspect::avionics(
            "legacy-duplicate",
            "avionics",
            "KMA 24",
            "Bendix King KMA 24",
            "legacy duplicate",
            2,
            "installed",
            Some("listing evidence".to_string()),
            Some("low".to_string()),
        )
        .with_covered_association(first_link, ListingAssociationRole::Installed, survivor_id)
        .with_covered_association(
            second_link,
            ListingAssociationRole::Installed,
            duplicate_id,
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();

        let request = AvionicsConsolidationRequest {
            survivor_id,
            duplicate_ids: vec![duplicate_id],
        };
        let preview = preview_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(!preview.can_apply);
        assert_eq!(preview.changes.listing_link_conflicts_coalesced, 0);
        assert!(preview
            .blockers
            .iter()
            .any(|blocker| blocker.contains("multiple associations that would collapse")));
        assert!(matches!(
            consolidate_avionics_models(&db, &request).await,
            Err(ConsolidationError::Conflict(_))
        ));

        let model_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models WHERE id = ?")
                .bind(duplicate_id)
                .fetch_one(pool(&db))
                .await
                .unwrap();
        assert_eq!(model_count, 1);
        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(link_count, 2);
        let (payload_json, payload_hash): (String, String) = sqlx::query_as(
            "SELECT review_payload_json, review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(payload_hash, staged.review_payload_sha256);
        let payload: Value = serde_json::from_str(&payload_json).unwrap();
        let coverage = payload["aspects"][0]["covered_associations"]
            .as_array()
            .unwrap();
        assert_eq!(coverage.len(), 2);
        let manufacturer_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_manufacturers WHERE id = ?")
                .bind(duplicate_manufacturer_id)
                .fetch_one(pool(&db))
                .await
                .unwrap();
        assert_eq!(manufacturer_count, 1);
        let guard_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_catalog_consolidation_guard")
                .fetch_one(pool(&db))
                .await
                .unwrap();
        assert_eq!(guard_count, 0);
    }

    #[tokio::test]
    async fn punctuation_equal_raw_makers_require_evidence_backed_identity_scope() {
        let db = test_db().await;
        let (survivor_manufacturer_id, survivor_id) =
            insert_legacy_model(&db, "Garmin Inc.", "gar min", "GTX 345", &["Transponder"]).await;
        let (duplicate_manufacturer_id, duplicate_id) =
            insert_legacy_model(&db, "Garmin", "garmin", "GTX 345 Remote", &["Transponder"]).await;
        assert_ne!(survivor_manufacturer_id, duplicate_manufacturer_id);
        set_legacy_identifier(&db, survivor_id, "010-01750-01", "0100175001").await;
        set_legacy_identifier(&db, duplicate_id, "010 01750 01", "0100175001").await;

        let audit = audit_avionics_catalog_duplicates(&db).await.unwrap();
        assert!(audit.canonical_groups.is_empty());
        assert_eq!(audit.identifier_groups.len(), 1);
        let request = AvionicsConsolidationRequest {
            survivor_id,
            duplicate_ids: vec![duplicate_id],
        };
        let plans = plan_canonical_legacy_duplicates(&db).await.unwrap();
        assert!(plans.is_empty());
        let preview = preview_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(!preview.can_apply);
        assert!(preview.matches.is_empty());
        assert!(matches!(
            consolidate_avionics_models(&db, &request).await,
            Err(ConsolidationError::Conflict(_))
        ));
        let direct_guard = sqlx::query(
            "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id, survivor_model_id) VALUES (?, ?)",
        )
        .bind(duplicate_id)
        .bind(survivor_id)
        .execute(pool(&db))
        .await;
        assert!(direct_guard.is_err());
        let retained_duplicate: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models WHERE id = ?")
                .bind(duplicate_id)
                .fetch_one(pool(&db))
                .await
                .unwrap();
        assert_eq!(retained_duplicate, 1);

        establish_evidence_backed_manufacturer_identity(&db, survivor_manufacturer_id).await;
        let plans = plan_canonical_legacy_duplicates(&db).await.unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].model_keys.len(), 2);
        assert_eq!(
            plans[0].identifier_keys,
            vec![StableIdentifierKey {
                kind: "manufacturer_part_number".to_string(),
                value: "0100175001".to_string(),
            }]
        );
        assert!(plans[0].blockers.is_empty());
        let preview = preview_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(preview.can_apply, "{:?}", preview.blockers);
        assert_eq!(
            preview.matches[0].basis,
            IdentityMatchBasis::StableManufacturerIdentifier
        );
        consolidate_avionics_models(&db, &request).await.unwrap();
        assert!(audit_avionics_catalog_duplicates(&db)
            .await
            .unwrap()
            .identifier_groups
            .is_empty());
    }

    #[tokio::test]
    async fn unreviewed_survivor_cannot_absorb_an_approved_duplicate() {
        let db = test_db().await;
        let (unreviewed_manufacturer_id, unreviewed_id) =
            insert_legacy_model(&db, "Garmin Inc.", "gar min", "GTX 345", &["Transponder"]).await;
        let (approved_manufacturer_id, approved_id) =
            insert_legacy_model(&db, "Garmin", "garmin", "GTX 345 Remote", &["Transponder"]).await;
        assert_ne!(unreviewed_manufacturer_id, approved_manufacturer_id);
        set_legacy_identifier(&db, unreviewed_id, "010-01750-01", "0100175001").await;
        set_legacy_identifier(&db, approved_id, "010 01750 01", "0100175001").await;
        establish_evidence_backed_manufacturer_identity(&db, unreviewed_manufacturer_id).await;
        sqlx::query(
            r#"UPDATE avionics_models
               SET identity_source_url='https://manufacturer.example/gtx345',
                   identity_source_title='GTX 345 product data sheet',
                   identity_evidence_text='The manufacturer identifies this exact GTX 345 product and part number.',
                   identity_evidence_kind='authoritative_reference',
                   identity_confidence='very_high',
                   catalog_reviewed_at=CURRENT_TIMESTAMP,
                   catalog_status='approved'
               WHERE id=?"#,
        )
        .bind(approved_id)
        .execute(pool(&db))
        .await
        .unwrap();

        let unsafe_request = AvionicsConsolidationRequest {
            survivor_id: unreviewed_id,
            duplicate_ids: vec![approved_id],
        };
        let preview = preview_avionics_model_consolidation(&db, &unsafe_request)
            .await
            .unwrap();
        assert!(!preview.can_apply);
        assert!(preview
            .blockers
            .iter()
            .any(|blocker| blocker.contains("only unreviewed legacy rows")));
        let direct_guard = sqlx::query(
            "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id, survivor_model_id) VALUES (?, ?)",
        )
        .bind(approved_id)
        .bind(unreviewed_id)
        .execute(pool(&db))
        .await;
        assert!(direct_guard.is_err());
        assert!(matches!(
            consolidate_avionics_models(&db, &unsafe_request).await,
            Err(ConsolidationError::Conflict(_))
        ));

        let approved_survivor_request = AvionicsConsolidationRequest {
            survivor_id: approved_id,
            duplicate_ids: vec![unreviewed_id],
        };
        let preview = preview_avionics_model_consolidation(&db, &approved_survivor_request)
            .await
            .unwrap();
        assert!(preview.can_apply, "{:?}", preview.blockers);
        consolidate_avionics_models(&db, &approved_survivor_request)
            .await
            .unwrap();
        let survivor_status: String =
            sqlx::query_scalar("SELECT catalog_status FROM avionics_models WHERE id = ?")
                .bind(approved_id)
                .fetch_one(pool(&db))
                .await
                .unwrap();
        assert_eq!(survivor_status, "approved");
    }

    #[tokio::test]
    async fn stale_persisted_identifier_keys_block_consolidation() {
        let db = test_db().await;
        let (_, survivor_id) =
            insert_legacy_model(&db, "Garmin", "garmin", "GTX 345", &["Transponder"]).await;
        let (_, duplicate_id) = insert_legacy_model(
            &db,
            "Garmin Inc.",
            "gar min",
            "GTX 345 Remote",
            &["Transponder"],
        )
        .await;
        set_legacy_identifier(&db, survivor_id, "010-01750-01", "wrong-survivor-key").await;
        set_legacy_identifier(&db, duplicate_id, "010 01750 01", "wrong-duplicate-key").await;

        let request = AvionicsConsolidationRequest {
            survivor_id,
            duplicate_ids: vec![duplicate_id],
        };
        let preview = preview_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(!preview.can_apply);
        assert!(preview
            .blockers
            .iter()
            .any(|blocker| blocker.contains("stale or corrupt persisted")));
        assert!(matches!(
            consolidate_avionics_models(&db, &request).await,
            Err(ConsolidationError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn identical_identifier_values_with_different_kinds_are_not_identity_edges() {
        let db = test_db().await;
        let (_, part_number_id) = insert_legacy_model(
            &db,
            "Garmin Inc.",
            "gar min",
            "Remote Transponder",
            &["Transponder"],
        )
        .await;
        let (_, sku_id) = insert_legacy_model(
            &db,
            "Garmin",
            "garmin",
            "Installation Bundle",
            &["Transponder"],
        )
        .await;
        set_legacy_identifier_kind(
            &db,
            part_number_id,
            "manufacturer_part_number",
            "010-01750-01",
            "0100175001",
        )
        .await;
        set_legacy_identifier_kind(&db, sku_id, "sku", "010-01750-01", "0100175001").await;

        let audit = audit_avionics_catalog_duplicates(&db).await.unwrap();
        assert!(audit.canonical_groups.is_empty());
        assert!(audit.identifier_groups.is_empty());
        assert!(plan_canonical_legacy_duplicates(&db)
            .await
            .unwrap()
            .is_empty());

        let preview = preview_avionics_model_consolidation(
            &db,
            &AvionicsConsolidationRequest {
                survivor_id: part_number_id,
                duplicate_ids: vec![sku_id],
            },
        )
        .await
        .unwrap();
        assert!(!preview.can_apply);
        assert!(preview.matches.is_empty());
    }

    #[tokio::test]
    async fn database_guard_is_pair_scoped_immutable_and_never_reuses_preexisting_rows() {
        let db = test_db().await;
        let (survivor_manufacturer_id, survivor_id) =
            insert_legacy_model(&db, "Bendix King", "bendix king", "KX 155", &["NAV"]).await;
        let (_, duplicate_id) =
            insert_legacy_model(&db, "BendixKing", "bendixking", "KX-155", &["NAV"]).await;
        let (_, unrelated_id) =
            insert_legacy_model(&db, "Honeywell", "honeywell", "KX 155", &["NAV"]).await;
        set_legacy_identifier(&db, survivor_id, "KX-155", "kx155").await;
        set_legacy_identifier(&db, duplicate_id, "KX 155", "kx155").await;
        establish_evidence_backed_manufacturer_identity(&db, survivor_manufacturer_id).await;

        let unauthorized = sqlx::query(
            "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id, survivor_model_id) VALUES (?, ?)",
        )
        .bind(unrelated_id)
        .bind(survivor_id)
        .execute(pool(&db))
        .await;
        assert!(unauthorized.is_err());

        sqlx::query(
            "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id, survivor_model_id) VALUES (?, ?)",
        )
        .bind(duplicate_id)
        .bind(survivor_id)
        .execute(pool(&db))
        .await
        .unwrap();
        let mutable = sqlx::query(
            "UPDATE avionics_catalog_consolidation_guard SET purpose = purpose WHERE duplicate_model_id = ?",
        )
        .bind(duplicate_id)
        .execute(pool(&db))
        .await;
        assert!(mutable.is_err());
        let parent_mutation = sqlx::query("UPDATE avionics_models SET name = name WHERE id = ?")
            .bind(survivor_id)
            .execute(pool(&db))
            .await;
        assert!(parent_mutation.is_err());

        let request = AvionicsConsolidationRequest {
            survivor_id,
            duplicate_ids: vec![duplicate_id],
        };
        let conflict = consolidate_avionics_models(&db, &request).await;
        assert!(matches!(conflict, Err(ConsolidationError::Conflict(_))));
        let retained: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_catalog_consolidation_guard WHERE duplicate_model_id = ? AND survivor_model_id = ?",
        )
        .bind(duplicate_id)
        .bind(survivor_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(retained, 1);
        sqlx::query(
            "DELETE FROM avionics_catalog_consolidation_guard WHERE duplicate_model_id = ?",
        )
        .bind(duplicate_id)
        .execute(pool(&db))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn bulk_planner_does_not_bridge_name_only_and_identifier_edges() {
        let db = test_db().await;
        let (first_manufacturer_id, first_id) =
            insert_legacy_model(&db, "Garmin Inc.", "gar min", "Alpha", &["NAV"]).await;
        let (_, bridge_id) = insert_legacy_model(&db, "Garmin", "g armin", "Alpha", &["NAV"]).await;
        let (_, third_id) =
            insert_legacy_model(&db, "Garmin Corp.", "ga rmin", "Bravo", &["NAV"]).await;
        set_legacy_identifier(&db, first_id, "A-1", "a1").await;
        set_legacy_identifier(&db, bridge_id, "B-2", "b2").await;
        set_legacy_identifier(&db, third_id, "B 2", "b2").await;
        establish_evidence_backed_manufacturer_identity(&db, first_manufacturer_id).await;

        let plans = plan_canonical_legacy_duplicates(&db).await.unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].request.survivor_id, bridge_id);
        assert_eq!(plans[0].request.duplicate_ids, vec![third_id]);
        assert_eq!(plans[0].model_keys, vec!["alpha", "bravo"]);
        assert_eq!(
            plans[0].identifier_keys,
            vec![StableIdentifierKey {
                kind: "manufacturer_part_number".to_string(),
                value: "b2".to_string(),
            }]
        );
        assert!(plans[0].blockers.is_empty());

        let preview = preview_avionics_model_consolidation(&db, &plans[0].request)
            .await
            .unwrap();
        assert!(preview.can_apply, "{:?}", preview.blockers);
        assert!(!plans[0].request.duplicate_ids.contains(&first_id));
    }

    #[tokio::test]
    async fn guarded_consolidation_refuses_ready_or_verified_listing_links() {
        let db = test_db().await;
        let (_, survivor_id) =
            insert_legacy_model(&db, "Garmin", "garmin", "G1000", &["GPS"]).await;
        let (_, duplicate_id) =
            insert_legacy_model(&db, "Garmin", "garmin", "G1000", &["GPS"]).await;
        let listing_id = insert_listing(&db).await;
        allow_legacy_fixture_inserts(&db).await;
        sqlx::query(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id) VALUES (?, ?)",
        )
        .bind(listing_id)
        .bind(duplicate_id)
        .execute(pool(&db))
        .await
        .unwrap();
        // Simulate a pre-migration corrupt legacy row. Current schemas reject
        // verified-but-not-ready state at the database boundary.
        sqlx::query("DROP TRIGGER listing_verified_requires_ready_update")
            .execute(pool(&db))
            .await
            .unwrap();
        sqlx::query("UPDATE aircraft_sale_listings SET is_verified = TRUE WHERE id = ?")
            .bind(listing_id)
            .execute(pool(&db))
            .await
            .unwrap();
        let request = AvionicsConsolidationRequest {
            survivor_id,
            duplicate_ids: vec![duplicate_id],
        };
        let preview = preview_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(!preview.can_apply);
        assert!(preview
            .blockers
            .iter()
            .any(|blocker| blocker.contains("ready or verified")));
        assert!(matches!(
            consolidate_avionics_models(&db, &request).await,
            Err(ConsolidationError::Conflict(_))
        ));
        let duplicate_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models WHERE id = ?")
                .bind(duplicate_id)
                .fetch_one(pool(&db))
                .await
                .unwrap();
        assert_eq!(duplicate_count, 1);
    }

    #[tokio::test]
    async fn consolidation_remaps_default_suite_and_reference_roles() {
        let db = test_db().await;
        let (survivor_manufacturer_id, survivor_id) =
            insert_legacy_model(&db, "BendixKing", "bendixking", "KX 155", &["NAV"]).await;
        let (_, duplicate_id) =
            insert_legacy_model(&db, "Bendix King", "bendix king", "KX-155", &["COM"]).await;
        set_legacy_identifier(&db, survivor_id, "KX-155", "kx155").await;
        set_legacy_identifier(&db, duplicate_id, "KX 155", "kx155").await;
        establish_evidence_backed_manufacturer_identity(&db, survivor_manufacturer_id).await;
        let (_, component_id) = insert_legacy_model(
            &db,
            "Legacy Component",
            "legacy component",
            "KI 209",
            &["CDI"],
        )
        .await;
        insert_listing(&db).await;
        let variant_id: i64 = sqlx::query_scalar(
            r#"SELECT variant.id
               FROM aircraft_model_variants variant
               JOIN aircraft_models model ON model.id = variant.aircraft_model_id
               JOIN aircraft_manufacturers manufacturer
                 ON manufacturer.id = model.aircraft_manufacturer_id
               WHERE manufacturer.normalized_name = 'cessna'
                 AND model.normalized_name = '182t'
                 AND variant.normalized_name = 'standard'"#,
        )
        .fetch_one(pool(&db))
        .await
        .unwrap();
        allow_legacy_fixture_inserts(&db).await;

        for (model_id, quantity, suffix) in
            [(survivor_id, 1_i64, "first"), (duplicate_id, 3, "second")]
        {
            sqlx::query(
                r#"INSERT INTO aircraft_model_variant_default_avionics (
                     aircraft_model_variant_id, model_year, avionics_model_id,
                     quantity, source_url, source_title, source_notes, source_confidence
                   ) VALUES (?, 2004, ?, ?, ?, ?, ?, 'high')"#,
            )
            .bind(variant_id)
            .bind(model_id)
            .bind(quantity)
            .bind(format!("https://evidence.example/{suffix}"))
            .bind(format!("{suffix} source"))
            .bind(format!("{suffix} notes"))
            .execute(pool(&db))
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO avionics_suite_components (suite_model_id, component_model_id, quantity) VALUES (?, ?, 1), (?, ?, 4), (?, ?, 1)",
        )
        .bind(survivor_id)
        .bind(component_id)
        .bind(duplicate_id)
        .bind(component_id)
        .bind(survivor_id)
        .bind(duplicate_id)
        .execute(pool(&db))
        .await
        .unwrap();

        // Only the directly joined version row is relevant to consolidation.
        // Foreign keys are disabled on this fixture connection so the test
        // need not construct the unrelated aircraft-curation hierarchy.
        let mut connection = pool(&db).acquire().await.unwrap();
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO aircraft_reference_configuration_versions (
                 id, aircraft_reference_configuration_id, model_year, revision,
                 publication_state, approval_decision_id
               ) VALUES (9001, 9001, 2004, 1, 'building', 9001)"#,
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO aircraft_reference_avionics (
                 aircraft_reference_configuration_version_id, avionics_model_id,
                 quantity, equipment_role, evidence_claim_id
               ) VALUES (9001, ?, 1, 'standard', 9001),
                        (9001, ?, 3, 'standard', 9001)"#,
        )
        .bind(survivor_id)
        .bind(duplicate_id)
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await
            .unwrap();
        drop(connection);

        let request = AvionicsConsolidationRequest {
            survivor_id,
            duplicate_ids: vec![duplicate_id],
        };
        let preview = preview_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(preview.can_apply, "{:?}", preview.blockers);
        assert_eq!(preview.changes.default_link_conflicts_coalesced, 1);
        assert_eq!(preview.changes.reference_link_conflicts_coalesced, 1);
        assert_eq!(preview.changes.suite_link_conflicts_coalesced, 1);
        assert_eq!(preview.changes.suite_self_links_removed, 1);
        consolidate_avionics_models(&db, &request).await.unwrap();

        let default_link: (i64, i64) = sqlx::query_as(
            "SELECT avionics_model_id, quantity FROM aircraft_model_variant_default_avionics WHERE aircraft_model_variant_id = ? AND model_year = 2004",
        )
        .bind(variant_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(default_link, (survivor_id, 3));
        let suite_link: (i64, i64, i64) = sqlx::query_as(
            "SELECT suite_model_id, component_model_id, quantity FROM avionics_suite_components",
        )
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(suite_link, (survivor_id, component_id, 4));
        let reference_link: (i64, i64) = sqlx::query_as(
            "SELECT avionics_model_id, quantity FROM aircraft_reference_avionics WHERE aircraft_reference_configuration_version_id = 9001",
        )
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(reference_link, (survivor_id, 3));
    }

    #[tokio::test]
    async fn approved_cross_identity_alias_consolidates_without_status_demotion() {
        let db = test_db().await;
        let (legacy_manufacturer_id, duplicate_id) = insert_legacy_model(
            &db,
            "L-3 Communications",
            "l 3 communications",
            "WX-500 Legacy",
            &["Weather"],
        )
        .await;
        let (current_manufacturer_id, survivor_id) =
            insert_legacy_model(&db, "L3Harris", "l3harris", "WX-500", &["Weather"]).await;
        set_legacy_identifier(&db, duplicate_id, "WX-500", "wx500").await;
        set_legacy_identifier(&db, survivor_id, "WX 500", "wx500").await;

        let evidence = ManufacturerIdentityEvidence {
            source_url: "https://manufacturer.example/identity".to_string(),
            source_title: "Authoritative manufacturer identity".to_string(),
            evidence_text: "The manufacturer identifies this exact corporate product brand."
                .to_string(),
        };
        let legacy_identity = ensure_manufacturer_identity(&db, legacy_manufacturer_id, &evidence)
            .await
            .unwrap();
        let current_identity =
            ensure_manufacturer_identity(&db, current_manufacturer_id, &evidence)
                .await
                .unwrap();
        assert_ne!(
            legacy_identity.avionics_manufacturer_identity_id,
            current_identity.avionics_manufacturer_identity_id
        );

        for model_id in [duplicate_id, survivor_id] {
            sqlx::query(
                r#"UPDATE avionics_models
                   SET identity_source_url='https://manufacturer.example/wx500',
                       identity_source_title='WX-500 product data sheet',
                       identity_evidence_text='The manufacturer identifies this exact WX-500 product and model number.',
                       identity_evidence_kind='authoritative_reference',
                       identity_confidence='very_high',
                       catalog_reviewed_at=CURRENT_TIMESTAMP,
                       catalog_status='approved'
                   WHERE id=?"#,
            )
            .bind(model_id)
            .execute(pool(&db))
            .await
            .unwrap();
        }
        let candidate = stage_manufacturer_alias_candidate(
            &db,
            &StageManufacturerAliasCandidateRequest {
                avionics_manufacturer_id: legacy_manufacturer_id,
                candidate_manufacturer_identity_id: current_identity
                    .avionics_manufacturer_identity_id,
                candidate_basis: AliasCandidateBasis::GroundedAlias,
                matched_avionics_model_id: Some(duplicate_id),
                reason: "Authoritative corporate history connects the two manufacturer identities."
                    .to_string(),
                evidence: Some(evidence.clone()),
                confidence: "very_high".to_string(),
            },
        )
        .await
        .unwrap();
        let reviewer_id: i64 = sqlx::query_scalar("SELECT id FROM users ORDER BY id LIMIT 1")
            .fetch_one(pool(&db))
            .await
            .unwrap();
        let approval = approve_manufacturer_alias_candidate(
            &db,
            &ApproveManufacturerAliasCandidateRequest {
                candidate_id: candidate.id,
                evidence,
                reviewed_by_user_id: reviewer_id,
            },
        )
        .await
        .unwrap();
        assert_eq!(approval.blocking_product_collision_count, 1);
        assert!(!approval.identity_merge_created);

        let approved_identity_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_approved_product_identities WHERE avionics_model_id IN (?, ?)",
        )
        .bind(duplicate_id)
        .bind(survivor_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(approved_identity_count, 2);
        sqlx::query(
            r#"CREATE TRIGGER test_consolidation_never_updates_catalog_status
               BEFORE UPDATE OF catalog_status ON avionics_models
               BEGIN
                 SELECT RAISE(ABORT, 'catalog status mutation is forbidden during consolidation');
               END"#,
        )
        .execute(pool(&db))
        .await
        .unwrap();
        let request = AvionicsConsolidationRequest {
            survivor_id,
            duplicate_ids: vec![duplicate_id],
        };
        let preview = preview_avionics_model_consolidation(&db, &request)
            .await
            .unwrap();
        assert!(preview.can_apply, "{:?}", preview.blockers);
        consolidate_avionics_models(&db, &request).await.unwrap();
        let survivor_status: String =
            sqlx::query_scalar("SELECT catalog_status FROM avionics_models WHERE id = ?")
                .bind(survivor_id)
                .fetch_one(pool(&db))
                .await
                .unwrap();
        assert_eq!(survivor_status, "approved");
        let duplicate_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models WHERE id = ?")
                .bind(duplicate_id)
                .fetch_one(pool(&db))
                .await
                .unwrap();
        assert_eq!(duplicate_count, 0);
        let survivor_identity_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_approved_product_identities WHERE avionics_model_id = ?",
        )
        .bind(survivor_id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(survivor_identity_count, 1);
        let matched_model_id: Option<i64> = sqlx::query_scalar(
            "SELECT matched_avionics_model_id FROM avionics_manufacturer_alias_candidates WHERE id = ?",
        )
        .bind(candidate.id)
        .fetch_one(pool(&db))
        .await
        .unwrap();
        assert_eq!(matched_model_id, Some(survivor_id));
        let retained_raw_makers: i64 =
            sqlx::query_scalar("SELECT count(*) FROM avionics_manufacturers WHERE id IN (?, ?)")
                .bind(legacy_manufacturer_id)
                .bind(current_manufacturer_id)
                .fetch_one(pool(&db))
                .await
                .unwrap();
        assert_eq!(retained_raw_makers, 2);
        assert!(
            finalize_approved_manufacturer_identity_merge(&db, candidate.id)
                .await
                .unwrap()
                .identity_merge_created
        );
    }
}
