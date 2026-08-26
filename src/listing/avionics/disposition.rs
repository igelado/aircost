//! Immutable terminal receipts for current-schema listing occurrences.
//!
//! A receipt deliberately stores no observed maker/model text and no provider
//! research. Its identity is derived from the exact retained extraction plus
//! the array slot and component role. Pending observations remain represented
//! by the pending-review bundle and must not receive a terminal receipt.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::avionics::model::avionics_identities_are_typography_exact;
use crate::db::{AppDb, DatabaseBackend};
use crate::listing::avionics::extraction::{
    validate_current_avionics_extraction, CurrentAvionicsExtraction,
};
use crate::listing::review::ReviewAspectId;

pub(crate) const DISPOSITION_POLICY_VERSION: &str = "avionics_occurrence_v1";
const EXTRACTION_HASH_DOMAIN: &[u8] = b"aircost:listing-avionics-extraction:v1\0";
const OCCURRENCE_HASH_DOMAIN: &[u8] = b"aircost:listing-avionics-occurrence:v1\0";

pub(crate) const INSERT_DISPOSITION_SQL: &str = r#"
    INSERT INTO aircraft_sale_listing_avionics_dispositions (
      aircraft_sale_listing_id,
      plugin_submission_id,
      extraction_sha256,
      occurrence_index,
      occurrence_role,
      occurrence_fingerprint,
      outcome,
      avionics_model_id,
      reason_code,
      decision_reason,
      decision_source,
      actor_user_id,
      policy_version
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT (
      plugin_submission_id, extraction_sha256, occurrence_index, occurrence_role
    ) DO NOTHING
"#;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum OccurrenceRole {
    Primary,
    Replacement,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AutomaticOccurrenceDisposition {
    pub(crate) occurrence_index: usize,
    pub(crate) occurrence_role: OccurrenceRole,
    pub(crate) outcome: &'static str,
    pub(crate) avionics_model_id: Option<i64>,
    pub(crate) reason_code: &'static str,
    pub(crate) decision_reason: &'static str,
}

impl AutomaticOccurrenceDisposition {
    pub(crate) fn linked(
        occurrence_index: usize,
        occurrence_role: OccurrenceRole,
        avionics_model_id: i64,
    ) -> Self {
        Self {
            occurrence_index,
            occurrence_role,
            outcome: "linked",
            avionics_model_id: Some(avionics_model_id),
            reason_code: "automatic_verified_product",
            decision_reason:
                "Automatic resolution linked this occurrence to a verified catalog product.",
        }
    }

    pub(crate) fn discarded(occurrence_index: usize, occurrence_role: OccurrenceRole) -> Self {
        Self {
            occurrence_index,
            occurrence_role,
            outcome: "discarded",
            avionics_model_id: None,
            reason_code: "automatic_identity_rejected",
            decision_reason:
                "Automatic identity resolution classified this occurrence as non-product input.",
        }
    }
}

impl OccurrenceRole {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Replacement => "replacement",
        }
    }
}

