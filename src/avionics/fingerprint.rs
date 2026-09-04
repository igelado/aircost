//! Deterministic avionics catalog and collision-closure fingerprints.
//!
//! This module owns the neutral catalog snapshot contract shared by curation,
//! consolidation, listing materialization, and review. Hash domains and row
//! projections are versioned here so no consumer can silently reinterpret a
//! proof produced by another workflow.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use sha2::{Digest, Sha256};
use sqlx::{FromRow, Postgres, Sqlite, Transaction};

use crate::avionics::reuse::{
    current_reuse_attested_product_ids, reuse_attestation_is_current_postgres,
    reuse_attestation_is_current_sqlite,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::normalize::normalize_avionics_identifier;

const APPROVED_CATALOG_FINGERPRINT_DOMAIN: &[u8] = b"aircost:approved-avionics-catalog:v2";
const APPROVED_CATALOG_PRODUCT_FINGERPRINT_DOMAIN: &[u8] =
    b"aircost:approved-avionics-catalog-product:v2";
const ACTIVE_COLLISION_CLOSURE_FINGERPRINT_DOMAIN: &[u8] =
    b"aircost:active-avionics-collision-closure:v1";
const GROUNDED_COLLISION_CLOSURE_FINGERPRINT_DOMAIN: &[u8] =
    b"aircost:grounded-avionics-collision-closure:v1";

#[derive(Debug)]
pub(crate) enum AvionicsFingerprintError {
    Conflict(String),
    Database(String),
}

impl fmt::Display for AvionicsFingerprintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict(message) | Self::Database(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AvionicsFingerprintError {}

impl From<sqlx::Error> for AvionicsFingerprintError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

pub(crate) type AvionicsFingerprintResult<T> = Result<T, AvionicsFingerprintError>;

#[derive(Clone, Debug, FromRow)]
pub(crate) struct CatalogFingerprintRow {
    pub(crate) id: i64,
    pub(crate) manufacturer: String,
    pub(crate) model: String,
    pub(crate) capability: String,
    pub(crate) manufacturer_identifier_kind: Option<String>,
    pub(crate) manufacturer_identifier: Option<String>,
    pub(crate) avionics_manufacturer_identity_id: i64,
    pub(crate) canonical_product_key: String,
    pub(crate) graph_manufacturer_identifier_kind: String,
    pub(crate) canonical_identifier_key: String,
    pub(crate) identity_source_url: Option<String>,
    pub(crate) identity_source_title: Option<String>,
    pub(crate) identity_evidence_text: Option<String>,
    pub(crate) valuation_scope: String,
    pub(crate) suite_component_model_id: Option<i64>,
    pub(crate) suite_component_quantity: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct CatalogFingerprintProduct {
    pub(crate) id: i64,
    pub(crate) manufacturer: String,
    pub(crate) model: String,
    pub(crate) capabilities: Vec<String>,
    pub(crate) manufacturer_identifier_kind: String,
    pub(crate) manufacturer_identifier: String,
    pub(crate) avionics_manufacturer_identity_id: i64,
    pub(crate) canonical_product_key: String,
    pub(crate) graph_manufacturer_identifier_kind: String,
    pub(crate) canonical_identifier_key: String,
    pub(crate) identity_source_url: String,
    pub(crate) identity_source_title: String,
    pub(crate) identity_evidence_text: String,
    pub(crate) valuation_scope: String,
    pub(crate) suite_components: Vec<(i64, i64)>,
}

/// Every active catalog identity component that can block source-free reuse.
///
/// This projection is intentionally independent of product selectability: a
/// generic manufacturer, missing manufacturer identity, or missing capability
/// membership still participates in collision detection.
#[derive(Clone, Debug, FromRow)]
pub(crate) struct ActiveCollisionCatalogFingerprintRow {
    pub(crate) id: i64,
    pub(crate) catalog_status: String,
    pub(crate) effective_manufacturer_identity_id: Option<i64>,
    pub(crate) model: String,
    pub(crate) manufacturer_identifier_kind: Option<String>,
    pub(crate) manufacturer_identifier: Option<String>,
}

pub(crate) const APPROVED_CATALOG_ROWS_SQL: &str = r#"
    SELECT
      model.id,
      manufacturer.name AS manufacturer,
      model.name AS model,
      capability.name AS capability,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier,
      graph.avionics_manufacturer_identity_id,
      graph.canonical_product_key,
      graph.manufacturer_identifier_kind AS graph_manufacturer_identifier_kind,
      graph.canonical_identifier_key,
      model.identity_source_url,
      model.identity_source_title,
      model.identity_evidence_text,
      model.valuation_scope,
      suite_component.component_model_id AS suite_component_model_id,
      suite_component.quantity AS suite_component_quantity
    FROM avionics_models model
    JOIN avionics_manufacturers manufacturer
      ON manufacturer.id = model.avionics_manufacturer_id
    JOIN avionics_model_types membership
      ON membership.avionics_model_id = model.id
    JOIN avionics_types capability
      ON capability.id = membership.avionics_type_id
    JOIN avionics_approved_product_graph_identities graph
      ON graph.avionics_model_id = model.id
    LEFT JOIN avionics_suite_components suite_component
      ON suite_component.suite_model_id = model.id
    WHERE model.catalog_status = 'approved'
    ORDER BY model.id, capability.normalized_name, capability.id,
             suite_component.component_model_id
"#;

pub(crate) const ACTIVE_COLLISION_CATALOG_ROWS_SQL: &str = r#"
    SELECT
      model.id,
      model.catalog_status,
      effective_manufacturer.avionics_manufacturer_identity_id
        AS effective_manufacturer_identity_id,
      model.name AS model,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier
    FROM avionics_models model
    LEFT JOIN avionics_manufacturer_effective_memberships effective_manufacturer
      ON effective_manufacturer.avionics_manufacturer_id =
         model.avionics_manufacturer_id
    LEFT JOIN avionics_model_types capability_membership
      ON capability_membership.avionics_model_id = model.id
    WHERE model.catalog_status IN ('approved', 'unreviewed')
    GROUP BY
      model.id,
      model.catalog_status,
      effective_manufacturer.avionics_manufacturer_identity_id,
      model.name,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier
    ORDER BY model.id,
             effective_manufacturer.avionics_manufacturer_identity_id
"#;

fn feed_fingerprint(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

pub(crate) fn catalog_products(rows: Vec<CatalogFingerprintRow>) -> Vec<CatalogFingerprintProduct> {
    let mut products = BTreeMap::<i64, CatalogFingerprintProduct>::new();
    for row in rows {
        let product = products
            .entry(row.id)
            .or_insert_with(|| CatalogFingerprintProduct {
                id: row.id,
                manufacturer: row.manufacturer,
                model: row.model,
                capabilities: Vec::new(),
                manufacturer_identifier_kind: row.manufacturer_identifier_kind.unwrap_or_default(),
                manufacturer_identifier: row.manufacturer_identifier.unwrap_or_default(),
                avionics_manufacturer_identity_id: row.avionics_manufacturer_identity_id,
                canonical_product_key: row.canonical_product_key,
                graph_manufacturer_identifier_kind: row.graph_manufacturer_identifier_kind,
                canonical_identifier_key: row.canonical_identifier_key,
                identity_source_url: row.identity_source_url.unwrap_or_default(),
                identity_source_title: row.identity_source_title.unwrap_or_default(),
                identity_evidence_text: row.identity_evidence_text.unwrap_or_default(),
                valuation_scope: row.valuation_scope.clone(),
                suite_components: Vec::new(),
            });
        if !product.capabilities.contains(&row.capability) {
            product.capabilities.push(row.capability);
        }
        if let (Some(component_model_id), Some(quantity)) =
            (row.suite_component_model_id, row.suite_component_quantity)
        {
            let component = (component_model_id, quantity);
            if !product.suite_components.contains(&component) {
                product.suite_components.push(component);
            }
        }
    }
    for product in products.values_mut() {
        product.capabilities.sort();
        product.suite_components.sort_unstable();
    }
    products.into_values().collect()
}

fn suite_component_fingerprint_value(components: &[(i64, i64)]) -> String {
    components
        .iter()
        .map(|(component_model_id, quantity)| format!("{component_model_id}:{quantity}"))
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

pub(crate) fn fingerprint_catalog_products(products: &[CatalogFingerprintProduct]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(APPROVED_CATALOG_FINGERPRINT_DOMAIN);
    for product in products {
        for value in [
            product.id.to_string(),
            product.manufacturer.clone(),
            product.model.clone(),
            product.capabilities.join("\u{1f}"),
            product.manufacturer_identifier_kind.clone(),
            product.manufacturer_identifier.clone(),
            product.avionics_manufacturer_identity_id.to_string(),
            product.canonical_product_key.clone(),
            product.graph_manufacturer_identifier_kind.clone(),
            product.canonical_identifier_key.clone(),
            product.identity_source_url.clone(),
            product.identity_source_title.clone(),
            product.identity_evidence_text.clone(),
            product.valuation_scope.clone(),
            suite_component_fingerprint_value(&product.suite_components),
        ] {
            feed_fingerprint(&mut hasher, &value);
        }
    }
    format!("{:x}", hasher.finalize())
}

pub(crate) fn fingerprint_catalog_product(product: &CatalogFingerprintProduct) -> String {
    let mut hasher = Sha256::new();
    hasher.update(APPROVED_CATALOG_PRODUCT_FINGERPRINT_DOMAIN);
    for value in [
        product.id.to_string(),
        product.manufacturer.clone(),
        product.model.clone(),
        product.capabilities.join("\u{1f}"),
        product.manufacturer_identifier_kind.clone(),
        product.manufacturer_identifier.clone(),
        product.avionics_manufacturer_identity_id.to_string(),
        product.canonical_product_key.clone(),
        product.graph_manufacturer_identifier_kind.clone(),
        product.canonical_identifier_key.clone(),
        product.identity_source_url.clone(),
        product.identity_source_title.clone(),
        product.identity_evidence_text.clone(),
        product.valuation_scope.clone(),
        suite_component_fingerprint_value(&product.suite_components),
    ] {
        feed_fingerprint(&mut hasher, &value);
    }
    format!("{:x}", hasher.finalize())
}

pub(crate) fn catalog_product_fingerprints(
    products: &[CatalogFingerprintProduct],
) -> HashMap<i64, String> {
    products
        .iter()
        .map(|product| (product.id, fingerprint_catalog_product(product)))
        .collect()
}

pub(crate) fn catalog_product_fingerprint_from_rows(
    rows: &[CatalogFingerprintRow],
    avionics_model_id: i64,
) -> Option<String> {
    catalog_product_fingerprints(&catalog_products(rows.to_vec())).remove(&avionics_model_id)
}

pub(crate) fn fingerprint_approved_catalog_rows(rows: Vec<CatalogFingerprintRow>) -> String {
    fingerprint_catalog_products(&catalog_products(rows))
}

fn active_collision_closure_rows(
    rows: &[ActiveCollisionCatalogFingerprintRow],
    target_id: i64,
) -> Option<(
    &ActiveCollisionCatalogFingerprintRow,
    Vec<&ActiveCollisionCatalogFingerprintRow>,
)> {
    let target_rows = rows
        .iter()
        .filter(|row| row.id == target_id)
        .collect::<Vec<_>>();
    let [target] = target_rows.as_slice() else {
        return None;
    };
    let target_model_key = normalize_avionics_identifier(&target.model);
    let target_identifier_key = normalize_avionics_identifier(
        target
            .manufacturer_identifier
            .as_deref()
            .unwrap_or_default(),
    );
    if target_model_key.is_empty() || target_identifier_key.is_empty() {
        return None;
    }
    let members = rows
        .iter()
        .filter(|row| {
            let model_key = normalize_avionics_identifier(&row.model);
            let identifier_key = normalize_avionics_identifier(
                row.manufacturer_identifier.as_deref().unwrap_or_default(),
            );
            let exact_identity_collision = [model_key.as_str(), identifier_key.as_str()]
                .into_iter()
                .filter(|key| !key.is_empty())
                .any(|key| key == target_model_key || key == target_identifier_key);
            exact_identity_collision
                || (!model_key.is_empty()
                    && (model_key.starts_with(&target_model_key)
                        || target_model_key.starts_with(&model_key)))
        })
        .collect();
    Some((target, members))
}

pub(crate) fn active_collision_closure_member_ids(
    rows: &[ActiveCollisionCatalogFingerprintRow],
    target_id: i64,
) -> Option<Vec<i64>> {
    let (_, members) = active_collision_closure_rows(rows, target_id)?;
    Some(members.into_iter().map(|row| row.id).collect())
}

pub(crate) fn fingerprint_active_collision_closure(
    rows: &[ActiveCollisionCatalogFingerprintRow],
    current_reuse_eligible_ids: &HashSet<i64>,
    target_id: i64,
) -> Option<String> {
    let (_, members) = active_collision_closure_rows(rows, target_id)?;
    let mut keys = members
        .into_iter()
        .map(|row| {
            [
                row.id.to_string(),
                row.catalog_status.clone(),
                row.effective_manufacturer_identity_id
                    .map(|identity_id| identity_id.to_string())
                    .unwrap_or_default(),
                normalize_avionics_identifier(&row.model),
                row.manufacturer_identifier_kind.clone().unwrap_or_default(),
                normalize_avionics_identifier(
                    row.manufacturer_identifier.as_deref().unwrap_or_default(),
                ),
                current_reuse_eligible_ids.contains(&row.id).to_string(),
            ]
        })
        .collect::<Vec<_>>();
    keys.sort();

    let mut hasher = Sha256::new();
    hasher.update(ACTIVE_COLLISION_CLOSURE_FINGERPRINT_DOMAIN);
    feed_fingerprint(&mut hasher, &target_id.to_string());
    for key in keys {
        for value in key {
            feed_fingerprint(&mut hasher, &value);
        }
    }
    Some(format!("{:x}", hasher.finalize()))
}

pub(crate) fn fingerprint_grounded_collision_closure(
    rows: &[ActiveCollisionCatalogFingerprintRow],
    target_id: i64,
) -> Option<String> {
    let (_, members) = active_collision_closure_rows(rows, target_id)?;
    let mut keys = members
        .into_iter()
        .map(|row| {
            [
                row.id.to_string(),
                row.catalog_status.clone(),
                row.effective_manufacturer_identity_id
                    .map(|identity_id| identity_id.to_string())
                    .unwrap_or_default(),
                normalize_avionics_identifier(&row.model),
                row.manufacturer_identifier_kind.clone().unwrap_or_default(),
                normalize_avionics_identifier(
                    row.manufacturer_identifier.as_deref().unwrap_or_default(),
                ),
            ]
        })
        .collect::<Vec<_>>();
    keys.sort();

    let mut hasher = Sha256::new();
    hasher.update(GROUNDED_COLLISION_CLOSURE_FINGERPRINT_DOMAIN);
    feed_fingerprint(&mut hasher, &target_id.to_string());
    for key in keys {
        for value in key {
            feed_fingerprint(&mut hasher, &value);
        }
    }
    Some(format!("{:x}", hasher.finalize()))
}

pub(crate) async fn load_catalog_product_fingerprint_map(
    db: &AppDb,
) -> AvionicsFingerprintResult<HashMap<i64, String>> {
    let sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, CatalogFingerprintRow>(&sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, CatalogFingerprintRow>(&sql)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(catalog_product_fingerprints(&catalog_products(rows)))
}

pub(crate) async fn catalog_product_fingerprint_for_id(
    db: &AppDb,
    avionics_model_id: i64,
) -> AvionicsFingerprintResult<String> {
    load_catalog_product_fingerprint_map(db)
        .await?
        .remove(&avionics_model_id)
        .ok_or_else(|| {
            AvionicsFingerprintError::Conflict(format!(
                "catalog id {avionics_model_id} has no current approved product fingerprint"
            ))
        })
}

pub(crate) async fn load_active_collision_catalog_rows(
    db: &AppDb,
) -> AvionicsFingerprintResult<Vec<ActiveCollisionCatalogFingerprintRow>> {
    let sql = db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as::<
            _,
            ActiveCollisionCatalogFingerprintRow,
        >(&sql)
        .fetch_all(pool)
        .await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_as::<
            _,
            ActiveCollisionCatalogFingerprintRow,
        >(&sql)
        .fetch_all(pool)
        .await?),
    }
}

/// Optimistic token for every catalog fact that can change the zero-Gemini
/// listing-association resolver's identity decision.
pub(crate) async fn active_collision_closure_revision_sha256(
    db: &AppDb,
    target_id: i64,
) -> AvionicsFingerprintResult<String> {
    let rows = load_active_collision_catalog_rows(db).await?;
    let current_reuse_eligible_ids = current_reuse_attested_product_ids(db)
        .await
        .map_err(|error| AvionicsFingerprintError::Database(error.to_string()))?;
    fingerprint_active_collision_closure(&rows, &current_reuse_eligible_ids, target_id).ok_or_else(
        || {
            AvionicsFingerprintError::Conflict(format!(
                "catalog id {target_id} has no unique active collision-closure identity"
            ))
        },
    )
}

macro_rules! active_collision_closure_revision_in_transaction {
    ($db:expr, $transaction:expr, $target_id:expr, $reuse_is_current:path) => {{
        let sql = $db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
        let rows = sqlx::query_as::<_, ActiveCollisionCatalogFingerprintRow>(&sql)
            .fetch_all(&mut **$transaction)
            .await?;
        let member_ids =
            active_collision_closure_member_ids(&rows, $target_id).ok_or_else(|| {
                AvionicsFingerprintError::Conflict(format!(
                    "catalog id {} has no unique active collision-closure identity",
                    $target_id
                ))
            })?;
        let mut current_reuse_eligible_ids = HashSet::new();
        for member_id in member_ids {
            if $reuse_is_current($db, $transaction, member_id).await? {
                current_reuse_eligible_ids.insert(member_id);
            }
        }
        fingerprint_active_collision_closure(&rows, &current_reuse_eligible_ids, $target_id)
            .ok_or_else(|| {
                AvionicsFingerprintError::Conflict(format!(
                    "catalog id {} has no unique active collision-closure identity",
                    $target_id
                ))
            })
    }};
}

pub(crate) async fn active_collision_closure_revision_sha256_sqlite(
    db: &AppDb,
    transaction: &mut Transaction<'_, Sqlite>,
    target_id: i64,
) -> AvionicsFingerprintResult<String> {
    active_collision_closure_revision_in_transaction!(
        db,
        transaction,
        target_id,
        reuse_attestation_is_current_sqlite
    )
}

pub(crate) async fn active_collision_closure_revision_sha256_postgres(
    db: &AppDb,
    transaction: &mut Transaction<'_, Postgres>,
    target_id: i64,
) -> AvionicsFingerprintResult<String> {
    active_collision_closure_revision_in_transaction!(
        db,
        transaction,
        target_id,
        reuse_attestation_is_current_postgres
    )
}

pub(crate) async fn grounded_collision_closure_revision_sha256(
    db: &AppDb,
    target_id: i64,
) -> AvionicsFingerprintResult<String> {
    let rows = load_active_collision_catalog_rows(db).await?;
    fingerprint_grounded_collision_closure(&rows, target_id).ok_or_else(|| {
        AvionicsFingerprintError::Conflict(format!(
            "catalog id {target_id} has no unique grounded collision-closure identity"
        ))
    })
}

/// Current approved-only catalog revision. Unreviewed and rejected legacy rows
/// intentionally cannot invalidate a consumer's optimistic snapshot.
pub(crate) async fn approved_catalog_revision_sha256(
    db: &AppDb,
) -> AvionicsFingerprintResult<String> {
    let sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, CatalogFingerprintRow>(&sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, CatalogFingerprintRow>(&sql)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(fingerprint_approved_catalog_rows(rows))
}
