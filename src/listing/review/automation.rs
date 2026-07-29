//! Atomic persistence boundary for automated listing-avionics review.
//!
//! The model/API work that proposes accepted links intentionally happens
//! outside this module. This boundary revalidates every mutable dependency and
//! either applies the complete accepted/residual result or commits nothing.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::aircraft::faa::{normalize_n_number, normalize_serial_key};
use crate::avionics::reuse::{
    reuse_attestation_is_current_postgres, reuse_attestation_is_current_sqlite,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::listing::avionics::{
    approved_avionics_product_key, validate_canonical_avionics_actions, CanonicalAvionicsAction,
};

use super::{
    catalog_products, conservative_confidence, fingerprint_catalog_products, merged_notes,
    parse_payload, serialize_review_payload, sha256_hex, valid_sha256, CatalogFingerprintRow,
    PendingReviewAspect, ReviewError, ReviewResult, APPROVED_CATALOG_ROWS_SQL,
    POSTGRES_LISTING_CHILD_LOCK_SQL,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct AutomatedAvionicsLink {
    pub avionics_model_id: i64,
    pub quantity: i64,
    pub source_notes: Option<String>,
    pub source_confidence: Option<String>,
    pub configuration_action: String,
    pub replaces_avionics_model_id: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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

#[derive(Clone, Debug)]
struct CatalogGraphIdentity {
    key: String,
}

#[derive(Clone, Debug)]
struct PreparedLink {
    avionics_model_id: i64,
    subject_key: String,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    replacement_key: Option<String>,
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
        match link.configuration_action.as_str() {
            "installed" if link.replaces_avionics_model_id.is_none() => {}
            "replaces" | "removes" if link.replaces_avionics_model_id.is_some_and(|id| id > 0) => {}
            _ => {
                return Err(ReviewError::Validation(format!(
                    "accepted avionics catalog id {} has invalid action/target semantics",
                    link.avionics_model_id
                )))
            }
        }
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
          graph.canonical_product_key
        FROM avionics_models model
        JOIN avionics_approved_product_graph_identities graph
          ON graph.avionics_model_id = model.id
        WHERE model.id = ?
          AND model.catalog_status = 'approved'
        "#,
    );
    let select_existing_links = db.sql(
        r#"
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
        "#,
    );
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
            let catalog_revision_sha256 =
                fingerprint_catalog_products(&catalog_products(catalog_rows));

            let mut identity_cache = BTreeMap::<i64, CatalogGraphIdentity>::new();
            let mut required_model_ids = BTreeSet::new();
            for link in &request.accepted_links {
                required_model_ids.insert(link.avionics_model_id);
                if let Some(target) = link.replaces_avionics_model_id {
                    required_model_ids.insert(target);
                }
            }
            for model_id in required_model_ids {
                if !$reuse_is_current(db, &mut transaction, model_id).await? {
                    return Err(ReviewError::Stale(format!(
                        "accepted avionics catalog id {model_id} is not eligible for current-policy reuse; ground and re-attest it before automated linking"
                    )));
                }
                let identity: Option<(i64, String)> =
                    sqlx::query_as(&select_graph_identity)
                        .bind(model_id)
                        .fetch_optional(&mut *transaction)
                        .await?;
                let (manufacturer_identity_id, product_key) =
                    identity.ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "accepted avionics catalog id {model_id} is missing, unapproved, or lacks a stable graph identity"
                        ))
                    })?;
                identity_cache.insert(
                    model_id,
                    CatalogGraphIdentity {
                        key: approved_avionics_product_key(
                            manufacturer_identity_id,
                            &product_key,
                        )
                        .map_err(ReviewError::Stale)?,
                    },
                );
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
                    subject_key: subject.key.clone(),
                    quantity: link.quantity,
                    source: "listing".to_string(),
                    source_notes: link.source_notes.clone(),
                    // Validation requires high, but persist the exact supplied
                    // value rather than manufacturing corroboration here.
                    source_confidence: link.source_confidence.clone(),
                    configuration_action: link.configuration_action.clone(),
                    replaces_avionics_model_id: link.replaces_avionics_model_id,
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
            let mut preserved = BTreeMap::<String, PreparedLink>::new();
            for row in &existing_rows {
                if row.quantity <= 0
                    || row.source_confidence.as_deref() != Some("high")
                    || !matches!(row.source.as_str(), "listing" | "listing_review")
                    || row.installed_catalog_status.as_deref() != Some("approved")
                {
                    continue;
                }
                if !$reuse_is_current(db, &mut transaction, row.avionics_model_id).await? {
                    return Err(ReviewError::Stale(format!(
                        "preserved avionics catalog id {} is not eligible for current-policy reuse; ground and re-attest it before automated review",
                        row.avionics_model_id
                    )));
                }
                let subject_key = graph_key(
                    row.installed_manufacturer_identity_id,
                    row.installed_product_key.as_deref(),
                    row.avionics_model_id,
                )?;
                let replacement_key = if let Some(target_id) =
                    row.replaces_avionics_model_id
                {
                    if row.replacement_catalog_status.as_deref() != Some("approved") {
                        continue;
                    }
                    if !$reuse_is_current(db, &mut transaction, target_id).await? {
                        return Err(ReviewError::Stale(format!(
                            "preserved replacement catalog id {target_id} is not eligible for current-policy reuse; ground and re-attest it before automated review"
                        )));
                    }
                    Some(graph_key(
                        row.replacement_manufacturer_identity_id,
                        row.replacement_product_key.as_deref(),
                        target_id,
                    )?)
                } else {
                    None
                };
                let valid_shape = match row.configuration_action.as_str() {
                    "installed" => replacement_key.is_none(),
                    "replaces" | "removes" => replacement_key.is_some(),
                    _ => false,
                };
                if !valid_shape
                    || touched_keys.contains(&subject_key)
                    || replacement_key
                        .as_ref()
                        .is_some_and(|key| touched_keys.contains(key))
                {
                    continue;
                }
                let incoming = PreparedLink {
                    avionics_model_id: row.avionics_model_id,
                    subject_key: subject_key.clone(),
                    quantity: row.quantity,
                    source: row.source.clone(),
                    source_notes: row.source_notes.clone(),
                    source_confidence: row.source_confidence.clone(),
                    configuration_action: row.configuration_action.clone(),
                    replaces_avionics_model_id: row.replaces_avionics_model_id,
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
            preserved.extend(accepted);
            let assignments = preserved;
            validate_canonical_avionics_actions(
                &assignments.values().map(prepared_action).collect::<Vec<_>>(),
            )
            .map_err(ReviewError::Validation)?;

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
              engine_member_name, engine_member_sha256
            ) VALUES (
              ?, '2026-07-23',
              'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
              ?, ?, ?, 'MASTER.txt', ?, 'ACFTREF.txt', ?, 'ENGINE.txt', ?
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
        let rendered_html = "<html><body>Garmin avionics</body></html>";
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

    fn accepted(model_id: i64) -> AutomatedAvionicsLink {
        AutomatedAvionicsLink {
            avionics_model_id: model_id,
            quantity: 1,
            source_notes: Some("Exact model appears in listing equipment".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
        }
    }

    #[tokio::test]
    async fn partial_success_persists_accepted_links_and_only_residual_review() {
        let fixture = fixture().await;
        let accepted_id = insert_product(&fixture.db, "GTN 750Xi", "GTN750XI", true).await;
        let residual = pending_aspect("residual:0", "Unclear audio panel");
        // Legacy retained submissions may predate canonical_listing_id. Exact
        // nonblank source URL equality remains an ownership-scoped binding.
        sqlx::query("UPDATE plugin_submissions SET canonical_listing_id = NULL WHERE id = ?")
            .bind(fixture.submission_id)
            .execute(pool(&fixture.db))
            .await
            .unwrap();
        let result = apply_automated_avionics_review(
            &fixture.db,
            &request(
                &fixture,
                vec![accepted(accepted_id)],
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
    }

    #[tokio::test]
    async fn all_pass_clears_review_but_never_marks_listing_ready() {
        let fixture = fixture().await;
        let accepted_id = insert_product(&fixture.db, "GTX 345", "GTX345", true).await;
        let result = apply_automated_avionics_review(
            &fixture.db,
            &request(&fixture, vec![accepted(accepted_id)], vec![]),
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
            ) VALUES (?, ?, 1, 'listing', 'Exact model appears in listing equipment',
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
            INSERT INTO aircraft_sale_listing_avionics_corroborations (
              listing_link_id, association_role, avionics_model_id,
              observation_sha256, product_fingerprint, policy_version
            ) VALUES (?, 'installed', ?, ?, ?, 'listing_avionics_association_v1')
            "#,
        )
        .bind(existing_link_id)
        .bind(accepted_id)
        .bind("1".repeat(64))
        .bind(&product_fingerprint)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics_corroboration_scopes (
              listing_link_id, association_role, collision_closure_sha256, policy_version
            ) VALUES (?, 'installed', ?, 'listing_avionics_collision_closure_v1')
            "#,
        )
        .bind(existing_link_id)
        .bind("2".repeat(64))
        .execute(pool)
        .await
        .unwrap();

        let result = apply_automated_avionics_review(
            &fixture.db,
            &request(&fixture, vec![accepted(accepted_id)], vec![]),
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
        let corroboration_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_corroborations WHERE listing_link_id = ?",
        )
        .bind(existing_link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let scope_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_corroboration_scopes WHERE listing_link_id = ?",
        )
        .bind(existing_link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!((corroboration_count, scope_count), (1, 1));
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
            ) VALUES (?, ?, 2, 'listing', 'Exact model appears in listing equipment',
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
            INSERT INTO aircraft_sale_listing_avionics_corroborations (
              listing_link_id, association_role, avionics_model_id,
              observation_sha256, product_fingerprint, policy_version
            ) VALUES (?, 'installed', ?, ?, ?, 'listing_avionics_association_v1')
            "#,
        )
        .bind(existing_link_id)
        .bind(accepted_id)
        .bind("1".repeat(64))
        .bind(product_fingerprint)
        .execute(pool)
        .await
        .unwrap();

        apply_automated_avionics_review(
            &fixture.db,
            &request(&fixture, vec![accepted(accepted_id)], vec![]),
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
        let corroboration_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_corroborations WHERE listing_link_id = ?",
        )
        .bind(existing_link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(corroboration_count, 0);
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
        let mut stale = request(&fixture, vec![accepted(accepted_id)], vec![]);
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
            &request(&fixture, vec![accepted(accepted_id)], vec![]),
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
                &request(&fixture, vec![accepted(unapproved_id)], vec![])
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
            &request(&fixture, vec![accepted(accepted_id)], vec![]),
        )
        .await
        .expect_err("stale reuse eligibility must be checked at the link-write boundary");
        assert!(matches!(
            error,
            ReviewError::Stale(message)
                if message.contains("not eligible for current-policy reuse")
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
            &request(&fixture, vec![accepted(accepted_id)], vec![]),
        )
        .await
        .expect_err("an unattested historical link must keep automated review pending");
        assert!(matches!(
            error,
            ReviewError::Stale(message)
                if message.contains("preserved avionics catalog id")
                    && message.contains("not eligible for current-policy reuse")
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
