//! Current authority for listing-derived avionics associations.
//!
//! Automatic associations are usable only while every installed and replacement
//! endpoint remains bound to the exact retained plugin capture and extraction
//! checkpoint that authorized it. Human-reviewed associations deliberately do
//! not require a machine authorization row.

use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};

use sqlx::{Connection, FromRow, Postgres, Sqlite, Transaction};

use crate::avionics::fingerprint::{
    active_collision_closure_revision_sha256_postgres,
    active_collision_closure_revision_sha256_sqlite, catalog_product_fingerprints,
    catalog_products, fingerprint_grounded_collision_closure, ActiveCollisionCatalogFingerprintRow,
    AvionicsFingerprintError, CatalogFingerprintRow, ACTIVE_COLLISION_CATALOG_ROWS_SQL,
    APPROVED_CATALOG_ROWS_SQL,
};
use crate::avionics::reuse::{
    countable_unit_reuse_attestation_is_current_postgres,
    countable_unit_reuse_attestation_is_current_sqlite, reuse_attestation_is_current_postgres,
    reuse_attestation_is_current_sqlite,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::listing::replay::retained_capture_timestamp_chronology_valid;
use crate::listing::review::{association_observation_sha256_from_values, ListingAssociationRole};
use crate::plugin::{current_checkpoint_contains_avionics_source_evidence, sha256_hex};

pub(crate) type AuthorizationResult<T> = Result<T, AuthorizationError>;

#[derive(Debug)]
pub(crate) enum AuthorizationError {
    Database(String),
}

impl Display for AuthorizationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AuthorizationError {}

impl From<sqlx::Error> for AuthorizationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ListingAuthorizationState {
    automatic_link_ids: HashSet<i64>,
    current_automatic_link_ids: HashSet<i64>,
}

impl ListingAuthorizationState {
    pub(crate) fn all_automatic_associations_current(&self) -> bool {
        self.automatic_link_ids == self.current_automatic_link_ids
    }

    pub(crate) fn automatic_link_is_current(&self, listing_link_id: i64) -> bool {
        self.current_automatic_link_ids.contains(&listing_link_id)
    }
}

#[derive(Clone, Debug, FromRow)]
struct AutomaticListingLinkRow {
    listing_id: i64,
    listing_link_id: i64,
    avionics_model_id: i64,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    installed_catalog_status: String,
    replacement_catalog_status: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct AssociationAuthorizationRow {
    listing_link_id: i64,
    association_role: String,
    avionics_model_id: i64,
    authorization_kind: String,
    observation_sha256: String,
    product_fingerprint: String,
    grounded_resolution_sha256: Option<String>,
    evidence_capture_sha256: String,
    plugin_submission_id: i64,
    extracted_listing_sha256: String,
    collision_closure_sha256: String,
    source_revocation_count: Option<i64>,
    rendered_html: Option<String>,
    extracted_listing_json: Option<String>,
    install_created_at: Option<String>,
    submitted_at: Option<String>,
    install_revoked_at: Option<String>,
    exact_submission_is_current: bool,
    current_reuse_product_fingerprint: Option<String>,
}

const AUTOMATIC_LISTING_LINKS_SQL: &str = r#"
    SELECT
      listing.id AS listing_id,
      link.id AS listing_link_id,
      link.avionics_model_id,
      link.quantity,
      link.source,
      link.source_notes,
      link.source_confidence,
      link.configuration_action,
      link.replaces_avionics_model_id,
      installed_model.catalog_status AS installed_catalog_status,
      replacement_model.catalog_status AS replacement_catalog_status
    FROM aircraft_sale_listing_avionics link
    JOIN aircraft_sale_listings listing
      ON listing.id = link.aircraft_sale_listing_id
    JOIN avionics_models installed_model
      ON installed_model.id = link.avionics_model_id
    LEFT JOIN avionics_models replacement_model
      ON replacement_model.id = link.replaces_avionics_model_id
    WHERE listing.id = ?
      AND link.source IN ('listing', 'listing_explicit_count')
    ORDER BY link.id
"#;

const ASSOCIATION_AUTHORIZATIONS_SQL: &str = r#"
    SELECT
      authorization.listing_link_id,
      authorization.association_role,
      authorization.avionics_model_id,
      authorization.authorization_kind,
      authorization.observation_sha256,
      authorization.product_fingerprint,
      authorization.grounded_resolution_sha256,
      authorization.evidence_capture_sha256,
      authorization.plugin_submission_id,
      authorization.extracted_listing_sha256,
      authorization.collision_closure_sha256,
      authorization.source_revocation_count,
      submission.rendered_html,
      submission.extracted_listing_json,
      install.created_at AS install_created_at,
      submission.submitted_at,
      install.revoked_at AS install_revoked_at,
      submission.id IS NOT NULL AND install.id IS NOT NULL
        AS exact_submission_is_current,
      current_attestation.product_fingerprint
        AS current_reuse_product_fingerprint
    FROM aircraft_sale_listing_avionics_link_authorizations authorization
    JOIN aircraft_sale_listing_avionics link
      ON link.id = authorization.listing_link_id
    JOIN aircraft_sale_listings listing
      ON listing.id = link.aircraft_sale_listing_id
    LEFT JOIN avionics_product_reuse_attestations current_attestation
      ON current_attestation.avionics_model_id = authorization.avionics_model_id
    LEFT JOIN plugin_submissions submission
      ON submission.id = authorization.plugin_submission_id
     AND submission.canonical_listing_id = listing.id
     AND submission.user_id = listing.created_by_user_id
     AND submission.source_url = listing.source_url
     AND submission.rendered_html_sha256 = authorization.evidence_capture_sha256
     AND submission.extracted_listing_json IS NOT NULL
     AND submission.extraction_error IS NULL
    LEFT JOIN plugin_installs install
      ON install.id = submission.plugin_install_id
     AND install.user_id = submission.user_id
    WHERE listing.id = ?
    ORDER BY authorization.listing_link_id, authorization.association_role
"#;

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn exact_checkpoint_is_current(
    authorization: &AssociationAuthorizationRow,
    evidence_text: &str,
) -> bool {
    if evidence_text.trim().is_empty() || !authorization.exact_submission_is_current {
        return false;
    }
    let (Some(rendered_html), Some(checkpoint), Some(install_created_at), Some(submitted_at)) = (
        authorization.rendered_html.as_deref(),
        authorization.extracted_listing_json.as_deref(),
        authorization.install_created_at.as_deref(),
        authorization.submitted_at.as_deref(),
    ) else {
        return false;
    };
    authorization.plugin_submission_id > 0
        && valid_sha256(&authorization.evidence_capture_sha256)
        && valid_sha256(&authorization.extracted_listing_sha256)
        && sha256_hex(rendered_html.as_bytes()) == authorization.evidence_capture_sha256
        && sha256_hex(checkpoint.as_bytes()) == authorization.extracted_listing_sha256
        && retained_capture_timestamp_chronology_valid(
            install_created_at,
            submitted_at,
            authorization.install_revoked_at.as_deref(),
        )
        && current_checkpoint_contains_avionics_source_evidence(checkpoint, evidence_text)
}

fn role_label(role: ListingAssociationRole) -> &'static str {
    match role {
        ListingAssociationRole::Installed => "installed",
        ListingAssociationRole::Replacement => "replacement",
    }
}

macro_rules! endpoint_authorization_is_current {
    (
        $db:expr,
        $transaction:expr,
        $link:expr,
        $role:expr,
        $target_id:expr,
        $authorization:expr,
        $catalog_product_fingerprints:expr,
        $active_collision_rows:expr,
        $source_revocation_count:expr,
        $reuse_is_current:path,
        $countable_is_current:path,
        $active_collision_revision:path
    ) => {{
        (async {
            let link = $link;
            let role = $role;
            let target_id = $target_id;
            let authorization = $authorization;
            let Some(source_notes) = link
                .source_notes
                .as_deref()
                .filter(|notes| !notes.trim().is_empty())
            else {
                return Ok(false);
            };
            if authorization.listing_link_id != link.listing_link_id
                || authorization.association_role != role_label(role)
                || authorization.avionics_model_id != target_id
                || !exact_checkpoint_is_current(authorization, source_notes)
                || authorization.observation_sha256
                    != association_observation_sha256_from_values(
                        link.listing_id,
                        link.listing_link_id,
                        role,
                        target_id,
                        link.avionics_model_id,
                        link.replaces_avionics_model_id,
                        link.quantity,
                        &link.configuration_action,
                        source_notes,
                    )
            {
                return Ok(false);
            }

            match authorization.authorization_kind.as_str() {
                "manufacturer_reuse" => {
                    if authorization.grounded_resolution_sha256.is_some()
                        || authorization.source_revocation_count.is_some()
                        || authorization.current_reuse_product_fingerprint.as_deref()
                            != Some(authorization.product_fingerprint.as_str())
                        || !$reuse_is_current($db, $transaction, target_id).await?
                        || (link.source == "listing_explicit_count"
                            && (role != ListingAssociationRole::Installed
                                || !$countable_is_current($db, $transaction, target_id).await?))
                    {
                        return Ok(false);
                    }
                    match $active_collision_revision($db, $transaction, target_id).await {
                        Ok(current_collision) => {
                            Ok(current_collision == authorization.collision_closure_sha256)
                        }
                        Err(AvionicsFingerprintError::Conflict(_)) => Ok(false),
                        Err(AvionicsFingerprintError::Database(message)) => {
                            Err(AuthorizationError::Database(message))
                        }
                    }
                }
                "same_case_grounded" => {
                    let current_product = $catalog_product_fingerprints.get(&target_id);
                    let current_collision =
                        fingerprint_grounded_collision_closure($active_collision_rows, target_id);
                    Ok(link.source != "listing_explicit_count"
                        && authorization
                            .grounded_resolution_sha256
                            .as_deref()
                            .is_some_and(valid_sha256)
                        && authorization.source_revocation_count == Some($source_revocation_count)
                        && current_product == Some(&authorization.product_fingerprint)
                        && current_collision.as_deref()
                            == Some(authorization.collision_closure_sha256.as_str()))
                }
                _ => Ok(false),
            }
        })
        .await
    }};
}

macro_rules! listing_authorization_state_in_transaction {
    (
        $db:expr,
        $transaction:expr,
        $listing_id:expr,
        $reuse_is_current:path,
        $countable_is_current:path,
        $active_collision_revision:path
    ) => {{
        let links_sql = $db.sql(AUTOMATIC_LISTING_LINKS_SQL);
        let links = sqlx::query_as::<_, AutomaticListingLinkRow>(&links_sql)
            .bind($listing_id)
            .fetch_all(&mut **$transaction)
            .await?;
        let mut state = ListingAuthorizationState {
            automatic_link_ids: links.iter().map(|link| link.listing_link_id).collect(),
            current_automatic_link_ids: HashSet::new(),
        };
        if links.is_empty() {
            return Ok(state);
        }

        let authorizations_sql = $db.sql(ASSOCIATION_AUTHORIZATIONS_SQL);
        let authorizations = sqlx::query_as::<_, AssociationAuthorizationRow>(&authorizations_sql)
            .bind($listing_id)
            .fetch_all(&mut **$transaction)
            .await?;
        let authorizations = authorizations
            .into_iter()
            .map(|authorization| {
                (
                    (
                        authorization.listing_link_id,
                        authorization.association_role.clone(),
                    ),
                    authorization,
                )
            })
            .collect::<HashMap<_, _>>();

        let has_same_case_authorization = authorizations
            .values()
            .any(|authorization| authorization.authorization_kind == "same_case_grounded");
        let catalog_product_fingerprints = if has_same_case_authorization {
            let catalog_sql = $db.sql(APPROVED_CATALOG_ROWS_SQL);
            let catalog_rows = sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_sql)
                .fetch_all(&mut **$transaction)
                .await?;
            catalog_product_fingerprints(&catalog_products(catalog_rows))
        } else {
            HashMap::new()
        };
        let active_collision_rows = if has_same_case_authorization {
            let collision_sql = $db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
            sqlx::query_as::<_, ActiveCollisionCatalogFingerprintRow>(&collision_sql)
                .fetch_all(&mut **$transaction)
                .await?
        } else {
            Vec::new()
        };
        let source_revocation_count: i64 = if has_same_case_authorization {
            let revocation_sql =
                $db.sql("SELECT COUNT(*) FROM avionics_authoritative_source_origin_revocations");
            sqlx::query_scalar(&revocation_sql)
                .fetch_one(&mut **$transaction)
                .await?
        } else {
            0
        };

        for link in &links {
            let exact_shape = link.quantity > 0
                && link.source_confidence.as_deref() == Some("high")
                && link.installed_catalog_status == "approved"
                && match link.configuration_action.as_str() {
                    "installed" => link.replaces_avionics_model_id.is_none(),
                    "replaces" | "removes" => {
                        link.replaces_avionics_model_id.is_some()
                            && link.replacement_catalog_status.as_deref() == Some("approved")
                    }
                    _ => false,
                }
                && (link.source != "listing_explicit_count"
                    || (link.quantity == 2
                        && link.configuration_action == "installed"
                        && link.replaces_avionics_model_id.is_none()));
            if !exact_shape {
                continue;
            }

            let installed_key = (link.listing_link_id, "installed".to_string());
            let Some(installed_authorization) = authorizations.get(&installed_key) else {
                continue;
            };
            if !endpoint_authorization_is_current!(
                $db,
                $transaction,
                link,
                ListingAssociationRole::Installed,
                link.avionics_model_id,
                installed_authorization,
                &catalog_product_fingerprints,
                &active_collision_rows,
                source_revocation_count,
                $reuse_is_current,
                $countable_is_current,
                $active_collision_revision
            )? {
                continue;
            }

            if let Some(replacement_id) = link.replaces_avionics_model_id {
                let replacement_key = (link.listing_link_id, "replacement".to_string());
                let Some(replacement_authorization) = authorizations.get(&replacement_key) else {
                    continue;
                };
                if !endpoint_authorization_is_current!(
                    $db,
                    $transaction,
                    link,
                    ListingAssociationRole::Replacement,
                    replacement_id,
                    replacement_authorization,
                    &catalog_product_fingerprints,
                    &active_collision_rows,
                    source_revocation_count,
                    $reuse_is_current,
                    $countable_is_current,
                    $active_collision_revision
                )? {
                    continue;
                }
            }
            state
                .current_automatic_link_ids
                .insert(link.listing_link_id);
        }
        Ok(state)
    }};
}