pub(crate) fn extraction_sha256(extracted_listing_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(EXTRACTION_HASH_DOMAIN);
    hasher.update(extracted_listing_json.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(crate) fn occurrence_fingerprint(
    extraction_sha256: &str,
    occurrence_index: usize,
    role: OccurrenceRole,
) -> Result<String, String> {
    if extraction_sha256.len() != 64
        || !extraction_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("avionics occurrence extraction hash is invalid".to_string());
    }
    let mut hasher = Sha256::new();
    hasher.update(OCCURRENCE_HASH_DOMAIN);
    hasher.update(extraction_sha256.as_bytes());
    hasher.update(b"\0");
    hasher.update(occurrence_index.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(role.as_str().as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

/// Decode only IDs emitted by the current extraction/review projector. Legacy
/// integer IDs and reviewer-created opaque IDs cannot be rebound to a source
/// occurrence by guessing.
pub(crate) fn coordinates_from_aspect_id(
    aspect_id: &ReviewAspectId,
) -> Option<(usize, OccurrenceRole)> {
    let ReviewAspectId::String(value) = aspect_id else {
        return None;
    };
    let mut fields = value.split(':');
    if fields.next() != Some("avionics") {
        return None;
    }
    let index = fields.next()?.parse::<usize>().ok()?;
    let role = match fields.next()? {
        "primary" => OccurrenceRole::Primary,
        "replacement" => OccurrenceRole::Replacement,
        _ => return None,
    };
    fields.next().is_none().then_some((index, role))
}

pub(crate) fn bounded_decision_reason(reason: &str) -> Result<&str, String> {
    let reason = reason.trim();
    if reason.is_empty() || reason.len() > 500 {
        return Err("avionics occurrence decision reason must contain 1 to 500 bytes".to_string());
    }
    Ok(reason)
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct OccurrenceDispositionReconciliation {
    pub listing_id: i64,
    pub submission_id: i64,
    pub occurrence_component_count: usize,
    pub pending_count: usize,
    pub linked_count: usize,
    pub discarded_count: usize,
    pub already_recorded_count: usize,
    pub unknown_count: usize,
    pub dry_run: bool,
}

#[derive(Debug, FromRow)]
struct ReconciliationCaptureRow {
    listing_owner_user_id: i64,
    listing_source_url: Option<String>,
    submission_owner_user_id: i64,
    submission_canonical_listing_id: Option<i64>,
    submission_source_url: String,
    rendered_html: String,
    rendered_html_sha256: String,
    extracted_listing_json: Option<String>,
    extraction_error: Option<String>,
    pending_submission_id: Option<i64>,
    review_payload_json: Option<String>,
}

#[derive(Debug, FromRow)]
struct ReconciliationLinkRow {
    avionics_model_id: i64,
    manufacturer: String,
    model: String,
    quantity: i64,
    source_notes: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    replacement_manufacturer: Option<String>,
    replacement_model: Option<String>,
}

#[derive(Debug, FromRow)]
struct ExistingDispositionRow {
    occurrence_index: i64,
    occurrence_role: String,
}

/// Persist the resolver's exact action graph after the capture has been bound
/// and any pending review has been attached. This path never rematches raw
/// listing typography to canonical catalog labels.
pub(crate) async fn record_automatic_occurrence_dispositions(
    db: &AppDb,
    listing_id: i64,
    submission_id: i64,
    actor_user_id: i64,
    decisions: &[AutomaticOccurrenceDisposition],
) -> Result<OccurrenceDispositionReconciliation, String> {
    let capture = load_reconciliation_capture(db, listing_id, submission_id).await?;
    if actor_user_id != capture.listing_owner_user_id {
        return Err("occurrence disposition actor is not the listing owner".to_string());
    }
    if capture.extraction_error.is_some() {
        return Err("retained capture has an extraction error".to_string());
    }
    let extracted_listing_json = capture
        .extracted_listing_json
        .as_deref()
        .ok_or_else(|| "retained capture has no extraction checkpoint".to_string())?;
    let occurrences = validate_current_avionics_extraction(CurrentAvionicsExtraction {
        listing_id,
        listing_owner_user_id: capture.listing_owner_user_id,
        listing_source_url: capture.listing_source_url.as_deref(),
        submission_id,
        submission_owner_user_id: capture.submission_owner_user_id,
        submission_canonical_listing_id: capture.submission_canonical_listing_id,
        submission_source_url: &capture.submission_source_url,
        rendered_html: &capture.rendered_html,
        rendered_html_sha256: &capture.rendered_html_sha256,
        extracted_listing_json,
    })
    .map_err(|error| error.to_string())?;
    if capture
        .pending_submission_id
        .is_some_and(|id| id != submission_id)
    {
        return Err("pending review is bound to a different retained capture".to_string());
    }
    let pending_ids = pending_aspect_ids(capture.review_payload_json.as_deref())?;
    let pending = pending_ids
        .iter()
        .filter_map(|id| coordinates_from_aspect_id(&ReviewAspectId::String(id.clone())))
        .collect::<HashSet<_>>();
    let expected = occurrences
        .iter()
        .enumerate()
        .flat_map(|(index, occurrence)| {
            let mut keys = vec![(index, OccurrenceRole::Primary)];
            if occurrence.replaces.is_some() {
                keys.push((index, OccurrenceRole::Replacement));
            }
            keys
        })
        .collect::<HashSet<_>>();
    let mut terminal = HashSet::new();
    for decision in decisions {
        let key = (decision.occurrence_index, decision.occurrence_role);
        if !expected.contains(&key) {
            return Err(
                "automatic disposition names a nonexistent occurrence component".to_string(),
            );
        }
        if !terminal.insert(key) {
            return Err("automatic resolver emitted duplicate occurrence dispositions".to_string());
        }
        if pending.contains(&key) {
            return Err("one occurrence component cannot be both pending and terminal".to_string());
        }
        if (decision.outcome == "linked") != decision.avionics_model_id.is_some()
            || !matches!(decision.outcome, "linked" | "discarded")
        {
            return Err("automatic resolver emitted an invalid terminal disposition".to_string());
        }
    }
    if !pending.is_subset(&expected) {
        return Err("pending review names a nonexistent occurrence component".to_string());
    }
    let covered = pending.union(&terminal).copied().collect::<HashSet<_>>();
    if covered != expected {
        return Err(
            "automatic resolution did not classify every occurrence component as linked, discarded, or pending"
                .to_string(),
        );
    }

    let extraction_hash = extraction_sha256(extracted_listing_json);
    let existing = load_existing_dispositions(db, submission_id, &extraction_hash).await?;
    if !existing.is_empty() {
        return Err("fresh occurrence action graph already has terminal receipts".to_string());
    }
    let mut report = OccurrenceDispositionReconciliation {
        listing_id,
        submission_id,
        occurrence_component_count: expected.len(),
        pending_count: pending.len(),
        dry_run: false,
        ..OccurrenceDispositionReconciliation::default()
    };
    let rows = decisions
        .iter()
        .map(|decision| {
            Ok((
                decision,
                occurrence_fingerprint(
                    &extraction_hash,
                    decision.occurrence_index,
                    decision.occurrence_role,
                )?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    for decision in decisions {
        if decision.outcome == "linked" {
            report.linked_count += 1;
        } else {
            report.discarded_count += 1;
        }
    }
    let sql = db.sql(INSERT_DISPOSITION_SQL);
    macro_rules! insert_graph_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await.map_err(|error| error.to_string())?;
            for (decision, fingerprint) in &rows {
                let changed = sqlx::query(&sql)
                    .bind(listing_id)
                    .bind(submission_id)
                    .bind(&extraction_hash)
                    .bind(decision.occurrence_index as i64)
                    .bind(decision.occurrence_role.as_str())
                    .bind(fingerprint)
                    .bind(decision.outcome)
                    .bind(decision.avionics_model_id)
                    .bind(decision.reason_code)
                    .bind(decision.decision_reason)
                    .bind("automatic")
                    .bind(actor_user_id)
                    .bind(DISPOSITION_POLICY_VERSION)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| error.to_string())?
                    .rows_affected();
                if changed != 1 {
                    return Err("occurrence already has a terminal disposition".to_string());
                }
            }
            transaction
                .commit()
                .await
                .map_err(|error| error.to_string())?;
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => insert_graph_transaction!(pool),
        DatabaseBackend::Postgres(pool) => insert_graph_transaction!(pool),
    }
    Ok(report)
}

/// Provider-free reconciliation for an already-bound current extraction. It
/// is deliberately an audit/backfill path: absence never means discard, and a
/// link is inferred only from an exact, unambiguous retained association.
pub(crate) async fn reconcile_current_occurrence_dispositions(
    db: &AppDb,
    listing_id: i64,
    submission_id: i64,
    actor_user_id: i64,
    apply: bool,
) -> Result<OccurrenceDispositionReconciliation, String> {
    let capture = load_reconciliation_capture(db, listing_id, submission_id).await?;
    if actor_user_id != capture.listing_owner_user_id {
        return Err("occurrence disposition actor is not the listing owner".to_string());
    }
    if capture.extraction_error.is_some() {
        return Err("retained capture has an extraction error".to_string());
    }
    let extracted_listing_json = capture
        .extracted_listing_json
        .as_deref()
        .ok_or_else(|| "retained capture has no extraction checkpoint".to_string())?;
    let occurrences = validate_current_avionics_extraction(CurrentAvionicsExtraction {
        listing_id,
        listing_owner_user_id: capture.listing_owner_user_id,
        listing_source_url: capture.listing_source_url.as_deref(),
        submission_id,
        submission_owner_user_id: capture.submission_owner_user_id,
        submission_canonical_listing_id: capture.submission_canonical_listing_id,
        submission_source_url: &capture.submission_source_url,
        rendered_html: &capture.rendered_html,
        rendered_html_sha256: &capture.rendered_html_sha256,
        extracted_listing_json,
    })
    .map_err(|error| error.to_string())?;
    if capture
        .pending_submission_id
        .is_some_and(|id| id != submission_id)
    {
        return Err("pending review is bound to a different retained capture".to_string());
    }
    let pending = pending_aspect_ids(capture.review_payload_json.as_deref())?;
    let links = load_reconciliation_links(db, listing_id).await?;
    let extraction_hash = extraction_sha256(extracted_listing_json);
    let existing = load_existing_dispositions(db, submission_id, &extraction_hash).await?;
    let mut report = OccurrenceDispositionReconciliation {
        listing_id,
        submission_id,
        dry_run: !apply,
        ..OccurrenceDispositionReconciliation::default()
    };

    for (index, occurrence) in occurrences.iter().enumerate() {
        for role in [OccurrenceRole::Primary, OccurrenceRole::Replacement] {
            if role == OccurrenceRole::Replacement && occurrence.replaces.is_none() {
                continue;
            }
            report.occurrence_component_count += 1;
            if existing.contains(&(index as i64, role.as_str().to_string())) {
                report.already_recorded_count += 1;
                continue;
            }
            let aspect_id = format!("avionics:{index}:{}", role.as_str());
            if pending.contains(&aspect_id) {
                report.pending_count += 1;
                continue;
            }
            let candidates = links
                .iter()
                .filter(|link| link_matches_occurrence_component(link, occurrence, role))
                .collect::<Vec<_>>();
            let terminal = match candidates.as_slice() {
                [link] => Some((
                    "linked",
                    Some(match role {
                        OccurrenceRole::Primary => link.avionics_model_id,
                        OccurrenceRole::Replacement => link
                            .replaces_avionics_model_id
                            .expect("replacement match requires a replacement product"),
                    }),
                    "automatic_verified_product",
                    "Automatic resolution linked this occurrence to a verified catalog product.",
                )),
                _ => None,
            };
            let Some((outcome, product_id, reason_code, decision_reason)) = terminal else {
                report.unknown_count += 1;
                continue;
            };
            if outcome == "linked" {
                report.linked_count += 1;
            } else {
                report.discarded_count += 1;
            }
            if apply {
                insert_disposition(
                    db,
                    listing_id,
                    submission_id,
                    &extraction_hash,
                    index,
                    role,
                    outcome,
                    product_id,
                    reason_code,
                    decision_reason,
                    actor_user_id,
                )
                .await?;
            }
        }
    }
    Ok(report)
}

pub async fn reconcile_bound_occurrence_dispositions(
    db: &AppDb,
    listing_id: i64,
    submission_id: i64,
    actor_user_id: i64,
    apply: bool,
) -> Result<OccurrenceDispositionReconciliation, String> {
    reconcile_current_occurrence_dispositions(db, listing_id, submission_id, actor_user_id, apply)
        .await
}

async fn load_reconciliation_capture(
    db: &AppDb,
    listing_id: i64,
    submission_id: i64,
) -> Result<ReconciliationCaptureRow, String> {
    let sql = db.sql(
        r#"
        SELECT listing.created_by_user_id AS listing_owner_user_id,
               listing.source_url AS listing_source_url,
               submission.user_id AS submission_owner_user_id,
               submission.canonical_listing_id AS submission_canonical_listing_id,
               submission.source_url AS submission_source_url,
               submission.rendered_html,
               submission.rendered_html_sha256,
               submission.extracted_listing_json,
               submission.extraction_error,
               review.plugin_submission_id AS pending_submission_id,
               review.review_payload_json
        FROM aircraft_sale_listings listing
        JOIN plugin_submissions submission ON submission.id = ?
        LEFT JOIN aircraft_sale_listing_pending_reviews review
          ON review.listing_id = listing.id
        WHERE listing.id = ?
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ReconciliationCaptureRow>(&sql)
                .bind(submission_id)
                .bind(listing_id)
                .fetch_optional(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ReconciliationCaptureRow>(&sql)
                .bind(submission_id)
                .bind(listing_id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|error| error.to_string())?;
    row.ok_or_else(|| "listing or retained submission was not found".to_string())
}

fn pending_aspect_ids(review_payload_json: Option<&str>) -> Result<HashSet<String>, String> {
    let Some(payload) = review_payload_json else {
        return Ok(HashSet::new());
    };
    let value: Value = serde_json::from_str(payload)
        .map_err(|error| format!("pending review JSON is invalid: {error}"))?;
    let aspects = value
        .get("aspects")
        .and_then(Value::as_array)
        .ok_or_else(|| "pending review has no aspects array".to_string())?;
    Ok(aspects
        .iter()
        .filter_map(|aspect| aspect.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect())
}

async fn load_reconciliation_links(
    db: &AppDb,
    listing_id: i64,
) -> Result<Vec<ReconciliationLinkRow>, String> {
    let sql = db.sql(
        r#"
        SELECT link.avionics_model_id,
               manufacturer.name AS manufacturer,
               model.name AS model,
               link.quantity,
               link.source_notes,
               link.configuration_action,
               link.replaces_avionics_model_id,
               replacement_manufacturer.name AS replacement_manufacturer,
               replacement.name AS replacement_model
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model ON model.id = link.avionics_model_id
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        LEFT JOIN avionics_models replacement
          ON replacement.id = link.replaces_avionics_model_id
        LEFT JOIN avionics_manufacturers replacement_manufacturer
          ON replacement_manufacturer.id = replacement.avionics_manufacturer_id
        WHERE link.aircraft_sale_listing_id = ?
        ORDER BY link.id
        "#,
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ReconciliationLinkRow>(&sql)
                .bind(listing_id)
                .fetch_all(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ReconciliationLinkRow>(&sql)
                .bind(listing_id)
                .fetch_all(pool)
                .await
        }
    }
    .map_err(|error| error.to_string())
}

fn link_matches_occurrence_component(
    link: &ReconciliationLinkRow,
    occurrence: &crate::models::ParsedAvionics,
    role: OccurrenceRole,
) -> bool {
    let primary_matches = avionics_identities_are_typography_exact(
        &link.manufacturer,
        &link.model,
        &occurrence.manufacturer,
        &occurrence.model,
    ) && link.quantity == occurrence.quantity
        && link.configuration_action == occurrence.configuration_action
        && link.source_notes.as_deref() == occurrence.source_evidence_text.as_deref();
    if !primary_matches {
        return false;
    }
    match role {
        OccurrenceRole::Primary => {
            link.replaces_avionics_model_id.is_some() == occurrence.replaces.is_some()
        }
        OccurrenceRole::Replacement => occurrence.replaces.as_ref().is_some_and(|replacement| {
            link.replaces_avionics_model_id.is_some()
                && avionics_identities_are_typography_exact(
                    link.replacement_manufacturer.as_deref().unwrap_or_default(),
                    link.replacement_model.as_deref().unwrap_or_default(),
                    &replacement.manufacturer,
                    &replacement.model,
                )
        }),
    }
}

async fn load_existing_dispositions(
    db: &AppDb,
    submission_id: i64,
    extraction_sha256: &str,
) -> Result<HashSet<(i64, String)>, String> {
    let sql = db.sql(
        r#"
        SELECT occurrence_index, occurrence_role
        FROM aircraft_sale_listing_avionics_dispositions
        WHERE plugin_submission_id = ? AND extraction_sha256 = ?
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ExistingDispositionRow>(&sql)
                .bind(submission_id)
                .bind(extraction_sha256)
                .fetch_all(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ExistingDispositionRow>(&sql)
                .bind(submission_id)
                .bind(extraction_sha256)
                .fetch_all(pool)
                .await
        }
    }
    .map_err(|error| error.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| (row.occurrence_index, row.occurrence_role))
        .collect())
}

#[allow(clippy::too_many_arguments)]
async fn insert_disposition(
    db: &AppDb,
    listing_id: i64,
    submission_id: i64,
    extraction_sha256: &str,
    occurrence_index: usize,
    occurrence_role: OccurrenceRole,
    outcome: &str,
    avionics_model_id: Option<i64>,
    reason_code: &str,
    decision_reason: &str,
    actor_user_id: i64,
) -> Result<(), String> {
    let fingerprint = occurrence_fingerprint(extraction_sha256, occurrence_index, occurrence_role)?;
    let sql = db.sql(INSERT_DISPOSITION_SQL);
    let changed = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query(&sql)
            .bind(listing_id)
            .bind(submission_id)
            .bind(extraction_sha256)
            .bind(occurrence_index as i64)
            .bind(occurrence_role.as_str())
            .bind(fingerprint)
            .bind(outcome)
            .bind(avionics_model_id)
            .bind(reason_code)
            .bind(decision_reason)
            .bind("automatic")
            .bind(actor_user_id)
            .bind(DISPOSITION_POLICY_VERSION)
            .execute(pool)
            .await
            .map(|result| result.rows_affected()),
        DatabaseBackend::Postgres(pool) => sqlx::query(&sql)
            .bind(listing_id)
            .bind(submission_id)
            .bind(extraction_sha256)
            .bind(occurrence_index as i64)
            .bind(occurrence_role.as_str())
            .bind(fingerprint)
            .bind(outcome)
            .bind(avionics_model_id)
            .bind(reason_code)
            .bind(decision_reason)
            .bind("automatic")
            .bind(actor_user_id)
            .bind(DISPOSITION_POLICY_VERSION)
            .execute(pool)
            .await
            .map(|result| result.rows_affected()),
    }
    .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("occurrence already has a terminal disposition".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppDb;

    #[test]
    fn occurrence_identity_is_bound_to_extraction_slot_and_role() {
        let extraction = extraction_sha256(r#"{"avionics":[]}"#);
        assert_ne!(
            occurrence_fingerprint(&extraction, 0, OccurrenceRole::Primary).unwrap(),
            occurrence_fingerprint(&extraction, 0, OccurrenceRole::Replacement).unwrap()
        );
        assert_ne!(
            occurrence_fingerprint(&extraction, 0, OccurrenceRole::Primary).unwrap(),
            occurrence_fingerprint(&extraction, 1, OccurrenceRole::Primary).unwrap()
        );
    }

    #[test]
    fn only_current_projector_ids_decode() {
        assert_eq!(
            coordinates_from_aspect_id(&"avionics:12:replacement".into()),
            Some((12, OccurrenceRole::Replacement))
        );
        assert_eq!(coordinates_from_aspect_id(&12_i64.into()), None);
        assert_eq!(coordinates_from_aspect_id(&"legacy:12".into()), None);
        assert_eq!(
            coordinates_from_aspect_id(&"avionics:12:primary:x".into()),
            None
        );
    }

    #[tokio::test]
    async fn automatic_graph_insert_rolls_back_all_receipts_on_mid_graph_failure() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let user_id: i64 = sqlx::query_scalar("SELECT id FROM users ORDER BY id LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap();
        let variant_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let source = "https://example.test/atomic-dispositions";
        let listing_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO aircraft_sale_listings
               (aircraft_model_variant_id, created_by_user_id, source_url, model_year,
                asking_price_usd, airframe_hours)
               VALUES (?, ?, ?, 2020, 100000, 500) RETURNING id"#,
        )
        .bind(variant_id)
        .bind(user_id)
        .bind(source)
        .fetch_one(pool)
        .await
        .unwrap();
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'key') RETURNING id",
        )
        .bind(user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let html = "<p>Garmin G5 installed</p><p>Garmin GTX 345 installed</p>";
        let html_hash = crate::plugin::sha256_hex(html.as_bytes());
        let extracted = serde_json::json!({
            "avionics": [
                {"manufacturer":"Garmin","model":"G5","types":["Flight Display"],"quantity":1,"configuration_action":"installed","replaces":null,"source_evidence_text":"Garmin G5 installed","source_confidence":"high"},
                {"manufacturer":"Garmin","model":"GTX 345","types":["Transponder"],"quantity":1,"configuration_action":"installed","replaces":null,"source_evidence_text":"Garmin GTX 345 installed","source_confidence":"high"}
            ]
        })
        .to_string();
        let submission_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_submissions
               (user_id, plugin_install_id, source_url, rendered_html, rendered_html_sha256,
                signature_base64, extracted_listing_json, canonical_listing_id)
               VALUES (?, ?, ?, ?, ?, 'sig', ?, ?) RETURNING id"#,
        )
        .bind(user_id)
        .bind(install_id)
        .bind(source)
        .bind(html)
        .bind(html_hash)
        .bind(extracted)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"CREATE TRIGGER fail_second_disposition
               BEFORE INSERT ON aircraft_sale_listing_avionics_dispositions
               WHEN NEW.occurrence_index = 1
               BEGIN SELECT RAISE(ABORT, 'induced second receipt failure'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();
        let error = record_automatic_occurrence_dispositions(
            &db,
            listing_id,
            submission_id,
            user_id,
            &[
                AutomaticOccurrenceDisposition::discarded(0, OccurrenceRole::Primary),
                AutomaticOccurrenceDisposition::discarded(1, OccurrenceRole::Primary),
            ],
        )
        .await
        .expect_err("the induced second insert must fail");
        assert!(error.contains("induced second receipt failure"));
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM aircraft_sale_listing_avionics_dispositions")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(count, 0, "the first receipt must roll back with the second");
    }
}
