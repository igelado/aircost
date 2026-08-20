//! Atomic persistence boundary for automated listing-avionics review.
//!
//! The model/API work that proposes accepted links intentionally happens
//! outside this module. This boundary revalidates every mutable dependency and
//! either applies the complete accepted/residual result or commits nothing.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::aircraft::faa::{normalize_n_number, normalize_serial_key};
use crate::avionics::reuse::{
    product_reuse_attestation_is_current, reuse_attestation_is_current_postgres,
    reuse_attestation_is_current_sqlite,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::listing::avionics::{
    approved_avionics_product_key, validate_canonical_avionics_actions, CanonicalAvionicsAction,
};

use super::{
    active_collision_closure_member_ids, association_observation_sha256_from_values,
    catalog_product_fingerprints, catalog_products, conservative_confidence,
    fingerprint_active_collision_closure, fingerprint_catalog_products,
    fingerprint_grounded_collision_closure, merged_notes, parse_payload, serialize_review_payload,
    sha256_hex, valid_sha256, validate_exact_listing_evidence_span,
    validate_exact_listing_product_evidence, ActiveCollisionCatalogFingerprintRow,
    CatalogFingerprintRow, ListingAssociationRole, PendingReviewAspect, ReviewError, ReviewResult,
    ACTIVE_COLLISION_CATALOG_ROWS_SQL, APPROVED_CATALOG_ROWS_SQL,
    ASSOCIATION_AUTHORIZATION_POLICY_VERSION, POSTGRES_LISTING_CHILD_LOCK_SQL,
};
use crate::avionics::catalog::GroundedAvionicsResolutionReceipt;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct AutomatedPreservedAssociationGuard {
    pub listing_link_id: i64,
    pub association_role: ListingAssociationRole,
    pub expected_observation_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AutomatedAssociationAuthorization {
    ManufacturerReuse,
    SameCaseGrounded(GroundedAvionicsResolutionReceipt),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AutomatedAvionicsLink {
    pub avionics_model_id: i64,
    pub authorization: AutomatedAssociationAuthorization,
    pub expected_collision_closure_sha256: String,
    pub quantity: i64,
    pub source_notes: Option<String>,
    pub source_confidence: Option<String>,
    pub configuration_action: String,
    pub replaces_avionics_model_id: Option<i64>,
    pub replacement_authorization: Option<AutomatedAssociationAuthorization>,
    pub expected_replacement_collision_closure_sha256: Option<String>,
    pub preserved_association_guard: Option<AutomatedPreservedAssociationGuard>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AutomatedReviewApplyRequest {
    pub listing_id: i64,
    pub plugin_submission_id: i64,
    pub expected_review_payload_sha256: String,
    pub expected_rendered_html_sha256: String,
    pub expected_faa_snapshot_id: i64,
    pub expected_faa_source_record_sha256: String,
    pub accepted_links: Vec<AutomatedAvionicsLink>,
    pub residual_aspects: Vec<PendingReviewAspect>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct AutomatedReviewApplyResult {
    pub listing_id: i64,
    pub plugin_submission_id: i64,
    pub accepted_link_count: i64,
    pub preserved_link_count: i64,
    pub stored_link_count: i64,
    pub residual_aspect_count: i64,
    pub review_payload_sha256: Option<String>,
    pub catalog_revision_sha256: Option<String>,
    pub ingestion_state: String,
}

#[derive(Debug, FromRow)]
struct AutomationGuardRow {
    owner_user_id: i64,
    listing_source_url: Option<String>,
    registration_number: Option<String>,
    serial_number: Option<String>,
    ingestion_state: String,
    is_verified: bool,
    pending_aspect_count: i64,
    review_payload_json: String,
    review_payload_sha256: String,
    attached_plugin_submission_id: Option<i64>,
    submission_user_id: i64,
    submission_source_url: String,
    submission_canonical_listing_id: Option<i64>,
    rendered_html: String,
    rendered_html_sha256: String,
}

#[derive(Debug, FromRow)]
struct FaaRecordRow {
    manufacturer_serial_raw: Option<String>,
    manufacturer_serial_key: Option<String>,
    source_record_sha256: String,
}

#[derive(Debug, FromRow)]
struct ExistingLinkRow {
    id: i64,
    avionics_model_id: i64,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    installed_catalog_status: Option<String>,
    installed_manufacturer_identity_id: Option<i64>,
    installed_product_key: Option<String>,
    replacement_catalog_status: Option<String>,
    replacement_manufacturer_identity_id: Option<i64>,
    replacement_product_key: Option<String>,
}

#[derive(Debug, FromRow)]
struct ExistingSameCaseAuthorizationRow {
    listing_link_id: i64,
    association_role: String,
    avionics_model_id: i64,
    observation_sha256: String,
    product_fingerprint: String,
    grounded_resolution_sha256: Option<String>,
    evidence_capture_is_current: bool,
    collision_closure_sha256: String,
    policy_version: String,
}

#[derive(Clone, Debug)]
struct CatalogGraphIdentity {
    key: String,
    manufacturer: String,
    model: String,
}

#[derive(Clone, Debug)]
struct PreparedLink {
    avionics_model_id: i64,
    authorization: AutomatedAssociationAuthorization,
    subject_key: String,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    replacement_authorization: Option<AutomatedAssociationAuthorization>,
    replacement_key: Option<String>,
}

pub(crate) fn validate_automated_avionics_link(
    listing_id: i64,
    link: &AutomatedAvionicsLink,
) -> ReviewResult<()> {
    if link.avionics_model_id <= 0 || link.quantity <= 0 {
        return Err(ReviewError::Validation(
            "accepted avionics IDs and quantities must be positive".to_string(),
        ));
    }
    if link.source_confidence.as_deref() != Some("high") {
        return Err(ReviewError::Validation(format!(
            "automated acceptance for avionics catalog id {} requires high listing-source confidence",
            link.avionics_model_id
        )));
    }
    if !valid_sha256(&link.expected_collision_closure_sha256)
        || link
            .expected_replacement_collision_closure_sha256
            .as_deref()
            .is_some_and(|revision| !valid_sha256(revision))
    {
        return Err(ReviewError::Validation(format!(
            "automated acceptance for avionics catalog id {} requires lowercase collision-closure SHA-256 revisions",
            link.avionics_model_id
        )));
    }
    if let Some(guard) = link.preserved_association_guard.as_ref() {
        if guard.listing_link_id <= 0
            || guard.association_role != ListingAssociationRole::Installed
            || !valid_sha256(&guard.expected_observation_sha256)
            || link.configuration_action != "installed"
            || link.replaces_avionics_model_id.is_some()
        {
            return Err(ReviewError::Validation(format!(
                "preserved-association guard for avionics catalog id {} is invalid",
                link.avionics_model_id
            )));
        }
    }
    for (target_id, authorization) in std::iter::once((link.avionics_model_id, &link.authorization))
        .chain(
            link.replaces_avionics_model_id
                .zip(link.replacement_authorization.as_ref()),
        )
    {
        if let AutomatedAssociationAuthorization::SameCaseGrounded(receipt) = authorization {
            if receipt.listing_id() != listing_id
                || receipt.avionics_model_id() != target_id
                || !valid_sha256(receipt.resolution_sha256())
            {
                return Err(ReviewError::Validation(format!(
                    "same-case grounded authorization for catalog id {target_id} is not bound to this listing and product"
                )));
            }
        }
    }
    match link.configuration_action.as_str() {
        "installed"
            if link.replaces_avionics_model_id.is_none()
                && link.expected_replacement_collision_closure_sha256.is_none()
                && link.replacement_authorization.is_none() => {}
        "replaces" | "removes"
            if link.replaces_avionics_model_id.is_some_and(|id| id > 0)
                && link.expected_replacement_collision_closure_sha256.is_some()
                && link.replacement_authorization.is_some() => {}
        _ => {
            return Err(ReviewError::Validation(format!(
                "accepted avionics catalog id {} has invalid action/target semantics",
                link.avionics_model_id
            )))
        }
    }
    Ok(())
}

fn validate_request(
    request: &AutomatedReviewApplyRequest,
) -> ReviewResult<Option<super::SerializedReviewPayload>> {
    if request.listing_id <= 0
        || request.plugin_submission_id <= 0
        || request.expected_faa_snapshot_id <= 0
    {
        return Err(ReviewError::Validation(
            "listing, plugin submission, and FAA snapshot IDs must be positive".to_string(),
        ));
    }
    for (label, value) in [
        (
            "expected_review_payload_sha256",
            request.expected_review_payload_sha256.as_str(),
        ),
        (
            "expected_rendered_html_sha256",
            request.expected_rendered_html_sha256.as_str(),
        ),
        (
            "expected_faa_source_record_sha256",
            request.expected_faa_source_record_sha256.as_str(),
        ),
    ] {
        if !valid_sha256(value) {
            return Err(ReviewError::Validation(format!(
                "{label} must be lowercase SHA-256 hex"
            )));
        }
    }
    for link in &request.accepted_links {
        validate_automated_avionics_link(request.listing_id, link)?;
    }
    if request.residual_aspects.is_empty() {
        Ok(None)
    } else {
        serialize_review_payload(&request.residual_aspects).map(Some)
    }
}

fn graph_key(
    manufacturer_identity_id: Option<i64>,
    product_key: Option<&str>,
    model_id: i64,
) -> ReviewResult<String> {
    let manufacturer_identity_id = manufacturer_identity_id.ok_or_else(|| {
        ReviewError::Stale(format!(
            "approved avionics catalog id {model_id} has no stable manufacturer identity"
        ))
    })?;
    let product_key = product_key.ok_or_else(|| {
        ReviewError::Stale(format!(
            "approved avionics catalog id {model_id} has no stable product identity"
        ))
    })?;
    approved_avionics_product_key(manufacturer_identity_id, product_key).map_err(ReviewError::Stale)
}

fn merge_compatible(existing: &mut PreparedLink, incoming: PreparedLink) -> ReviewResult<()> {
    if existing.avionics_model_id != incoming.avionics_model_id {
        return Err(ReviewError::Conflict(format!(
            "approved avionics catalog ids {} and {} share one canonical graph identity",
            existing.avionics_model_id, incoming.avionics_model_id
        )));
    }
    if existing.configuration_action != incoming.configuration_action
        || existing.replacement_key != incoming.replacement_key
        || existing.replaces_avionics_model_id != incoming.replaces_avionics_model_id
        || existing.authorization != incoming.authorization
        || existing.replacement_authorization != incoming.replacement_authorization
    {
        return Err(ReviewError::Validation(format!(
            "avionics catalog id {} has conflicting installation actions or replacement targets",
            existing.avionics_model_id
        )));
    }
    existing.quantity = existing.quantity.max(incoming.quantity);
    existing.source_notes = merged_notes(
        existing.source_notes.as_deref(),
        incoming.source_notes.as_deref(),
    );
    existing.source_confidence = conservative_confidence(
        existing.source_confidence.as_deref(),
        incoming.source_confidence.as_deref(),
    );
    if existing.source != incoming.source
        && (existing.source == "listing_review" || incoming.source == "listing_review")
    {
        // This can only happen while coalescing two already-persisted,
        // reviewer-confirmed links. Automated input itself is always listing.
        existing.source = "listing_review".to_string();
    }
    Ok(())
}

fn prepared_action(link: &PreparedLink) -> CanonicalAvionicsAction {
    CanonicalAvionicsAction::new(
        link.subject_key.clone(),
        link.configuration_action.clone(),
        link.replacement_key.clone(),
    )
}

fn persisted_values_match(existing: &ExistingLinkRow, assignment: &PreparedLink) -> bool {
    existing.avionics_model_id == assignment.avionics_model_id
        && existing.quantity == assignment.quantity
        && existing.source == assignment.source
        && existing.source_notes == assignment.source_notes
        && existing.source_confidence == assignment.source_confidence
        && existing.configuration_action == assignment.configuration_action
        && existing.replaces_avionics_model_id == assignment.replaces_avionics_model_id
}

fn current_same_case_authorization(
    listing_id: i64,
    link: &ExistingLinkRow,
    role: ListingAssociationRole,
    target_id: i64,
    authorizations: &[ExistingSameCaseAuthorizationRow],
    catalog_product_fingerprints: &std::collections::HashMap<i64, String>,
    active_collision_catalog_rows: &[ActiveCollisionCatalogFingerprintRow],
) -> bool {
    let role_label = match role {
        ListingAssociationRole::Installed => "installed",
        ListingAssociationRole::Replacement => "replacement",
    };
    let Some(authorization) = authorizations.iter().find(|authorization| {
        authorization.listing_link_id == link.id
            && authorization.association_role == role_label
            && authorization.avionics_model_id == target_id
    }) else {
        return false;
    };
    if authorization.policy_version != ASSOCIATION_AUTHORIZATION_POLICY_VERSION
        || !authorization.evidence_capture_is_current
        || !authorization
            .grounded_resolution_sha256
            .as_deref()
            .is_some_and(valid_sha256)
        || catalog_product_fingerprints.get(&target_id) != Some(&authorization.product_fingerprint)
        || fingerprint_grounded_collision_closure(active_collision_catalog_rows, target_id)
            .as_deref()
            != Some(authorization.collision_closure_sha256.as_str())
    {
        return false;
    }
    authorization.observation_sha256
        == association_observation_sha256_from_values(
            listing_id,
            link.id,
            role,
            target_id,
            link.avionics_model_id,
            link.replaces_avionics_model_id,
            link.quantity,
            &link.configuration_action,
            link.source_notes.as_deref().unwrap_or_default(),
        )
}

fn serial_is_compatible(
    listing_serial: Option<&str>,
    faa_serial_raw: Option<&str>,
    faa_serial_key: Option<&str>,
) -> bool {
    let listing_serial = listing_serial
        .map(str::trim)
        .filter(|serial| !serial.is_empty());
    let Some(listing_serial) = listing_serial else {
        return true;
    };
    let faa_serial_raw = faa_serial_raw
        .map(str::trim)
        .filter(|serial| !serial.is_empty());
    let Some(faa_serial_raw) = faa_serial_raw else {
        return true;
    };
    let Some(listing_key) = normalize_serial_key(listing_serial) else {
        return false;
    };
    let Some(recomputed_faa_key) = normalize_serial_key(faa_serial_raw) else {
        return false;
    };
    faa_serial_key.is_some_and(|stored| stored == recomputed_faa_key && stored == listing_key)
}

const EXISTING_LISTING_LINKS_SQL: &str = r#"
    SELECT
      link.id,
      link.avionics_model_id,
      link.quantity,
      link.source,
      link.source_notes,
      link.source_confidence,
      link.configuration_action,
      link.replaces_avionics_model_id,
      installed.catalog_status AS installed_catalog_status,
      installed_identity.avionics_manufacturer_identity_id
        AS installed_manufacturer_identity_id,
      installed_identity.canonical_product_key AS installed_product_key,
      replacement.catalog_status AS replacement_catalog_status,
      replacement_identity.avionics_manufacturer_identity_id
        AS replacement_manufacturer_identity_id,
      replacement_identity.canonical_product_key AS replacement_product_key
    FROM aircraft_sale_listing_avionics link
    LEFT JOIN avionics_models installed
      ON installed.id = link.avionics_model_id
    LEFT JOIN avionics_approved_product_graph_identities installed_identity
      ON installed_identity.avionics_model_id = link.avionics_model_id
    LEFT JOIN avionics_models replacement
      ON replacement.id = link.replaces_avionics_model_id
    LEFT JOIN avionics_approved_product_graph_identities replacement_identity
      ON replacement_identity.avionics_model_id = link.replaces_avionics_model_id
    WHERE link.aircraft_sale_listing_id = ?
    ORDER BY link.id
"#;

const EXISTING_SAME_CASE_AUTHORIZATIONS_SQLITE_SQL: &str = r#"
    SELECT
      authorization.listing_link_id,
      authorization.association_role,
      authorization.avionics_model_id,
      authorization.observation_sha256,
      authorization.product_fingerprint,
      authorization.grounded_resolution_sha256,
      EXISTS (
        SELECT 1 FROM plugin_submissions capture
        WHERE capture.canonical_listing_id = link.aircraft_sale_listing_id
          AND capture.rendered_html_sha256 = authorization.evidence_capture_sha256
          AND length(trim(COALESCE(link.source_notes, ''))) > 0
          AND instr(capture.rendered_html, link.source_notes) > 0
      ) AS evidence_capture_is_current,
      authorization.collision_closure_sha256,
      authorization.policy_version
    FROM aircraft_sale_listing_avionics_authorizations authorization
    JOIN aircraft_sale_listing_avionics link
      ON link.id = authorization.listing_link_id
    WHERE link.aircraft_sale_listing_id = ?
      AND authorization.authorization_kind = 'same_case_grounded'
"#;

const EXISTING_SAME_CASE_AUTHORIZATIONS_POSTGRES_SQL: &str = r#"
    SELECT
      authorization.listing_link_id,
      authorization.association_role,
      authorization.avionics_model_id,
      authorization.observation_sha256,
      authorization.product_fingerprint,
      authorization.grounded_resolution_sha256,
      EXISTS (
        SELECT 1 FROM plugin_submissions capture
        WHERE capture.canonical_listing_id = link.aircraft_sale_listing_id
          AND capture.rendered_html_sha256 = authorization.evidence_capture_sha256
          AND length(BTRIM(COALESCE(link.source_notes, ''))) > 0
          AND position(link.source_notes IN capture.rendered_html) > 0
      ) AS evidence_capture_is_current,
      authorization.collision_closure_sha256,
      authorization.policy_version
    FROM aircraft_sale_listing_avionics_authorizations authorization
    JOIN aircraft_sale_listing_avionics link
      ON link.id = authorization.listing_link_id
    WHERE link.aircraft_sale_listing_id = ?
      AND authorization.authorization_kind = 'same_case_grounded'
"#;

fn preserved_link_is_eligible(link: &ExistingLinkRow) -> bool {
    link.quantity > 0
        && link.source_confidence.as_deref() == Some("high")
        && matches!(link.source.as_str(), "listing" | "listing_review")
        && link.installed_catalog_status.as_deref() == Some("approved")
        && match link.configuration_action.as_str() {
            "installed" => link.replaces_avionics_model_id.is_none(),
            "replaces" | "removes" => {
                link.replaces_avionics_model_id.is_some()
                    && link.replacement_catalog_status.as_deref() == Some("approved")
            }
            _ => false,
        }
}

#[allow(clippy::too_many_arguments)]
fn validate_preserved_link_authorizations(
    listing_id: i64,
    link: &ExistingLinkRow,
    installed_reuse_is_current: bool,
    replacement_reuse_is_current: bool,
    authorizations: &[ExistingSameCaseAuthorizationRow],
    catalog_product_fingerprints: &std::collections::HashMap<i64, String>,
    active_collision_catalog_rows: &[ActiveCollisionCatalogFingerprintRow],
) -> ReviewResult<()> {
    let installed_same_case_is_current = current_same_case_authorization(
        listing_id,
        link,
        ListingAssociationRole::Installed,
        link.avionics_model_id,
        authorizations,
        catalog_product_fingerprints,
        active_collision_catalog_rows,
    );
    if !installed_reuse_is_current && !installed_same_case_is_current {
        return Err(ReviewError::Stale(format!(
            "preserved avionics catalog id {} has neither current manufacturer-reuse nor same-case grounded authorization",
            link.avionics_model_id
        )));
    }
    if let Some(target_id) = link.replaces_avionics_model_id {
        let replacement_same_case_is_current = current_same_case_authorization(
            listing_id,
            link,
            ListingAssociationRole::Replacement,
            target_id,
            authorizations,
            catalog_product_fingerprints,
            active_collision_catalog_rows,
        );
        if !replacement_reuse_is_current && !replacement_same_case_is_current {
            return Err(ReviewError::Stale(format!(
                "preserved replacement catalog id {target_id} has neither current manufacturer-reuse nor same-case grounded authorization"
            )));
        }
    }
    Ok(())
}

/// Find a deterministic blocker among existing links that no pending paid
/// candidate can replace. This is deliberately narrower than the final apply
/// transaction: keys inside `candidate_graph_keys` are skipped because a paid
/// result may legitimately touch or repair them. The transaction remains the
/// authoritative complete graph and concurrency check.
pub(crate) async fn unrelated_preserved_avionics_blocker(
    db: &AppDb,
    listing_id: i64,
    candidate_graph_keys: &BTreeSet<String>,
) -> ReviewResult<Option<String>> {
    if listing_id <= 0 {
        return Err(ReviewError::Validation(
            "listing id must be positive when validating preserved avionics".to_string(),
        ));
    }
    let existing_links_sql = db.sql(EXISTING_LISTING_LINKS_SQL);
    let same_case_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(EXISTING_SAME_CASE_AUTHORIZATIONS_SQLITE_SQL),
        DatabaseBackend::Postgres(_) => db.sql(EXISTING_SAME_CASE_AUTHORIZATIONS_POSTGRES_SQL),
    };
    let catalog_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let active_collision_catalog_sql = db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
    let (existing_rows, authorizations, catalog_rows, active_collision_catalog_rows) =
        match db.backend() {
            DatabaseBackend::Sqlite(pool) => (
                sqlx::query_as::<_, ExistingLinkRow>(&existing_links_sql)
                    .bind(listing_id)
                    .fetch_all(pool)
                    .await?,
                sqlx::query_as::<_, ExistingSameCaseAuthorizationRow>(&same_case_sql)
                    .bind(listing_id)
                    .fetch_all(pool)
                    .await?,
                sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_sql)
                    .fetch_all(pool)
                    .await?,
                sqlx::query_as::<_, ActiveCollisionCatalogFingerprintRow>(
                    &active_collision_catalog_sql,
                )
                .fetch_all(pool)
                .await?,
            ),
            DatabaseBackend::Postgres(pool) => (
                sqlx::query_as::<_, ExistingLinkRow>(&existing_links_sql)
                    .bind(listing_id)
                    .fetch_all(pool)
                    .await?,
                sqlx::query_as::<_, ExistingSameCaseAuthorizationRow>(&same_case_sql)
                    .bind(listing_id)
                    .fetch_all(pool)
                    .await?,
                sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_sql)
                    .fetch_all(pool)
                    .await?,
                sqlx::query_as::<_, ActiveCollisionCatalogFingerprintRow>(
                    &active_collision_catalog_sql,
                )
                .fetch_all(pool)
                .await?,
            ),
        };
    let catalog_product_fingerprints =
        catalog_product_fingerprints(&catalog_products(catalog_rows));
    for link in existing_rows
        .iter()
        .filter(|link| preserved_link_is_eligible(link))
    {
        let subject_key = match graph_key(
            link.installed_manufacturer_identity_id,
            link.installed_product_key.as_deref(),
            link.avionics_model_id,
        ) {
            Ok(key) => key,
            Err(error) => return Ok(Some(error.to_string())),
        };
        let replacement_key = if let Some(target_id) = link.replaces_avionics_model_id {
            match graph_key(
                link.replacement_manufacturer_identity_id,
                link.replacement_product_key.as_deref(),
                target_id,
            ) {
                Ok(key) => Some(key),
                Err(error) => return Ok(Some(error.to_string())),
            }
        } else {
            None
        };
        if candidate_graph_keys.contains(&subject_key)
            || replacement_key
                .as_ref()
                .is_some_and(|key| candidate_graph_keys.contains(key))
        {
            continue;
        }
        let installed_reuse_is_current =
            product_reuse_attestation_is_current(db, link.avionics_model_id).await?;
        let replacement_reuse_is_current = match link.replaces_avionics_model_id {
            Some(target_id) => product_reuse_attestation_is_current(db, target_id).await?,
            None => false,
        };
        if let Err(error) = validate_preserved_link_authorizations(
            listing_id,
            link,
            installed_reuse_is_current,
            replacement_reuse_is_current,
            &authorizations,
            &catalog_product_fingerprints,
            &active_collision_catalog_rows,
        ) {
            return Ok(Some(error.to_string()));
        }
    }
    Ok(None)
}

/// Applies one API-produced automated review result atomically.
///
/// Accepted links are not reviewer decisions: their source remains `listing`
/// and the supplied source confidence is never upgraded. The listing remains
/// unpublished after this call; residual aspects leave it `pending_review`,
/// while an all-pass result returns it to `incomplete` for the ordinary
/// finalizer.
pub(crate) async fn apply_automated_avionics_review(
    db: &AppDb,
    request: &AutomatedReviewApplyRequest,
) -> ReviewResult<AutomatedReviewApplyResult> {
    let serialized_residual = validate_request(request)?;

    let guard_base = r#"
        SELECT
          listing.created_by_user_id AS owner_user_id,
          listing.source_url AS listing_source_url,
          listing.registration_number,
          listing.serial_number,
          listing.ingestion_state,
          listing.is_verified,
          review.pending_aspect_count,
          review.review_payload_json,
          review.review_payload_sha256,
          review.plugin_submission_id AS attached_plugin_submission_id,
          submission.user_id AS submission_user_id,
          submission.source_url AS submission_source_url,
          submission.canonical_listing_id AS submission_canonical_listing_id,
          submission.rendered_html,
          submission.rendered_html_sha256
        FROM aircraft_sale_listings listing
        JOIN aircraft_sale_listing_pending_reviews review
          ON review.listing_id = listing.id
        JOIN plugin_submissions submission
          ON submission.id = ?
        WHERE listing.id = ?
    "#;
    let postgres_guard = format!("{guard_base} FOR UPDATE");
    let review_select = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(guard_base),
        DatabaseBackend::Postgres(_) => db.sql(&postgres_guard),
    };
    let lock_faa = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql("SELECT 1"),
        DatabaseBackend::Postgres(_) => db.sql(
            "LOCK TABLE faa_registry_snapshots, faa_registry_coverage, faa_registry_aircraft IN SHARE ROW EXCLUSIVE MODE",
        ),
    };
    let latest_release = db.sql(
        r#"
        SELECT snapshot_date, archive_sha256
        FROM faa_registry_snapshots
        ORDER BY snapshot_date DESC, id DESC
        LIMIT 1
        "#,
    );
    let covering_snapshot = db.sql(
        r#"
        SELECT snapshot.id
        FROM faa_registry_snapshots snapshot
        JOIN faa_registry_coverage coverage
          ON coverage.snapshot_id = snapshot.id
         AND coverage.n_number = ?
        WHERE snapshot.snapshot_date = ?
          AND snapshot.archive_sha256 = ?
        ORDER BY (
          SELECT COUNT(*) FROM faa_registry_coverage target
          WHERE target.snapshot_id = snapshot.id
        ) DESC, snapshot.id DESC
        LIMIT 1
        "#,
    );
    let select_faa_record = db.sql(
        r#"
        SELECT
          aircraft.manufacturer_serial_raw,
          aircraft.manufacturer_serial_key,
          aircraft.source_record_sha256
        FROM faa_registry_coverage coverage
        JOIN faa_registry_aircraft aircraft
          ON aircraft.snapshot_id = coverage.snapshot_id
         AND aircraft.n_number = coverage.n_number
        WHERE coverage.snapshot_id = ?
          AND coverage.n_number = ?
          AND coverage.lookup_status = 'matched'
        "#,
    );
    let lock_catalog = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models WHERE catalog_status = 'approved' ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            "LOCK TABLE avionics_models, avionics_model_types, avionics_types, avionics_manufacturers, avionics_manufacturer_identities, avionics_manufacturer_identity_memberships, avionics_manufacturer_identity_merges, avionics_approved_product_identities, avionics_product_reuse_attestations, avionics_authoritative_source_origins, avionics_authoritative_source_origin_revocations IN SHARE ROW EXCLUSIVE MODE",
        ),
    };
    let lock_listing_children = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql("SELECT 1"),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_LISTING_CHILD_LOCK_SQL),
    };
    let catalog_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let select_graph_identity = db.sql(
        r#"
        SELECT
          graph.avionics_manufacturer_identity_id,
          graph.canonical_product_key,
          manufacturer.name,
          model.name
        FROM avionics_models model
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        JOIN avionics_approved_product_graph_identities graph
          ON graph.avionics_model_id = model.id
        WHERE model.id = ?
          AND model.catalog_status = 'approved'
        "#,
    );
    let select_existing_links = db.sql(EXISTING_LISTING_LINKS_SQL);
    let select_existing_same_case_authorizations = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(EXISTING_SAME_CASE_AUTHORIZATIONS_SQLITE_SQL),
        DatabaseBackend::Postgres(_) => db.sql(EXISTING_SAME_CASE_AUTHORIZATIONS_POSTGRES_SQL),
    };
    let active_collision_catalog_sql = db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
    let delete_link = db.sql(
        "DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? AND id = ?",
    );
    let insert_link = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics (
          aircraft_sale_listing_id,
          avionics_model_id,
          quantity,
          source,
          source_notes,
          source_confidence,
          configuration_action,
          replaces_avionics_model_id
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    );
    let delete_authorization = db.sql(
        r#"
        DELETE FROM aircraft_sale_listing_avionics_authorizations
        WHERE listing_link_id = ?
          AND association_role = ?
        "#,
    );
    let insert_reuse_authorization = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics_authorizations (
          listing_link_id,
          association_role,
          avionics_model_id,
          authorization_kind,
          observation_sha256,
          product_fingerprint,
          grounded_resolution_sha256,
          evidence_capture_sha256,
          collision_closure_sha256,
          policy_version
        )
        SELECT ?, ?, ?, 'manufacturer_reuse', ?, attestation.product_fingerprint,
               NULL, ?, ?, ?
        FROM avionics_product_reuse_attestations attestation
        WHERE attestation.avionics_model_id = ?
        "#,
    );
    let insert_grounded_authorization = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics_authorizations (
          listing_link_id,
          association_role,
          avionics_model_id,
          authorization_kind,
          observation_sha256,
          product_fingerprint,
          grounded_resolution_sha256,
          evidence_capture_sha256,
          collision_closure_sha256,
          policy_version
        ) VALUES (?, ?, ?, 'same_case_grounded', ?, ?, ?, ?, ?, ?)
        "#,
    );
    let update_review = db.sql(
        r#"
        UPDATE aircraft_sale_listing_pending_reviews
        SET plugin_submission_id = ?,
            extraction_sha256 = ?,
            catalog_revision_sha256 = ?,
            pending_aspect_count = ?,
            review_payload_json = ?,
            review_payload_sha256 = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE listing_id = ?
          AND review_payload_sha256 = ?
        "#,
    );
    let delete_review = db.sql(
        r#"
        DELETE FROM aircraft_sale_listing_pending_reviews
        WHERE listing_id = ?
          AND review_payload_sha256 = ?
        "#,
    );
    let mark_pending = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'pending_review',
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND ingestion_state = 'pending_review'
          AND is_verified = FALSE
        "#,
    );
    let mark_incomplete = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'incomplete',
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND ingestion_state = 'pending_review'
          AND is_verified = FALSE
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_reviews review
            WHERE review.listing_id = aircraft_sale_listings.id
          )
        "#,
    );

    macro_rules! apply_in_transaction {
        ($pool:expr, $reuse_is_current:path) => {{
            let mut transaction = $pool.begin().await?;
            if matches!(db.backend(), DatabaseBackend::Postgres(_)) {
                // Match every other PostgreSQL listing mutation: mutable
                // catalog/source state, FAA source state, child tables, rows.
                sqlx::query(&lock_catalog)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query(&lock_faa)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query(&lock_listing_children)
                    .execute(&mut *transaction)
                    .await?;
            }
            let guard = sqlx::query_as::<_, AutomationGuardRow>(&review_select)
                .bind(request.plugin_submission_id)
                .bind(request.listing_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "listing {} no longer has the expected pending review and retained submission",
                        request.listing_id
                    ))
                })?;
            if guard.ingestion_state != "pending_review" || guard.is_verified {
                return Err(ReviewError::Stale(format!(
                    "listing {} is no longer in its expected pending-review state",
                    request.listing_id
                )));
            }
            if guard.review_payload_sha256 != request.expected_review_payload_sha256 {
                return Err(ReviewError::Stale(
                    "review payload changed while automated review was running".to_string(),
                ));
            }
            // Verify the stored payload itself rather than trusting only its
            // optimistic-lock column.
            parse_payload(
                &guard.review_payload_json,
                Some(&guard.review_payload_sha256),
                guard.pending_aspect_count,
            )?;
            let submission_is_bound = guard.submission_canonical_listing_id
                == Some(request.listing_id)
                || (guard.submission_canonical_listing_id.is_none()
                    && !guard.submission_source_url.trim().is_empty()
                    && guard
                        .listing_source_url
                        .as_deref()
                        .is_some_and(|url| !url.trim().is_empty())
                    && guard.listing_source_url.as_deref()
                        == Some(guard.submission_source_url.as_str()));
            if guard.attached_plugin_submission_id != Some(request.plugin_submission_id)
                || guard.submission_user_id != guard.owner_user_id
                || !submission_is_bound
            {
                return Err(ReviewError::Stale(
                    "pending review is no longer bound to the exact retained listing submission"
                        .to_string(),
                ));
            }
            if guard.rendered_html_sha256 != request.expected_rendered_html_sha256
                || sha256_hex(guard.rendered_html.as_bytes())
                    != request.expected_rendered_html_sha256
            {
                return Err(ReviewError::Stale(
                    "retained listing HTML changed or failed its content hash".to_string(),
                ));
            }
            if !request.accepted_links.is_empty()
                && guard.submission_canonical_listing_id != Some(request.listing_id)
            {
                return Err(ReviewError::Stale(
                    "accepted listing associations require the retained submission to be bound to the exact canonical listing"
                        .to_string(),
                ));
            }
            for link in &request.accepted_links {
                let evidence_text = link.source_notes.as_deref().ok_or_else(|| {
                    ReviewError::Validation(format!(
                        "automated acceptance for avionics catalog id {} requires exact listing evidence",
                        link.avionics_model_id
                    ))
                })?;
                validate_exact_listing_evidence_span(&guard.rendered_html, evidence_text)
                    .map_err(ReviewError::Validation)?;
            }

            let registration = guard
                .registration_number
                .as_deref()
                .and_then(normalize_n_number)
                .ok_or_else(|| {
                    ReviewError::Stale(
                        "listing no longer has a valid N-number for FAA grounding".to_string(),
                    )
                })?;
            if matches!(db.backend(), DatabaseBackend::Sqlite(_)) {
                sqlx::query(&lock_faa)
                    .execute(&mut *transaction)
                    .await?;
            }
            let (latest_snapshot_date, latest_archive_sha256): (String, String) =
                sqlx::query_as(&latest_release)
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or_else(|| {
                        ReviewError::Stale(
                            "FAA registry grounding is unavailable".to_string(),
                        )
                    })?;
            let current_covering_snapshot_id: i64 =
                sqlx::query_scalar(&covering_snapshot)
                    .bind(registration.as_str())
                    .bind(latest_snapshot_date.as_str())
                    .bind(latest_archive_sha256.as_str())
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "latest FAA release does not cover {registration}"
                        ))
                    })?;
            if current_covering_snapshot_id != request.expected_faa_snapshot_id {
                return Err(ReviewError::Stale(
                    "FAA registry grounding changed while automated review was running"
                        .to_string(),
                ));
            }
            let faa_record = sqlx::query_as::<_, FaaRecordRow>(&select_faa_record)
                .bind(request.expected_faa_snapshot_id)
                .bind(registration.as_str())
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "expected current FAA record for {registration} is missing"
                    ))
                })?;
            if faa_record.source_record_sha256 != request.expected_faa_source_record_sha256 {
                return Err(ReviewError::Stale(
                    "FAA source record changed while automated review was running".to_string(),
                ));
            }
            if !serial_is_compatible(
                guard.serial_number.as_deref(),
                faa_record.manufacturer_serial_raw.as_deref(),
                faa_record.manufacturer_serial_key.as_deref(),
            ) {
                return Err(ReviewError::Stale(
                    "listing serial number conflicts with the current FAA record".to_string(),
                ));
            }

            // SQLite retains its original deferred-read behavior and obtains
            // the single writer lock only at the prior mutation boundary.
            if matches!(db.backend(), DatabaseBackend::Sqlite(_)) {
                sqlx::query(&lock_catalog)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query(&lock_listing_children)
                    .execute(&mut *transaction)
                    .await?;
            }
            let catalog_rows = sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let catalog_products = catalog_products(catalog_rows);
            let catalog_product_fingerprints =
                catalog_product_fingerprints(&catalog_products);
            let catalog_revision_sha256 = fingerprint_catalog_products(&catalog_products);

            let mut identity_cache = BTreeMap::<i64, CatalogGraphIdentity>::new();
            let mut required_model_ids = BTreeSet::new();
            let mut authorization_by_model = BTreeMap::new();
            for link in &request.accepted_links {
                required_model_ids.insert(link.avionics_model_id);
                if authorization_by_model
                    .insert(link.avionics_model_id, link.authorization.clone())
                    .is_some_and(|existing| existing != link.authorization)
                {
                    return Err(ReviewError::Conflict(format!(
                        "catalog id {} has conflicting automatic authorization proofs",
                        link.avionics_model_id
                    )));
                }
                if let Some(target) = link.replaces_avionics_model_id {
                    required_model_ids.insert(target);
                    let replacement_authorization = link
                        .replacement_authorization
                        .as_ref()
                        .expect("validated replacement has an authorization")
                        .clone();
                    if authorization_by_model
                        .insert(target, replacement_authorization.clone())
                        .is_some_and(|existing| existing != replacement_authorization)
                    {
                        return Err(ReviewError::Conflict(format!(
                            "replacement catalog id {target} has conflicting automatic authorization proofs"
                        )));
                    }
                }
            }
            let active_collision_catalog_rows =
                sqlx::query_as::<_, ActiveCollisionCatalogFingerprintRow>(
                    &active_collision_catalog_sql,
                )
                .fetch_all(&mut *transaction)
                .await?;
            let mut collision_member_ids = BTreeSet::new();
            for model_id in &required_model_ids {
                let members = active_collision_closure_member_ids(
                    &active_collision_catalog_rows,
                    *model_id,
                )
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "accepted avionics catalog id {model_id} has no unique active collision-closure identity"
                    ))
                })?;
                collision_member_ids.extend(members);
            }
            let mut current_reuse_attested_ids = HashSet::new();
            for model_id in collision_member_ids {
                if $reuse_is_current(db, &mut transaction, model_id).await? {
                    current_reuse_attested_ids.insert(model_id);
                }
            }
            let mut collision_closures = BTreeMap::new();
            for model_id in &required_model_ids {
                let authorization = authorization_by_model
                    .get(model_id)
                    .expect("required model authorization was collected");
                let collision_closure = match authorization {
                    AutomatedAssociationAuthorization::ManufacturerReuse => {
                        if !current_reuse_attested_ids.contains(model_id) {
                            return Err(ReviewError::Stale(format!(
                                "accepted avionics catalog id {model_id} lost its manufacturer-primary reuse authorization"
                            )));
                        }
                        fingerprint_active_collision_closure(
                            &active_collision_catalog_rows,
                            &current_reuse_attested_ids,
                            *model_id,
                        )
                    }
                    AutomatedAssociationAuthorization::SameCaseGrounded(_) => {
                        fingerprint_grounded_collision_closure(
                            &active_collision_catalog_rows,
                            *model_id,
                        )
                    }
                }
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "accepted avionics catalog id {model_id} has no unique active collision-closure identity"
                    ))
                })?;
                collision_closures.insert(*model_id, collision_closure);
                let identity: Option<(i64, String, String, String)> =
                    sqlx::query_as(&select_graph_identity)
                        .bind(model_id)
                        .fetch_optional(&mut *transaction)
                        .await?;
                let (manufacturer_identity_id, product_key, manufacturer, model) =
                    identity.ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "accepted avionics catalog id {model_id} is missing, unapproved, or lacks a stable graph identity"
                        ))
                    })?;
                identity_cache.insert(
                    *model_id,
                    CatalogGraphIdentity {
                        key: approved_avionics_product_key(
                            manufacturer_identity_id,
                            &product_key,
                        )
                        .map_err(ReviewError::Stale)?,
                        manufacturer,
                        model,
                    },
                );
            }
            for link in &request.accepted_links {
                if collision_closures.get(&link.avionics_model_id)
                    != Some(&link.expected_collision_closure_sha256)
                {
                    return Err(ReviewError::Stale(format!(
                        "active avionics collision catalog changed after automatic identity resolution for catalog id {}",
                        link.avionics_model_id
                    )));
                }
                if let Some(target_id) = link.replaces_avionics_model_id {
                    let expected = link
                        .expected_replacement_collision_closure_sha256
                        .as_ref()
                        .expect("replacement validation requires its collision revision");
                    if collision_closures.get(&target_id) != Some(expected) {
                        return Err(ReviewError::Stale(format!(
                            "active avionics collision catalog changed after automatic identity resolution for replacement catalog id {target_id}"
                        )));
                    }
                }
            }

            let mut accepted = BTreeMap::<String, PreparedLink>::new();
            let mut touched_keys = BTreeSet::new();
            for link in &request.accepted_links {
                let subject = identity_cache
                    .get(&link.avionics_model_id)
                    .expect("accepted subject identity was loaded");
                let replacement_key = link.replaces_avionics_model_id.map(|target| {
                    identity_cache
                        .get(&target)
                        .expect("accepted replacement identity was loaded")
                        .key
                        .clone()
                });
                touched_keys.insert(subject.key.clone());
                touched_keys.extend(replacement_key.iter().cloned());
                let incoming = PreparedLink {
                    avionics_model_id: link.avionics_model_id,
                    authorization: link.authorization.clone(),
                    subject_key: subject.key.clone(),
                    quantity: link.quantity,
                    source: "listing".to_string(),
                    source_notes: link.source_notes.clone(),
                    // Validation requires high, but persist the exact supplied
                    // value rather than manufacturing corroboration here.
                    source_confidence: link.source_confidence.clone(),
                    configuration_action: link.configuration_action.clone(),
                    replaces_avionics_model_id: link.replaces_avionics_model_id,
                    replacement_authorization: link.replacement_authorization.clone(),
                    replacement_key,
                };
                if let Some(existing) = accepted.get_mut(&incoming.subject_key) {
                    merge_compatible(existing, incoming)?;
                } else {
                    accepted.insert(incoming.subject_key.clone(), incoming);
                }
            }
            validate_canonical_avionics_actions(
                &accepted.values().map(prepared_action).collect::<Vec<_>>(),
            )
            .map_err(ReviewError::Validation)?;

            let existing_rows =
                sqlx::query_as::<_, ExistingLinkRow>(&select_existing_links)
                    .bind(request.listing_id)
                    .fetch_all(&mut *transaction)
                    .await?;
            let existing_same_case_authorizations =
                sqlx::query_as::<_, ExistingSameCaseAuthorizationRow>(
                    &select_existing_same_case_authorizations,
                )
                .bind(request.listing_id)
                .fetch_all(&mut *transaction)
                .await?;
            for link in &request.accepted_links {
                let Some(guard) = link.preserved_association_guard.as_ref() else {
                    continue;
                };
                let existing = existing_rows
                    .iter()
                    .find(|row| row.id == guard.listing_link_id)
                    .ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "preserved avionics listing link {} disappeared before automated corroboration",
                            guard.listing_link_id
                        ))
                    })?;
                let target_id = match guard.association_role {
                    ListingAssociationRole::Installed => existing.avionics_model_id,
                    ListingAssociationRole::Replacement => {
                        existing.replaces_avionics_model_id.unwrap_or_default()
                    }
                };
                let actual_observation_sha256 = association_observation_sha256_from_values(
                    request.listing_id,
                    existing.id,
                    guard.association_role,
                    target_id,
                    existing.avionics_model_id,
                    existing.replaces_avionics_model_id,
                    existing.quantity,
                    &existing.configuration_action,
                    existing.source_notes.as_deref().unwrap_or_default(),
                );
                if target_id != link.avionics_model_id
                    || actual_observation_sha256 != guard.expected_observation_sha256
                {
                    return Err(ReviewError::Stale(format!(
                        "preserved avionics listing link {} changed after local evaluation",
                        guard.listing_link_id
                    )));
                }
            }
            let mut preserved = BTreeMap::<String, PreparedLink>::new();
            for row in &existing_rows {
                if !preserved_link_is_eligible(row) {
                    continue;
                }
                let subject_key = graph_key(
                    row.installed_manufacturer_identity_id,
                    row.installed_product_key.as_deref(),
                    row.avionics_model_id,
                )?;
                let replacement_key = if let Some(target_id) =
                    row.replaces_avionics_model_id
                {
                    Some(graph_key(
                        row.replacement_manufacturer_identity_id,
                        row.replacement_product_key.as_deref(),
                        target_id,
                    )?)
                } else {
                    None
                };
                if touched_keys.contains(&subject_key)
                    || replacement_key
                        .as_ref()
                        .is_some_and(|key| touched_keys.contains(key))
                {
                    continue;
                }
                let installed_reuse_is_current =
                    $reuse_is_current(db, &mut transaction, row.avionics_model_id).await?;
                let replacement_reuse_is_current =
                    if let Some(target_id) = row.replaces_avionics_model_id {
                        $reuse_is_current(db, &mut transaction, target_id).await?
                    } else {
                        false
                    };
                validate_preserved_link_authorizations(
                    request.listing_id,
                    row,
                    installed_reuse_is_current,
                    replacement_reuse_is_current,
                    &existing_same_case_authorizations,
                    &catalog_product_fingerprints,
                    &active_collision_catalog_rows,
                )?;
                let incoming = PreparedLink {
                    avionics_model_id: row.avionics_model_id,
                    authorization: AutomatedAssociationAuthorization::ManufacturerReuse,
                    subject_key: subject_key.clone(),
                    quantity: row.quantity,
                    source: row.source.clone(),
                    source_notes: row.source_notes.clone(),
                    source_confidence: row.source_confidence.clone(),
                    configuration_action: row.configuration_action.clone(),
                    replaces_avionics_model_id: row.replaces_avionics_model_id,
                    replacement_authorization: row
                        .replaces_avionics_model_id
                        .map(|_| AutomatedAssociationAuthorization::ManufacturerReuse),
                    replacement_key,
                };
                if let Some(existing) = preserved.get_mut(&subject_key) {
                    merge_compatible(existing, incoming)?;
                } else {
                    preserved.insert(subject_key, incoming);
                }
            }

            let preserved_link_count = preserved.len() as i64;
            let accepted_link_count = accepted.len() as i64;
            let accepted_subject_keys = accepted.keys().cloned().collect::<BTreeSet<_>>();
            preserved.extend(accepted);
            let assignments = preserved;
            validate_canonical_avionics_actions(
                &assignments.values().map(prepared_action).collect::<Vec<_>>(),
            )
            .map_err(ReviewError::Validation)?;
            for subject_key in &accepted_subject_keys {
                let assignment = assignments
                    .get(subject_key)
                    .expect("accepted assignment remains in the complete action graph");
                let evidence_text = assignment.source_notes.as_deref().ok_or_else(|| {
                    ReviewError::Validation(format!(
                        "automated acceptance for avionics catalog id {} requires exact listing evidence",
                        assignment.avionics_model_id
                    ))
                })?;
                let installed_identity = identity_cache
                    .get(&assignment.avionics_model_id)
                    .expect("accepted installed identity was loaded");
                validate_exact_listing_product_evidence(
                    &guard.rendered_html,
                    evidence_text,
                    &installed_identity.manufacturer,
                    &installed_identity.model,
                )
                .map_err(ReviewError::Validation)?;
                if let Some(target_id) = assignment.replaces_avionics_model_id {
                    let replacement_identity = identity_cache
                        .get(&target_id)
                        .expect("accepted replacement identity was loaded");
                    validate_exact_listing_product_evidence(
                        &guard.rendered_html,
                        evidence_text,
                        &replacement_identity.manufacturer,
                        &replacement_identity.model,
                    )
                    .map_err(ReviewError::Validation)?;
                }
            }

            let mut retained_link_ids = BTreeSet::new();
            let mut retained_assignment_keys = BTreeSet::new();
            for (subject_key, assignment) in &assignments {
                if let Some(existing) = existing_rows
                    .iter()
                    .find(|existing| persisted_values_match(existing, assignment))
                {
                    retained_link_ids.insert(existing.id);
                    retained_assignment_keys.insert(subject_key.clone());
                }
            }
            for existing in &existing_rows {
                if retained_link_ids.contains(&existing.id) {
                    continue;
                }
                sqlx::query(&delete_link)
                    .bind(request.listing_id)
                    .bind(existing.id)
                    .execute(&mut *transaction)
                    .await?;
            }
            for (subject_key, assignment) in &assignments {
                if retained_assignment_keys.contains(subject_key) {
                    continue;
                }
                sqlx::query(&insert_link)
                    .bind(request.listing_id)
                    .bind(assignment.avionics_model_id)
                    .bind(assignment.quantity)
                    .bind(assignment.source.as_str())
                    .bind(assignment.source_notes.as_deref())
                    .bind(assignment.source_confidence.as_deref())
                    .bind(assignment.configuration_action.as_str())
                    .bind(assignment.replaces_avionics_model_id)
                    .execute(&mut *transaction)
                    .await?;
            }

            let persisted_rows = sqlx::query_as::<_, ExistingLinkRow>(&select_existing_links)
                .bind(request.listing_id)
                .fetch_all(&mut *transaction)
                .await?;
            for subject_key in &accepted_subject_keys {
                let assignment = assignments
                    .get(subject_key)
                    .expect("accepted assignment remains in the complete action graph");
                let persisted = persisted_rows
                    .iter()
                    .find(|row| persisted_values_match(row, assignment))
                    .ok_or_else(|| {
                        ReviewError::Conflict(format!(
                            "accepted avionics catalog id {} was not persisted exactly",
                            assignment.avionics_model_id
                        ))
                    })?;
                let evidence_text = assignment
                    .source_notes
                    .as_deref()
                    .expect("accepted assignment evidence was validated");
                let mut roles = vec![(
                    ListingAssociationRole::Installed,
                    "installed",
                    assignment.avionics_model_id,
                    &assignment.authorization,
                )];
                if let Some(replacement_id) = assignment.replaces_avionics_model_id {
                    roles.push((
                        ListingAssociationRole::Replacement,
                        "replacement",
                        replacement_id,
                        assignment
                            .replacement_authorization
                            .as_ref()
                            .expect("prepared replacement has an authorization"),
                    ));
                }
                for (role, role_label, target_id, authorization) in roles {
                    let observation_sha256 = association_observation_sha256_from_values(
                        request.listing_id,
                        persisted.id,
                        role,
                        target_id,
                        assignment.avionics_model_id,
                        assignment.replaces_avionics_model_id,
                        assignment.quantity,
                        &assignment.configuration_action,
                        evidence_text,
                    );
                    sqlx::query(&delete_authorization)
                        .bind(persisted.id)
                        .bind(role_label)
                        .execute(&mut *transaction)
                        .await?;
                    let collision_closure = collision_closures
                        .get(&target_id)
                        .expect("accepted target collision closure was loaded");
                    let inserted = match authorization {
                        AutomatedAssociationAuthorization::ManufacturerReuse => {
                            sqlx::query(&insert_reuse_authorization)
                                .bind(persisted.id)
                                .bind(role_label)
                                .bind(target_id)
                                .bind(observation_sha256.as_str())
                                .bind(request.expected_rendered_html_sha256.as_str())
                                .bind(collision_closure)
                                .bind(ASSOCIATION_AUTHORIZATION_POLICY_VERSION)
                                .bind(target_id)
                                .execute(&mut *transaction)
                                .await?
                                .rows_affected()
                        }
                        AutomatedAssociationAuthorization::SameCaseGrounded(receipt) => {
                            let product_fingerprint = catalog_product_fingerprints
                                .get(&target_id)
                                .ok_or_else(|| {
                                    ReviewError::Stale(format!(
                                        "grounded catalog id {target_id} lost its approved product fingerprint"
                                    ))
                                })?;
                            sqlx::query(&insert_grounded_authorization)
                                .bind(persisted.id)
                                .bind(role_label)
                                .bind(target_id)
                                .bind(observation_sha256.as_str())
                                .bind(product_fingerprint)
                                .bind(receipt.resolution_sha256())
                                .bind(request.expected_rendered_html_sha256.as_str())
                                .bind(collision_closure)
                                .bind(ASSOCIATION_AUTHORIZATION_POLICY_VERSION)
                                .execute(&mut *transaction)
                                .await?
                                .rows_affected()
                        }
                    };
                    if inserted != 1 {
                        return Err(ReviewError::Conflict(format!(
                            "accepted avionics catalog id {target_id} lost its exact association authorization before commit"
                        )));
                    }
                }
            }

            let (review_payload_sha256, stored_catalog_revision, ingestion_state) =
                if let Some(serialized) = &serialized_residual {
                    let changed = sqlx::query(&update_review)
                        .bind(request.plugin_submission_id)
                        .bind(serialized.extraction_sha256.as_str())
                        .bind(catalog_revision_sha256.as_str())
                        .bind(serialized.pending_aspect_count)
                        .bind(serialized.review_payload_json.as_str())
                        .bind(serialized.review_payload_sha256.as_str())
                        .bind(request.listing_id)
                        .bind(request.expected_review_payload_sha256.as_str())
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if changed != 1 {
                        return Err(ReviewError::Stale(
                            "pending review changed before residual aspects were staged"
                                .to_string(),
                        ));
                    }
                    let changed = sqlx::query(&mark_pending)
                        .bind(request.listing_id)
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if changed != 1 {
                        return Err(ReviewError::Stale(
                            "listing state changed before residual aspects were staged"
                                .to_string(),
                        ));
                    }
                    (
                        Some(serialized.review_payload_sha256.clone()),
                        Some(catalog_revision_sha256.clone()),
                        "pending_review".to_string(),
                    )
                } else {
                    let changed = sqlx::query(&delete_review)
                        .bind(request.listing_id)
                        .bind(request.expected_review_payload_sha256.as_str())
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if changed != 1 {
                        return Err(ReviewError::Stale(
                            "pending review changed before it could be cleared".to_string(),
                        ));
                    }
                    let changed = sqlx::query(&mark_incomplete)
                        .bind(request.listing_id)
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if changed != 1 {
                        return Err(ReviewError::Stale(
                            "listing state changed before automated review completed".to_string(),
                        ));
                    }
                    (None, None, "incomplete".to_string())
                };

            transaction.commit().await?;
            Ok::<AutomatedReviewApplyResult, ReviewError>(
                AutomatedReviewApplyResult {
                    listing_id: request.listing_id,
                    plugin_submission_id: request.plugin_submission_id,
                    accepted_link_count,
                    preserved_link_count,
                    stored_link_count: assignments.len() as i64,
                    residual_aspect_count: serialized_residual
                        .as_ref()
                        .map_or(0, |serialized| serialized.pending_aspect_count),
                    review_payload_sha256,
                    catalog_revision_sha256: stored_catalog_revision,
                    ingestion_state,
                },
            )
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            apply_in_transaction!(pool, reuse_attestation_is_current_sqlite)
        }
        DatabaseBackend::Postgres(pool) => {
            apply_in_transaction!(pool, reuse_attestation_is_current_postgres)
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use crate::avionics::manufacturer::{
        ensure_manufacturer_identity, ManufacturerIdentityEvidence,
    };
    use crate::avionics::reuse::refresh_reuse_attestation_sqlite;
    use crate::normalize::{
        normalize_avionics_identifier, normalize_avionics_manufacturer_name,
        normalize_avionics_model_name, normalize_name,
    };

    use super::*;
    use crate::listing::review::{stage_pending_review, ReviewProduct};

    struct Fixture {
        db: AppDb,
        listing_id: i64,
        submission_id: i64,
        review_payload_sha256: String,
        rendered_html_sha256: String,
        faa_snapshot_id: i64,
        faa_source_record_sha256: String,
    }

    fn pool(db: &AppDb) -> &SqlitePool {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("automation tests require SQLite");
        };
        pool
    }

    fn pending_aspect(id: &str, label: &str) -> PendingReviewAspect {
        PendingReviewAspect::avionics(
            id,
            "avionics_identity",
            label,
            format!("{label} shown in listing equipment"),
            "automated_identity_unresolved",
            1,
            "installed",
            Some(format!("Listing identifies {label}")),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            label,
            vec!["GPS".to_string()],
        ))
    }

    async fn fixture() -> Fixture {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = pool(&db);
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
              model_year, asking_price_usd, registration_number, serial_number,
              airframe_hours
            ) VALUES (?, ?, 'https://broker.example/listing', 2020, 450000,
                      'N123AB', '182-01234', 900)
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .bind(owner_user_id)
        .fetch_one(pool)
        .await
        .unwrap();

        let faa_archive_sha256 = "a".repeat(64);
        let faa_source_record_sha256 = "b".repeat(64);
        let faa_source_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO curation_evidence_sources (
              source_url, resolved_url, source_title, publisher, source_domain,
              source_tier, content_sha256, retrieved_at
            ) VALUES (
              'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
              'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
              'FAA Releasable Aircraft Registry', 'Federal Aviation Administration',
              'faa.gov', 'regulator_primary', ?, CURRENT_TIMESTAMP
            )
            RETURNING id
            "#,
        )
        .bind(&faa_archive_sha256)
        .fetch_one(pool)
        .await
        .unwrap();
        let faa_snapshot_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO faa_registry_snapshots (
              evidence_source_id, snapshot_date, source_url, archive_sha256,
              source_manifest_sha256, target_set_sha256,
              master_member_name, master_member_sha256,
              aircraft_member_name, aircraft_member_sha256,
              engine_member_name, engine_member_sha256, record_hash_domain
            ) VALUES (
              ?, '2026-07-23',
              'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
              ?, ?, ?, 'MASTER.txt', ?, 'ACFTREF.txt', ?, 'ENGINE.txt', ?,
              'aircost-faa-master-retained-aircraft-projection-v1'
            )
            RETURNING id
            "#,
        )
        .bind(faa_source_id)
        .bind(&faa_archive_sha256)
        .bind("c".repeat(64))
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .bind("f".repeat(64))
        .bind("0".repeat(64))
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO faa_registry_aircraft (
              snapshot_id, n_number, manufacturer_serial_raw,
              manufacturer_serial_key, aircraft_code, year_manufactured,
              source_record_sha256
            ) VALUES (?, 'N123AB', '182-01234', '18201234',
                      '1234567', 2020, ?)
            "#,
        )
        .bind(faa_snapshot_id)
        .bind(&faa_source_record_sha256)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO faa_registry_coverage (snapshot_id, n_number, lookup_status) VALUES (?, 'N123AB', 'matched')",
        )
        .bind(faa_snapshot_id)
        .execute(pool)
        .await
        .unwrap();

        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'test-key') RETURNING id",
        )
        .bind(owner_user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let rendered_html = r#"<html><body>
            Garmin avionics: Garmin GTN 750Xi, Garmin GTX 345, Garmin GNS 430W,
            Garmin GMA 340, Garmin GTN 650Xi, Garmin GNS 530W, and Garmin Unverified Unit.
            GIA-63W NAV/COM/GPS with Glideslope.
            Garmin GTN 750Xi replaces Garmin GNS 530W.
        </body></html>"#;
        let rendered_html_sha256 = sha256_hex(rendered_html.as_bytes());
        let submission_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, canonical_listing_id
            ) VALUES (?, ?, 'https://broker.example/listing', ?, ?,
                      'test-signature', ?)
            RETURNING id
            "#,
        )
        .bind(owner_user_id)
        .bind(install_id)
        .bind(rendered_html)
        .bind(&rendered_html_sha256)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let staged = stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[pending_aspect("original:0", "Unknown panel item")],
        )
        .await
        .unwrap();

        Fixture {
            db,
            listing_id,
            submission_id,
            review_payload_sha256: staged.review_payload_sha256,
            rendered_html_sha256,
            faa_snapshot_id,
            faa_source_record_sha256,
        }
    }

    async fn insert_product(db: &AppDb, model: &str, identifier: &str, approved: bool) -> i64 {
        let pool = pool(db);
        let manufacturer_key = normalize_avionics_manufacturer_name("Garmin");
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Garmin', ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(&manufacturer_key)
        .execute(pool)
        .await
        .unwrap();
        let manufacturer_id: i64 =
            sqlx::query_scalar("SELECT id FROM avionics_manufacturers WHERE normalized_name = ?")
                .bind(&manufacturer_key)
                .fetch_one(pool)
                .await
                .unwrap();
        ensure_manufacturer_identity(
            db,
            manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: "https://www.garmin.com/en-US/aviation/".to_string(),
                source_title: "Garmin Aviation".to_string(),
                evidence_text: "Garmin identifies itself as the avionics manufacturer.".to_string(),
            },
        )
        .await
        .unwrap();
        let model_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_evidence_kind, identity_confidence, catalog_reviewed_at
            ) VALUES (
              ?, ?, ?, 'manufacturer_model_number', ?, ?,
              'https://www.garmin.com/en-US/aviation/',
              'Garmin product reference',
              'Garmin identifies this exact marketed avionics product.',
              'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
            )
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .bind(model)
        .bind(normalize_avionics_model_name(model))
        .bind(identifier)
        .bind(normalize_avionics_identifier(identifier))
        .fetch_one(pool)
        .await
        .unwrap();
        let capability = normalize_name("GPS");
        sqlx::query(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('GPS', ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(&capability)
        .execute(pool)
        .await
        .unwrap();
        let capability_id: i64 =
            sqlx::query_scalar("SELECT id FROM avionics_types WHERE normalized_name = ?")
                .bind(&capability)
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(model_id)
        .bind(capability_id)
        .execute(pool)
        .await
        .unwrap();
        if approved {
            sqlx::query("UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?")
                .bind(model_id)
                .execute(pool)
                .await
                .unwrap();
            let mut transaction = pool.begin().await.unwrap();
            assert!(
                refresh_reuse_attestation_sqlite(
                    db,
                    &mut transaction,
                    model_id,
                    "https://www.garmin.com/en-US/aviation/",
                )
                .await
                .unwrap(),
                "approved automation fixture must be current-policy reusable"
            );
            transaction.commit().await.unwrap();
        }
        model_id
    }

    fn request(
        fixture: &Fixture,
        accepted_links: Vec<AutomatedAvionicsLink>,
        residual_aspects: Vec<PendingReviewAspect>,
    ) -> AutomatedReviewApplyRequest {
        AutomatedReviewApplyRequest {
            listing_id: fixture.listing_id,
            plugin_submission_id: fixture.submission_id,
            expected_review_payload_sha256: fixture.review_payload_sha256.clone(),
            expected_rendered_html_sha256: fixture.rendered_html_sha256.clone(),
            expected_faa_snapshot_id: fixture.faa_snapshot_id,
            expected_faa_source_record_sha256: fixture.faa_source_record_sha256.clone(),
            accepted_links,
            residual_aspects,
        }
    }

    fn accepted_with_revision(
        model_id: i64,
        expected_collision_closure_sha256: String,
        source_notes: String,
    ) -> AutomatedAvionicsLink {
        AutomatedAvionicsLink {
            avionics_model_id: model_id,
            authorization: AutomatedAssociationAuthorization::ManufacturerReuse,
            expected_collision_closure_sha256,
            quantity: 1,
            source_notes: Some(source_notes),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: None,
        }
    }

    async fn accepted(db: &AppDb, model_id: i64) -> AutomatedAvionicsLink {
        let sql = db.sql(
            r#"
            SELECT manufacturer.name || ' ' || model.name
            FROM avionics_models model
            JOIN avionics_manufacturers manufacturer
              ON manufacturer.id = model.avionics_manufacturer_id
            WHERE model.id = ?
            "#,
        );
        let source_notes = match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query_scalar(&sql)
                .bind(model_id)
                .fetch_one(pool)
                .await
                .unwrap(),
            DatabaseBackend::Postgres(pool) => sqlx::query_scalar(&sql)
                .bind(model_id)
                .fetch_one(pool)
                .await
                .unwrap(),
        };
        accepted_with_revision(
            model_id,
            super::super::active_collision_closure_revision_sha256(db, model_id)
                .await
                .unwrap(),
            source_notes,
        )
    }

    async fn same_case_accepted(fixture: &Fixture, model_id: i64) -> AutomatedAvionicsLink {
        let mut link = accepted_with_revision(
            model_id,
            super::super::grounded_collision_closure_revision_sha256(&fixture.db, model_id)
                .await
                .unwrap(),
            "Garmin GTX 345".to_string(),
        );
        link.authorization = AutomatedAssociationAuthorization::SameCaseGrounded(
            crate::avionics::catalog::grounded_resolution_receipt_for_test(
                fixture.listing_id,
                model_id,
            ),
        );
        link
    }

    #[tokio::test]
    async fn same_case_grounded_authorization_accepts_without_global_reuse() {
        let fixture = fixture().await;
        let model_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        let accepted_link = same_case_accepted(&fixture, model_id).await;
        let AutomatedAssociationAuthorization::SameCaseGrounded(receipt) =
            &accepted_link.authorization
        else {
            unreachable!()
        };
        let resolution_sha256 = receipt.resolution_sha256().to_string();
        let residual = pending_aspect("residual:0", "Unknown audio panel");

        let result = apply_automated_avionics_review(
            &fixture.db,
            &request(&fixture, vec![accepted_link], vec![residual.clone()]),
        )
        .await
        .expect("full grounding should authorize its exact listing association");
        assert_eq!(result.accepted_link_count, 1);

        let stored: (String, Option<String>, String) = sqlx::query_as(
            r#"
            SELECT authorization_kind, grounded_resolution_sha256,
                   evidence_capture_sha256
            FROM aircraft_sale_listing_avionics_authorizations authorization
            JOIN aircraft_sale_listing_avionics link
              ON link.id = authorization.listing_link_id
            WHERE link.aircraft_sale_listing_id = ?
              AND authorization.association_role = 'installed'
            "#,
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored.0, "same_case_grounded");
        assert_eq!(stored.1.as_deref(), Some(resolution_sha256.as_str()));
        assert_eq!(stored.2, fixture.rendered_html_sha256);
        let reuse_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?",
        )
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(reuse_count, 0, "same-case proof must not mint global reuse");

        let mut replay = request(&fixture, vec![], vec![residual]);
        replay.expected_review_payload_sha256 = result
            .review_payload_sha256
            .expect("the residual review should remain staged");
        let replayed = apply_automated_avionics_review(&fixture.db, &replay)
            .await
            .expect(
                "a current same-case authorization should preserve its exact link without Gemini",
            );
        assert_eq!(replayed.preserved_link_count, 1);
        assert_eq!(replayed.stored_link_count, 1);
    }

    #[tokio::test]
    async fn same_case_grounding_repairs_an_exact_existing_link_without_prior_authorization() {
        let fixture = fixture().await;
        let model_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        let existing_link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Garmin GTX 345',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(fixture.listing_id)
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();

        let result = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![same_case_accepted(&fixture, model_id).await],
                vec![],
            ),
        )
        .await
        .expect("fresh same-case grounding should authorize the touched exact row");

        assert_eq!(result.accepted_link_count, 1);
        assert_eq!(result.stored_link_count, 1);
        let stored: (i64, String) = sqlx::query_as(
            r#"
            SELECT link.id, authorization.authorization_kind
            FROM aircraft_sale_listing_avionics link
            JOIN aircraft_sale_listing_avionics_authorizations authorization
              ON authorization.listing_link_id = link.id
             AND authorization.association_role = 'installed'
            WHERE link.aircraft_sale_listing_id = ?
              AND link.avionics_model_id = ?
            "#,
        )
        .bind(fixture.listing_id)
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored, (existing_link_id, "same_case_grounded".to_string()));
    }

    #[tokio::test]
    async fn same_case_authorization_is_removed_when_product_source_proof_changes() {
        let fixture = fixture().await;
        let model_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![same_case_accepted(&fixture, model_id).await],
                vec![],
            ),
        )
        .await
        .unwrap();

        sqlx::query("UPDATE avionics_models SET identity_evidence_text = ? WHERE id = ?")
            .bind("The authoritative identity proof was replaced.")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE avionics_model_id = ?",
        )
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(authorization_count, 0);
    }

    #[tokio::test]
    async fn same_case_authorization_is_removed_only_for_its_revoked_exact_origin() {
        let fixture = fixture().await;
        let model_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![same_case_accepted(&fixture, model_id).await],
                vec![],
            ),
        )
        .await
        .unwrap();
        let reviewer_user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email = ?")
            .bind(crate::db::DEVELOPER_EMAIL)
            .fetch_one(pool)
            .await
            .unwrap();

        let static_origin_id: i64 = sqlx::query_scalar(
            "SELECT id FROM avionics_authoritative_source_origins WHERE https_origin = 'https://static.garmin.com'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO avionics_authoritative_source_origin_revocations (
              avionics_authoritative_source_origin_id, revoked_by_user_id, reason
            ) VALUES (?, ?, 'Regression test revokes a sibling exact origin')
            "#,
        )
        .bind(static_origin_id)
        .bind(reviewer_user_id)
        .execute(pool)
        .await
        .unwrap();
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE avionics_model_id = ?",
        )
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            authorization_count, 1,
            "revoking a sibling exact origin must not invalidate this source proof"
        );

        let product_origin_id: i64 = sqlx::query_scalar(
            "SELECT id FROM avionics_authoritative_source_origins WHERE https_origin = 'https://www.garmin.com'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO avionics_authoritative_source_origin_revocations (
              avionics_authoritative_source_origin_id, revoked_by_user_id, reason
            ) VALUES (?, ?, 'Regression test revokes the product proof origin')
            "#,
        )
        .bind(product_origin_id)
        .bind(reviewer_user_id)
        .execute(pool)
        .await
        .unwrap();
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE avionics_model_id = ?",
        )
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(authorization_count, 0);
    }

    #[tokio::test]
    async fn same_case_authorization_is_removed_for_a_revoked_regulator_origin() {
        let fixture = fixture().await;
        let model_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            r#"
            UPDATE avionics_models
            SET identity_source_url = 'https://drs.faa.gov/browse/avionics/gtx345',
                identity_source_title = 'FAA DRS GTX 345 record',
                identity_evidence_text =
                  'FAA DRS identifies the exact GTX 345 avionics product.'
            WHERE id = ?
            "#,
        )
        .bind(model_id)
        .execute(pool)
        .await
        .unwrap();
        let reviewer_user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email = ?")
            .bind(crate::db::DEVELOPER_EMAIL)
            .fetch_one(pool)
            .await
            .unwrap();
        let regulator_origin_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_authoritative_source_origins (
              authority_kind, avionics_manufacturer_identity_id,
              regulator_key, https_origin, evidence_source_url,
              evidence_source_title, evidence_text, approval_basis,
              approved_by_user_id, approval_reason
            ) VALUES (
              'regulator_primary', NULL, 'faa_drs', 'https://drs.faa.gov',
              'https://drs.faa.gov', 'FAA Dynamic Regulatory System',
              'FAA DRS is an authoritative regulator source for this test.',
              'human_review', ?,
              'Regression test approves the exact FAA DRS origin'
            )
            RETURNING id
            "#,
        )
        .bind(reviewer_user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![same_case_accepted(&fixture, model_id).await],
                vec![],
            ),
        )
        .await
        .unwrap();

        sqlx::query(
            r#"
            INSERT INTO avionics_authoritative_source_origin_revocations (
              avionics_authoritative_source_origin_id, revoked_by_user_id, reason
            ) VALUES (?, ?, 'Regression test revokes the regulator proof origin')
            "#,
        )
        .bind(regulator_origin_id)
        .bind(reviewer_user_id)
        .execute(pool)
        .await
        .unwrap();
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE avionics_model_id = ?",
        )
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(authorization_count, 0);
    }

    #[tokio::test]
    async fn listing_authorization_is_removed_when_its_only_capture_changes() {
        let fixture = fixture().await;
        let model_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![same_case_accepted(&fixture, model_id).await],
                vec![],
            ),
        )
        .await
        .unwrap();

        let changed_html = "<html><body>Listing capture replaced.</body></html>";
        sqlx::query(
            "UPDATE plugin_submissions SET rendered_html = ?, rendered_html_sha256 = ? WHERE id = ?",
        )
        .bind(changed_html)
        .bind(sha256_hex(changed_html.as_bytes()))
        .bind(fixture.submission_id)
        .execute(pool)
        .await
        .unwrap();
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE avionics_model_id = ?",
        )
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(authorization_count, 0);
    }

    #[tokio::test]
    async fn same_case_grounded_receipt_cannot_be_replayed_for_another_listing_or_product() {
        let fixture = fixture().await;
        let model_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let other_id = insert_product(&fixture.db, "GTN 750Xi", "GTN750XI", true).await;
        let pool = pool(&fixture.db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        let mut wrong_listing = same_case_accepted(&fixture, model_id).await;
        wrong_listing.authorization = AutomatedAssociationAuthorization::SameCaseGrounded(
            crate::avionics::catalog::grounded_resolution_receipt_for_test(
                fixture.listing_id + 1,
                model_id,
            ),
        );
        assert!(matches!(
            apply_automated_avionics_review(
                &fixture.db,
                &request(&fixture, vec![wrong_listing], vec![])
            )
            .await,
            Err(ReviewError::Validation(message))
                if message.contains("not bound to this listing and product")
        ));

        let mut wrong_product = same_case_accepted(&fixture, model_id).await;
        wrong_product.avionics_model_id = other_id;
        wrong_product.expected_collision_closure_sha256 =
            super::super::grounded_collision_closure_revision_sha256(&fixture.db, other_id)
                .await
                .unwrap();
        assert!(matches!(
            apply_automated_avionics_review(
                &fixture.db,
                &request(&fixture, vec![wrong_product], vec![])
            )
            .await,
            Err(ReviewError::Validation(message))
                if message.contains("not bound to this listing and product")
        ));
    }

    #[tokio::test]
    async fn partial_success_persists_accepted_links_and_only_residual_review() {
        let fixture = fixture().await;
        let accepted_id = insert_product(&fixture.db, "GTN 750Xi", "GTN750XI", true).await;
        let residual = pending_aspect("residual:0", "Unclear audio panel");
        let result = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![accepted(&fixture.db, accepted_id).await],
                vec![residual.clone()],
            ),
        )
        .await
        .unwrap();

        assert_eq!(result.accepted_link_count, 1);
        assert_eq!(result.residual_aspect_count, 1);
        assert_eq!(result.ingestion_state, "pending_review");
        let pool = pool(&fixture.db);
        let stored: (String, Option<String>) = sqlx::query_as(
            "SELECT source, source_confidence FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?",
        )
        .bind(fixture.listing_id)
        .bind(accepted_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored, ("listing".to_string(), Some("high".to_string())));
        let authorization_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM aircraft_sale_listing_avionics_authorizations authorization
            JOIN aircraft_sale_listing_avionics link
              ON link.id = authorization.listing_link_id
            WHERE link.aircraft_sale_listing_id = ?
            "#,
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(authorization_count, 1);
        let review: (i64, String) = sqlx::query_as(
            "SELECT pending_aspect_count, review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(review.0, 1);
        assert_eq!(
            review.1,
            serialize_review_payload(&[residual])
                .unwrap()
                .review_payload_sha256
        );
        let owner_user_id: i64 = sqlx::query_scalar(
            "SELECT created_by_user_id FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        super::super::restage_pending_review_if_current(
            &fixture.db,
            owner_user_id,
            fixture.listing_id,
            &review.1,
        )
        .await
        .unwrap();
        let restaged: (String, i64) = sqlx::query_as(
            "SELECT review_payload_json, pending_aspect_count FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let payload = parse_payload(&restaged.0, None, restaged.1).unwrap();
        assert!(payload
            .aspects
            .iter()
            .all(|aspect| aspect.reuse_attestation_target_id != Some(accepted_id)));
    }

    #[tokio::test]
    async fn restage_reissues_corroboration_after_model_only_evidence_repair() {
        let fixture = fixture().await;
        let accepted_id = insert_product(&fixture.db, "GIA63W", "GIA63W", true).await;
        let residual = pending_aspect("residual:0", "Unclear audio panel");
        let collision_revision =
            super::super::active_collision_closure_revision_sha256(&fixture.db, accepted_id)
                .await
                .unwrap();
        let result = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![accepted_with_revision(
                    accepted_id,
                    collision_revision.clone(),
                    "GIA-63W NAV/COM/GPS with Glideslope".to_string(),
                )],
                vec![residual.clone()],
            ),
        )
        .await
        .unwrap();
        assert_eq!(result.accepted_link_count, 1);
        assert_eq!(result.residual_aspect_count, 1);

        let pool = pool(&fixture.db);
        let (link_id, source_notes): (i64, Option<String>) = sqlx::query_as(
            r#"
            SELECT id, source_notes
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
              AND avionics_model_id = ?
            "#,
        )
        .bind(fixture.listing_id)
        .bind(accepted_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            source_notes.as_deref(),
            Some("GIA-63W NAV/COM/GPS with Glideslope")
        );
        let before: (String, String) = sqlx::query_as(
            r#"
            SELECT authorization.observation_sha256,
                   authorization.collision_closure_sha256
            FROM aircraft_sale_listing_avionics_authorizations authorization
            WHERE authorization.listing_link_id = ?
              AND authorization.association_role = 'installed'
            "#,
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(before.1, collision_revision);

        let owner_user_id: i64 = sqlx::query_scalar(
            "SELECT created_by_user_id FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let review_hash: String = sqlx::query_scalar(
            "SELECT review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        super::super::restage_pending_review_if_current(
            &fixture.db,
            owner_user_id,
            fixture.listing_id,
            &review_hash,
        )
        .await
        .unwrap();

        let (repaired_notes, confidence): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT source_notes, source_confidence FROM aircraft_sale_listing_avionics WHERE id = ?",
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(repaired_notes.as_deref(), Some("GIA-63W"));
        assert_eq!(confidence.as_deref(), Some("high"));
        let after: (String, String) = sqlx::query_as(
            r#"
            SELECT authorization.observation_sha256,
                   authorization.collision_closure_sha256
            FROM aircraft_sale_listing_avionics_authorizations authorization
            WHERE authorization.listing_link_id = ?
              AND authorization.association_role = 'installed'
            "#,
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_ne!(after.0, before.0);
        assert_eq!(
            after.0,
            association_observation_sha256_from_values(
                fixture.listing_id,
                link_id,
                ListingAssociationRole::Installed,
                accepted_id,
                accepted_id,
                None,
                1,
                "installed",
                "GIA-63W",
            )
        );
        assert_eq!(after.1, collision_revision);
        let restaged: (String, i64, String) = sqlx::query_as(
            "SELECT review_payload_json, pending_aspect_count, review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let payload = parse_payload(&restaged.0, None, restaged.1).unwrap();
        assert_eq!(payload.aspects.len(), 1);
        assert_eq!(payload.aspects[0].id, residual.id);
        assert_ne!(
            payload.aspects[0].reuse_attestation_target_id,
            Some(accepted_id)
        );

        let second = super::super::restage_pending_review_if_current(
            &fixture.db,
            owner_user_id,
            fixture.listing_id,
            &restaged.2,
        )
        .await
        .unwrap()
        .expect("the independent residual remains pending");
        assert_eq!(second.review_payload_sha256, restaged.2);
        assert_eq!(second.pending_aspect_count, 1);
        let after_second: (String, String, i64) = sqlx::query_as(
            r#"
            SELECT authorization.observation_sha256,
                   authorization.collision_closure_sha256,
                   (SELECT COUNT(*)
                    FROM aircraft_sale_listing_avionics_authorizations
                    WHERE listing_link_id = ?)
            FROM aircraft_sale_listing_avionics_authorizations authorization
            WHERE authorization.listing_link_id = ?
              AND authorization.association_role = 'installed'
            "#,
        )
        .bind(link_id)
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(after_second, (after.0, after.1, 1));
    }

    #[tokio::test]
    async fn restage_does_not_mint_corroboration_from_listing_review_provenance() {
        let fixture = fixture().await;
        let reviewed_id = insert_product(&fixture.db, "GIA63W", "GIA63W", true).await;
        let pool = pool(&fixture.db);
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing_review',
                      'GIA-63W NAV/COM/GPS with Glideslope',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(fixture.listing_id)
        .bind(reviewed_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let owner_user_id: i64 = sqlx::query_scalar(
            "SELECT created_by_user_id FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();

        let restaged = super::super::restage_pending_review_if_current(
            &fixture.db,
            owner_user_id,
            fixture.listing_id,
            &fixture.review_payload_sha256,
        )
        .await
        .unwrap()
        .expect("the independent residual remains pending");
        assert_eq!(restaged.pending_aspect_count, 1);
        let repaired_notes: Option<String> = sqlx::query_scalar(
            "SELECT source_notes FROM aircraft_sale_listing_avionics WHERE id = ?",
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(repaired_notes.as_deref(), Some("GIA-63W"));
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = ?",
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(authorization_count, 0);
    }

    #[tokio::test]
    async fn all_pass_clears_review_but_never_marks_listing_ready() {
        let fixture = fixture().await;
        let accepted_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let result = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![accepted(&fixture.db, accepted_id).await],
                vec![],
            ),
        )
        .await
        .unwrap();

        assert_eq!(result.residual_aspect_count, 0);
        assert_eq!(result.review_payload_sha256, None);
        assert_eq!(result.ingestion_state, "incomplete");
        let pool = pool(&fixture.db);
        let state: String =
            sqlx::query_scalar("SELECT ingestion_state FROM aircraft_sale_listings WHERE id = ?")
                .bind(fixture.listing_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let review_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(state, "incomplete");
        assert_eq!(review_count, 0);
    }

    #[tokio::test]
    async fn exact_accepted_link_keeps_its_existing_row() {
        let fixture = fixture().await;
        let accepted_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        let existing_link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Garmin GTX 345',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(fixture.listing_id)
        .bind(accepted_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let product_fingerprint: String = sqlx::query_scalar(
            "SELECT product_fingerprint FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?",
        )
        .bind(accepted_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics_authorizations (
              listing_link_id, association_role, avionics_model_id,
              authorization_kind, observation_sha256, product_fingerprint,
              grounded_resolution_sha256, evidence_capture_sha256,
              collision_closure_sha256, policy_version
            ) VALUES (?, 'installed', ?, 'manufacturer_reuse', ?, ?, NULL,
                      ?, ?, 'listing_avionics_authorization_v1')
            "#,
        )
        .bind(existing_link_id)
        .bind(accepted_id)
        .bind("1".repeat(64))
        .bind(&product_fingerprint)
        .bind(&fixture.rendered_html_sha256)
        .bind("2".repeat(64))
        .execute(pool)
        .await
        .unwrap();

        let result = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![accepted(&fixture.db, accepted_id).await],
                vec![],
            ),
        )
        .await
        .unwrap();

        assert_eq!(result.accepted_link_count, 1);
        assert_eq!(result.stored_link_count, 1);
        let stored_link_id: i64 = sqlx::query_scalar(
            "SELECT id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?",
        )
        .bind(fixture.listing_id)
        .bind(accepted_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored_link_id, existing_link_id);
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = ?",
        )
        .bind(existing_link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(authorization_count, 1);
        let refreshed: (String, String) = sqlx::query_as(
            r#"
            SELECT authorization.observation_sha256,
                   authorization.collision_closure_sha256
            FROM aircraft_sale_listing_avionics_authorizations authorization
            WHERE authorization.listing_link_id = ?
              AND authorization.association_role = 'installed'
            "#,
        )
        .bind(existing_link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_ne!(refreshed.0, "1".repeat(64));
        assert_ne!(refreshed.1, "2".repeat(64));
    }

    #[tokio::test]
    async fn changed_accepted_link_replaces_the_row_and_invalidates_corroboration() {
        let fixture = fixture().await;
        let accepted_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        let existing_link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 2, 'listing', 'Garmin GTX 345',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(fixture.listing_id)
        .bind(accepted_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let product_fingerprint: String = sqlx::query_scalar(
            "SELECT product_fingerprint FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?",
        )
        .bind(accepted_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics_authorizations (
              listing_link_id, association_role, avionics_model_id,
              authorization_kind, observation_sha256, product_fingerprint,
              grounded_resolution_sha256, evidence_capture_sha256,
              collision_closure_sha256, policy_version
            ) VALUES (?, 'installed', ?, 'manufacturer_reuse', ?, ?, NULL,
                      ?, ?, 'listing_avionics_authorization_v1')
            "#,
        )
        .bind(existing_link_id)
        .bind(accepted_id)
        .bind("1".repeat(64))
        .bind(product_fingerprint)
        .bind(&fixture.rendered_html_sha256)
        .bind("2".repeat(64))
        .execute(pool)
        .await
        .unwrap();

        apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![accepted(&fixture.db, accepted_id).await],
                vec![],
            ),
        )
        .await
        .unwrap();

        let stored_link_id: i64 = sqlx::query_scalar(
            "SELECT id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?",
        )
        .bind(fixture.listing_id)
        .bind(accepted_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_ne!(stored_link_id, existing_link_id);
        let old_authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = ?",
        )
        .bind(existing_link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(old_authorization_count, 0);
        let replacement_authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = ?",
        )
        .bind(stored_link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(replacement_authorization_count, 1);
    }

    #[tokio::test]
    async fn changed_preserved_association_guard_fails_closed_before_mutation() {
        let fixture = fixture().await;
        let model_id = insert_product(&fixture.db, "GTN 750Xi", "GTN750XI", true).await;
        let pool = pool(&fixture.db);
        let evidence = "Garmin GTN 750Xi";
        let listing_link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', ?, 'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(fixture.listing_id)
        .bind(model_id)
        .bind(evidence)
        .fetch_one(pool)
        .await
        .unwrap();
        let expected_observation_sha256 = association_observation_sha256_from_values(
            fixture.listing_id,
            listing_link_id,
            ListingAssociationRole::Installed,
            model_id,
            model_id,
            None,
            1,
            "installed",
            evidence,
        );
        let mut link = accepted(&fixture.db, model_id).await;
        link.preserved_association_guard = Some(AutomatedPreservedAssociationGuard {
            listing_link_id,
            association_role: ListingAssociationRole::Installed,
            expected_observation_sha256,
        });

        sqlx::query("UPDATE aircraft_sale_listing_avionics SET quantity = 2 WHERE id = ?")
            .bind(listing_link_id)
            .execute(pool)
            .await
            .unwrap();

        let error = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![link],
                vec![pending_aspect("original:0", "Unknown panel item")],
            ),
        )
        .await
        .expect_err("a preserved link mutation after evaluation must fail closed");
        assert!(matches!(
            error,
            ReviewError::Stale(message) if message.contains("changed after local evaluation")
        ));
        let retained_quantity: i64 =
            sqlx::query_scalar("SELECT quantity FROM aircraft_sale_listing_avionics WHERE id = ?")
                .bind(listing_link_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let retained_review_sha256: String = sqlx::query_scalar(
            "SELECT review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = ?",
        )
        .bind(listing_link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained_quantity, 2);
        assert_eq!(retained_review_sha256, fixture.review_payload_sha256);
        assert_eq!(authorization_count, 0);
    }

    #[tokio::test]
    async fn stale_review_hash_rolls_back_every_link_change() {
        let fixture = fixture().await;
        let existing_id = insert_product(&fixture.db, "GNS 430W", "GNS430W", true).await;
        let accepted_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, source,
              source_confidence, configuration_action
            ) VALUES (?, ?, 'listing', 'high', 'installed')
            "#,
        )
        .bind(fixture.listing_id)
        .bind(existing_id)
        .execute(pool)
        .await
        .unwrap();
        let mut stale = request(
            &fixture,
            vec![accepted(&fixture.db, accepted_id).await],
            vec![],
        );
        stale.expected_review_payload_sha256 = "0".repeat(64);
        assert!(matches!(
            apply_automated_avionics_review(&fixture.db, &stale).await,
            Err(ReviewError::Stale(_))
        ));
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT avionics_model_id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? ORDER BY avionics_model_id",
        )
        .bind(fixture.listing_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(ids, vec![existing_id]);
        let review_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(review_count, 1);
    }

    #[tokio::test]
    async fn approved_high_existing_link_is_preserved_when_disjoint() {
        let fixture = fixture().await;
        let existing_id = insert_product(&fixture.db, "GNS 430W", "GNS430W", true).await;
        let weak_id = insert_product(&fixture.db, "GMA 340", "GMA340", true).await;
        let accepted_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        let mut preserved_link_id = None;
        for (model_id, confidence) in [(existing_id, "high"), (weak_id, "medium")] {
            let link_id: i64 = sqlx::query_scalar(
                r#"
                INSERT INTO aircraft_sale_listing_avionics (
                  aircraft_sale_listing_id, avionics_model_id, source,
                  source_confidence, configuration_action
                ) VALUES (?, ?, 'listing', ?, 'installed')
                RETURNING id
                "#,
            )
            .bind(fixture.listing_id)
            .bind(model_id)
            .bind(confidence)
            .fetch_one(pool)
            .await
            .unwrap();
            if model_id == existing_id {
                preserved_link_id = Some(link_id);
            }
        }
        let result = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![accepted(&fixture.db, accepted_id).await],
                vec![],
            ),
        )
        .await
        .unwrap();
        assert_eq!(result.preserved_link_count, 1);
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT avionics_model_id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? ORDER BY avionics_model_id",
        )
        .bind(fixture.listing_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(ids, vec![existing_id, accepted_id]);
        let stored_preserved_link_id: i64 = sqlx::query_scalar(
            "SELECT id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?",
        )
        .bind(fixture.listing_id)
        .bind(existing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(Some(stored_preserved_link_id), preserved_link_id);
    }

    #[tokio::test]
    async fn unapproved_accepted_id_is_refused_without_mutation() {
        let fixture = fixture().await;
        let unapproved_id = insert_product(&fixture.db, "Unverified Unit", "UNKNOWN1", false).await;
        assert!(matches!(
            apply_automated_avionics_review(
                &fixture.db,
                &request(
                    &fixture,
                    vec![accepted_with_revision(
                        unapproved_id,
                        "0".repeat(64),
                        "Garmin Unverified Unit".to_string(),
                    )],
                    vec![],
                )
            )
            .await,
            Err(ReviewError::Stale(_))
        ));
        let pool = pool(&fixture.db);
        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let review_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(link_count, 0);
        assert_eq!(review_count, 1);
    }

    #[tokio::test]
    async fn attestation_removed_after_resolution_rejects_automated_link_atomically() {
        let fixture = fixture().await;
        let accepted_id = insert_product(&fixture.db, "GTN 650Xi", "GTN650XI", true).await;
        let pool = pool(&fixture.db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(accepted_id)
            .execute(pool)
            .await
            .unwrap();

        let error = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![accepted(&fixture.db, accepted_id).await],
                vec![],
            ),
        )
        .await
        .expect_err("stale reuse eligibility must be checked at the link-write boundary");
        assert!(matches!(
            error,
            ReviewError::Stale(message)
                if message.contains("lost its manufacturer-primary reuse authorization")
        ));
        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let review_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(link_count, 0);
        assert_eq!(review_count, 1);
    }

    #[tokio::test]
    async fn changed_collision_closure_rejects_automated_link_atomically() {
        let fixture = fixture().await;
        let accepted_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let accepted = accepted(&fixture.db, accepted_id).await;
        insert_product(&fixture.db, "GTX 345 Legacy", "GTX345LEGACY", false).await;

        let error = apply_automated_avionics_review(
            &fixture.db,
            &request(&fixture, vec![accepted], vec![]),
        )
        .await
        .expect_err("resolution-time collision closure must be rechecked under the write lock");
        assert!(matches!(
            error,
            ReviewError::Stale(message)
                if message.contains("collision catalog changed")
        ));
        let pool = pool(&fixture.db);
        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let review_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!((link_count, review_count), (0, 1));
    }

    #[tokio::test]
    async fn non_listing_or_unbound_evidence_never_mints_corroboration() {
        let fixture = fixture().await;
        let accepted_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let mut fabricated = accepted(&fixture.db, accepted_id).await;
        fabricated.source_notes = Some("Fabricated Garmin GTX 345 occurrence".to_string());
        let error = apply_automated_avionics_review(
            &fixture.db,
            &request(&fixture, vec![fabricated], vec![]),
        )
        .await
        .expect_err("non-listing text cannot become durable association evidence");
        assert!(matches!(error, ReviewError::Validation(_)));

        sqlx::query("UPDATE plugin_submissions SET canonical_listing_id = NULL WHERE id = ?")
            .bind(fixture.submission_id)
            .execute(pool(&fixture.db))
            .await
            .unwrap();
        let error = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![accepted(&fixture.db, accepted_id).await],
                vec![],
            ),
        )
        .await
        .expect_err("URL equality cannot substitute for exact corroboration provenance");
        assert!(matches!(
            error,
            ReviewError::Stale(message) if message.contains("exact canonical listing")
        ));
        let counts: (i64, i64) = sqlx::query_as(
            r#"
            SELECT
              (SELECT COUNT(*) FROM aircraft_sale_listing_avionics
               WHERE aircraft_sale_listing_id = ?),
              (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations)
            "#,
        )
        .bind(fixture.listing_id)
        .fetch_one(pool(&fixture.db))
        .await
        .unwrap();
        assert_eq!(counts, (0, 0));
    }

    #[tokio::test]
    async fn replacement_acceptance_corroborates_both_exact_link_roles() {
        let fixture = fixture().await;
        let installed_id = insert_product(&fixture.db, "GTN 750Xi", "GTN750XI", true).await;
        let replacement_id = insert_product(&fixture.db, "GNS 530W", "GNS530W", true).await;
        let link = AutomatedAvionicsLink {
            avionics_model_id: installed_id,
            authorization: AutomatedAssociationAuthorization::ManufacturerReuse,
            expected_collision_closure_sha256:
                super::super::active_collision_closure_revision_sha256(&fixture.db, installed_id)
                    .await
                    .unwrap(),
            quantity: 1,
            source_notes: Some("Garmin GTN 750Xi replaces Garmin GNS 530W".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "replaces".to_string(),
            replaces_avionics_model_id: Some(replacement_id),
            replacement_authorization: Some(AutomatedAssociationAuthorization::ManufacturerReuse),
            expected_replacement_collision_closure_sha256: Some(
                super::super::active_collision_closure_revision_sha256(&fixture.db, replacement_id)
                    .await
                    .unwrap(),
            ),
            preserved_association_guard: None,
        };

        apply_automated_avionics_review(&fixture.db, &request(&fixture, vec![link], vec![]))
            .await
            .unwrap();
        let rows: Vec<(String, i64)> = sqlx::query_as(
            r#"
            SELECT authorization.association_role, authorization.avionics_model_id
            FROM aircraft_sale_listing_avionics_authorizations authorization
            JOIN aircraft_sale_listing_avionics link
              ON link.id = authorization.listing_link_id
            WHERE link.aircraft_sale_listing_id = ?
            ORDER BY authorization.association_role
            "#,
        )
        .bind(fixture.listing_id)
        .fetch_all(pool(&fixture.db))
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                ("installed".to_string(), installed_id),
                ("replacement".to_string(), replacement_id),
            ]
        );
        let authorization_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM aircraft_sale_listing_avionics_authorizations authorization
            JOIN aircraft_sale_listing_avionics link
              ON link.id = authorization.listing_link_id
            WHERE link.aircraft_sale_listing_id = ?
            "#,
        )
        .bind(fixture.listing_id)
        .fetch_one(pool(&fixture.db))
        .await
        .unwrap();
        assert_eq!(authorization_count, 2);
    }

    #[tokio::test]
    async fn unattested_existing_link_is_not_silently_preserved_by_automation() {
        let fixture = fixture().await;
        let existing_id = insert_product(&fixture.db, "GNS 530W", "GNS530W", true).await;
        let accepted_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let pool = pool(&fixture.db);
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, source,
              source_confidence, configuration_action
            ) VALUES (?, ?, 'listing', 'high', 'installed')
            "#,
        )
        .bind(fixture.listing_id)
        .bind(existing_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(existing_id)
            .execute(pool)
            .await
            .unwrap();

        let error = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![accepted(&fixture.db, accepted_id).await],
                vec![],
            ),
        )
        .await
        .expect_err("an unattested historical link must keep automated review pending");
        assert!(matches!(
            error,
            ReviewError::Stale(message)
                if message.contains("preserved avionics catalog id")
                    && message.contains("neither current manufacturer-reuse nor same-case grounded authorization")
        ));
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT avionics_model_id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? ORDER BY avionics_model_id",
        )
        .bind(fixture.listing_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(ids, vec![existing_id]);
        let review_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(review_count, 1);
    }
}