pub(crate) async fn listing_authorization_state_sqlite(
    db: &AppDb,
    transaction: &mut Transaction<'_, Sqlite>,
    listing_id: i64,
) -> AuthorizationResult<ListingAuthorizationState> {
    listing_authorization_state_in_transaction!(
        db,
        transaction,
        listing_id,
        reuse_attestation_is_current_sqlite,
        countable_unit_reuse_attestation_is_current_sqlite,
        active_collision_closure_revision_sha256_sqlite
    )
}

pub(crate) async fn listing_authorization_state_postgres(
    db: &AppDb,
    transaction: &mut Transaction<'_, Postgres>,
    listing_id: i64,
) -> AuthorizationResult<ListingAuthorizationState> {
    listing_authorization_state_in_transaction!(
        db,
        transaction,
        listing_id,
        reuse_attestation_is_current_postgres,
        countable_unit_reuse_attestation_is_current_postgres,
        active_collision_closure_revision_sha256_postgres
    )
}

pub(crate) async fn listing_authorization_state(
    db: &AppDb,
    listing_id: i64,
) -> AuthorizationResult<ListingAuthorizationState> {
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            let state =
                listing_authorization_state_sqlite(db, &mut transaction, listing_id).await?;
            transaction.commit().await?;
            Ok(state)
        }
        DatabaseBackend::Postgres(pool) => {
            let mut connection = pool.acquire().await?;
            let mut transaction = connection
                .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
                .await?;
            let state =
                listing_authorization_state_postgres(db, &mut transaction, listing_id).await?;
            transaction.commit().await?;
            Ok(state)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        exact_checkpoint_is_current, sha256_hex, AssociationAuthorizationRow,
        ListingAuthorizationState,
    };

    const EVIDENCE: &str = "Garmin G5 installed";

    fn checkpoint() -> String {
        serde_json::json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2020,
            "asking_price_usd": 100000,
            "currency": "USD",
            "airframe_hours": 1000,
            "engine_hours": null,
            "engine_time_basis": "unknown",
            "engine_time_evidence": null,
            "engine_time_confidence": null,
            "propeller_hours": null,
            "propeller_time_basis": "unknown",
            "propeller_time_evidence": null,
            "propeller_time_confidence": null,
            "installed_engine": null,
            "installed_propeller": null,
            "registration_number": "N12345",
            "serial_number": "TEST123",
            "status": "active",
            "avionics": [{
                "manufacturer": "Garmin",
                "model": "G5",
                "types": ["Flight Display"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": EVIDENCE,
                "source_confidence": "high"
            }],
            "valuation_facts": []
        })
        .to_string()
    }

    fn authorization() -> AssociationAuthorizationRow {
        let rendered_html = format!("<html><body>{EVIDENCE}</body></html>");
        let extracted_listing_json = checkpoint();
        AssociationAuthorizationRow {
            listing_link_id: 10,
            association_role: "installed".to_string(),
            avionics_model_id: 20,
            authorization_kind: "manufacturer_reuse".to_string(),
            observation_sha256: "1".repeat(64),
            product_fingerprint: "2".repeat(64),
            grounded_resolution_sha256: None,
            evidence_capture_sha256: sha256_hex(rendered_html.as_bytes()),
            plugin_submission_id: 30,
            extracted_listing_sha256: sha256_hex(extracted_listing_json.as_bytes()),
            collision_closure_sha256: "3".repeat(64),
            source_revocation_count: None,
            rendered_html: Some(rendered_html),
            extracted_listing_json: Some(extracted_listing_json),
            install_created_at: Some("2026-08-01T00:00:00Z".to_string()),
            submitted_at: Some("2026-08-02T00:00:00Z".to_string()),
            install_revoked_at: None,
            exact_submission_is_current: true,
            current_reuse_product_fingerprint: Some("2".repeat(64)),
        }
    }

    #[test]
    fn exact_checkpoint_rehashes_capture_and_decoded_evidence() {
        let current = authorization();
        assert!(exact_checkpoint_is_current(&current, EVIDENCE));

        let mut stale_checkpoint = current.clone();
        stale_checkpoint.extracted_listing_sha256 = "0".repeat(64);
        assert!(!exact_checkpoint_is_current(&stale_checkpoint, EVIDENCE));
        assert!(!exact_checkpoint_is_current(
            &current,
            "different listing evidence"
        ));
    }

    #[test]
    fn exact_checkpoint_rejects_invalid_install_chronology() {
        let mut authorization = authorization();
        authorization.install_revoked_at = Some("2026-08-01T12:00:00Z".to_string());
        authorization.submitted_at = Some("2026-08-02T00:00:00Z".to_string());
        assert!(!exact_checkpoint_is_current(&authorization, EVIDENCE));
    }

    #[test]
    fn listing_state_fails_closed_when_any_automatic_link_is_stale() {
        let state = ListingAuthorizationState {
            automatic_link_ids: HashSet::from([1, 2]),
            current_automatic_link_ids: HashSet::from([1]),
        };
        assert!(!state.all_automatic_associations_current());
        assert!(state.automatic_link_is_current(1));
        assert!(!state.automatic_link_is_current(2));
        assert!(ListingAuthorizationState::default().all_automatic_associations_current());
    }
}
