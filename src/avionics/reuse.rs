//! Positive-only eligibility attestations for reusing approved avionics.
//!
//! Catalog approval remains historical product truth. Reuse eligibility is a
//! narrower, current-policy cache: the stored fingerprint must still match the
//! complete approved product identity, capability set, and active exact source
//! origin. Missing, revoked, or stale attestations fail closed without hiding
//! the product from collision review or existing historical associations.

use std::collections::HashSet;

use sha2::{Digest, Sha256};
use sqlx::{FromRow, Postgres, Sqlite, Transaction};

use super::manufacturer::canonical_exact_https_origin;
use crate::db::{AppDb, DatabaseBackend};

pub(crate) const AVIONICS_REUSE_POLICY_VERSION: &str = "avionics_reuse_v2";
const AVIONICS_REUSE_FINGERPRINT_DOMAIN: &[u8] =
    b"aircost:avionics-product-reuse-attestation:v2:target-aware-oem-proof\0";

#[derive(Clone, Debug, FromRow)]
struct ReuseAttestationRow {
    avionics_model_id: i64,
    avionics_authoritative_source_origin_id: i64,
    policy_version: String,
    product_fingerprint: String,
}

#[derive(Clone, Debug, FromRow, PartialEq, Eq)]
struct ReuseFingerprintRow {
    avionics_model_id: i64,
    avionics_manufacturer_id: i64,
    manufacturer_name: String,
    manufacturer_normalized_name: String,
    avionics_manufacturer_identity_id: i64,
    manufacturer_identity_name: String,
    manufacturer_identity_key: String,
    model_name: String,
    model_normalized_name: String,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    normalized_manufacturer_identifier: Option<String>,
    identity_source_url: Option<String>,
    identity_source_title: Option<String>,
    identity_evidence_text: Option<String>,
    identity_evidence_kind: Option<String>,
    identity_confidence: Option<String>,
    canonical_product_key: String,
    canonical_identifier_key: String,
    source_origin_id: i64,
    source_origin: String,
    capability_name: String,
    capability_normalized_name: String,
}

const REUSE_FINGERPRINT_ROWS_SQL: &str = r#"
    SELECT
      model.id AS avionics_model_id,
      manufacturer.id AS avionics_manufacturer_id,
      manufacturer.name AS manufacturer_name,
      manufacturer.normalized_name AS manufacturer_normalized_name,
      product_identity.avionics_manufacturer_identity_id,
      manufacturer_identity.canonical_name AS manufacturer_identity_name,
      manufacturer_identity.normalized_identity_key AS manufacturer_identity_key,
      model.name AS model_name,
      model.normalized_name AS model_normalized_name,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier,
      model.normalized_manufacturer_identifier,
      model.identity_source_url,
      model.identity_source_title,
      model.identity_evidence_text,
      model.identity_evidence_kind,
      model.identity_confidence,
      product_identity.canonical_product_key,
      product_identity.canonical_identifier_key,
      source_origin.id AS source_origin_id,
      source_origin.https_origin AS source_origin,
      capability.name AS capability_name,
      capability.normalized_name AS capability_normalized_name
    FROM avionics_models model
    JOIN avionics_manufacturers manufacturer
      ON manufacturer.id = model.avionics_manufacturer_id
    JOIN avionics_approved_product_identities product_identity
      ON product_identity.avionics_model_id = model.id
    JOIN avionics_manufacturer_identities manufacturer_identity
      ON manufacturer_identity.id =
         product_identity.avionics_manufacturer_identity_id
    JOIN avionics_model_types membership
      ON membership.avionics_model_id = model.id
    JOIN avionics_types capability
      ON capability.id = membership.avionics_type_id
    JOIN avionics_active_authoritative_source_origins source_origin
      ON source_origin.id = ?
     AND source_origin.authority_kind = 'manufacturer_primary'
    JOIN avionics_manufacturer_effective_identities origin_identity
      ON origin_identity.identity_id =
         source_origin.avionics_manufacturer_identity_id
     AND origin_identity.avionics_manufacturer_identity_id =
         product_identity.avionics_manufacturer_identity_id
    WHERE model.id = ?
      AND model.catalog_status = 'approved'
    ORDER BY capability.normalized_name, capability.id
"#;

fn feed_fingerprint(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn required<'a>(value: &'a Option<String>) -> Option<&'a str> {
    value.as_deref().filter(|value| !value.trim().is_empty())
}

fn fingerprint_rows(rows: &[ReuseFingerprintRow]) -> Option<String> {
    let first = rows.first()?;
    let scalar_matches = rows.iter().all(|row| {
        row.avionics_model_id == first.avionics_model_id
            && row.avionics_manufacturer_id == first.avionics_manufacturer_id
            && row.manufacturer_name == first.manufacturer_name
            && row.manufacturer_normalized_name == first.manufacturer_normalized_name
            && row.avionics_manufacturer_identity_id == first.avionics_manufacturer_identity_id
            && row.manufacturer_identity_name == first.manufacturer_identity_name
            && row.manufacturer_identity_key == first.manufacturer_identity_key
            && row.model_name == first.model_name
            && row.model_normalized_name == first.model_normalized_name
            && row.manufacturer_identifier_kind == first.manufacturer_identifier_kind
            && row.manufacturer_identifier == first.manufacturer_identifier
            && row.normalized_manufacturer_identifier == first.normalized_manufacturer_identifier
            && row.identity_source_url == first.identity_source_url
            && row.identity_source_title == first.identity_source_title
            && row.identity_evidence_text == first.identity_evidence_text
            && row.identity_evidence_kind == first.identity_evidence_kind
            && row.identity_confidence == first.identity_confidence
            && row.canonical_product_key == first.canonical_product_key
            && row.canonical_identifier_key == first.canonical_identifier_key
            && row.source_origin_id == first.source_origin_id
            && row.source_origin == first.source_origin
    });
    if !scalar_matches {
        return None;
    }

    let manufacturer_identifier_kind = required(&first.manufacturer_identifier_kind)?;
    let manufacturer_identifier = required(&first.manufacturer_identifier)?;
    let normalized_manufacturer_identifier = required(&first.normalized_manufacturer_identifier)?;
    let identity_source_url = required(&first.identity_source_url)?;
    let identity_source_title = required(&first.identity_source_title)?;
    let identity_evidence_text = required(&first.identity_evidence_text)?;
    let identity_evidence_kind = required(&first.identity_evidence_kind)?;
    let identity_confidence = required(&first.identity_confidence)?;
    let mut capabilities = rows
        .iter()
        .map(|row| {
            (
                row.capability_normalized_name.as_str(),
                row.capability_name.as_str(),
            )
        })
        .collect::<Vec<_>>();
    capabilities.sort_unstable();
    capabilities.dedup();
    if capabilities.is_empty() || capabilities.len() != rows.len() {
        return None;
    }

    let mut hasher = Sha256::new();
    hasher.update(AVIONICS_REUSE_FINGERPRINT_DOMAIN);
    for value in [
        AVIONICS_REUSE_POLICY_VERSION.to_string(),
        first.avionics_model_id.to_string(),
        first.avionics_manufacturer_id.to_string(),
        first.manufacturer_name.clone(),
        first.manufacturer_normalized_name.clone(),
        first.avionics_manufacturer_identity_id.to_string(),
        first.manufacturer_identity_name.clone(),
        first.manufacturer_identity_key.clone(),
        first.model_name.clone(),
        first.model_normalized_name.clone(),
        "approved".to_string(),
        manufacturer_identifier_kind.to_string(),
        manufacturer_identifier.to_string(),
        normalized_manufacturer_identifier.to_string(),
        identity_source_url.to_string(),
        identity_source_title.to_string(),
        identity_evidence_text.to_string(),
        identity_evidence_kind.to_string(),
        identity_confidence.to_string(),
        first.canonical_product_key.clone(),
        first.canonical_identifier_key.clone(),
        first.source_origin_id.to_string(),
        first.source_origin.clone(),
    ] {
        feed_fingerprint(&mut hasher, &value);
    }
    feed_fingerprint(&mut hasher, &capabilities.len().to_string());
    for (normalized_name, name) in capabilities {
        feed_fingerprint(&mut hasher, normalized_name);
        feed_fingerprint(&mut hasher, name);
    }
    Some(format!("{:x}", hasher.finalize()))
}

async fn load_attestation_rows(db: &AppDb) -> Result<Vec<ReuseAttestationRow>, sqlx::Error> {
    let sql = db.sql(
        r#"
        SELECT
          attestation.avionics_model_id,
          attestation.avionics_authoritative_source_origin_id,
          attestation.policy_version,
          attestation.product_fingerprint
        FROM avionics_product_reuse_attestations attestation
        JOIN avionics_active_authoritative_source_origins source_origin
          ON source_origin.id =
             attestation.avionics_authoritative_source_origin_id
        ORDER BY attestation.avionics_model_id
        "#,
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ReuseAttestationRow>(&sql)
                .fetch_all(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ReuseAttestationRow>(&sql)
                .fetch_all(pool)
                .await
        }
    }
}

async fn load_fingerprint_rows(
    db: &AppDb,
    avionics_model_id: i64,
    source_origin_id: i64,
) -> Result<Vec<ReuseFingerprintRow>, sqlx::Error> {
    let sql = db.sql(REUSE_FINGERPRINT_ROWS_SQL);
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ReuseFingerprintRow>(&sql)
                .bind(source_origin_id)
                .bind(avionics_model_id)
                .fetch_all(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ReuseFingerprintRow>(&sql)
                .bind(source_origin_id)
                .bind(avionics_model_id)
                .fetch_all(pool)
                .await
        }
    }
}

/// IDs that are eligible for no-grounding reuse under the current policy.
///
/// Any malformed or stale row is ignored. The catalog row remains available
/// to the normal grounded collision pipeline.
pub(crate) async fn current_reuse_attested_product_ids(
    db: &AppDb,
) -> Result<HashSet<i64>, sqlx::Error> {
    let attestations = load_attestation_rows(db).await?;
    let mut eligible = HashSet::new();
    for attestation in attestations {
        if attestation.policy_version != AVIONICS_REUSE_POLICY_VERSION {
            continue;
        }
        let rows = load_fingerprint_rows(
            db,
            attestation.avionics_model_id,
            attestation.avionics_authoritative_source_origin_id,
        )
        .await?;
        if fingerprint_rows(&rows).as_deref() == Some(attestation.product_fingerprint.as_str()) {
            eligible.insert(attestation.avionics_model_id);
        }
    }
    Ok(eligible)
}

/// Whether this exact HTTPS origin is currently curated as a primary
/// manufacturer source for the approved product's effective manufacturer
/// identity. This is a read-only cost gate used before paid grounding.
pub(crate) async fn reuse_source_origin_is_authorized(
    db: &AppDb,
    avionics_model_id: i64,
    source_url: &str,
) -> Result<bool, sqlx::Error> {
    let Ok(source_origin) = canonical_exact_https_origin(source_url) else {
        return Ok(false);
    };
    let sql = db.sql(
        r#"
        SELECT source_origin.id
        FROM avionics_models model
        JOIN avionics_approved_product_identities product_identity
          ON product_identity.avionics_model_id = model.id
        JOIN avionics_active_authoritative_source_origins source_origin
          ON source_origin.authority_kind = 'manufacturer_primary'
         AND source_origin.https_origin = ?
        JOIN avionics_manufacturer_effective_identities origin_identity
          ON origin_identity.identity_id =
             source_origin.avionics_manufacturer_identity_id
         AND origin_identity.avionics_manufacturer_identity_id =
             product_identity.avionics_manufacturer_identity_id
        WHERE model.id = ?
          AND model.catalog_status = 'approved'
        ORDER BY source_origin.id
        LIMIT 1
        "#,
    );
    let origin_id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(source_origin.as_str())
                .bind(avionics_model_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(source_origin.as_str())
                .bind(avionics_model_id)
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(origin_id.is_some())
}

macro_rules! refresh_reuse_attestation {
    (
        $db:expr,
        $transaction:expr,
        $avionics_model_id:expr,
        $attestation_source_url:expr
    ) => {{
        let avionics_model_id = $avionics_model_id;
        let Ok(source_origin) = canonical_exact_https_origin($attestation_source_url) else {
            return Ok(false);
        };

        let source_origin_sql = $db.sql(
            r#"
            SELECT source_origin.id
            FROM avionics_models model
            JOIN avionics_approved_product_identities product_identity
              ON product_identity.avionics_model_id = model.id
            JOIN avionics_active_authoritative_source_origins source_origin
              ON source_origin.authority_kind = 'manufacturer_primary'
             AND source_origin.https_origin = ?
            JOIN avionics_manufacturer_effective_identities origin_identity
              ON origin_identity.identity_id =
                 source_origin.avionics_manufacturer_identity_id
             AND origin_identity.avionics_manufacturer_identity_id =
                 product_identity.avionics_manufacturer_identity_id
            WHERE model.id = ?
              AND model.catalog_status = 'approved'
            ORDER BY source_origin.id
            LIMIT 1
            "#,
        );
        let source_origin_id: Option<i64> = sqlx::query_scalar(&source_origin_sql)
            .bind(source_origin.as_str())
            .bind(avionics_model_id)
            .fetch_optional(&mut **$transaction)
            .await?;
        let Some(source_origin_id) = source_origin_id else {
            return Ok(false);
        };

        let fingerprint_sql = $db.sql(REUSE_FINGERPRINT_ROWS_SQL);
        let rows = sqlx::query_as::<_, ReuseFingerprintRow>(&fingerprint_sql)
            .bind(source_origin_id)
            .bind(avionics_model_id)
            .fetch_all(&mut **$transaction)
            .await?;
        let Some(product_fingerprint) = fingerprint_rows(&rows) else {
            return Ok(false);
        };
        let delete_sql =
            $db.sql("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?");
        sqlx::query(&delete_sql)
            .bind(avionics_model_id)
            .execute(&mut **$transaction)
            .await?;
        let insert_sql = $db.sql(
            r#"
            INSERT INTO avionics_product_reuse_attestations (
              avionics_model_id,
              avionics_authoritative_source_origin_id,
              policy_version,
              product_fingerprint
            ) VALUES (?, ?, ?, ?)
            "#,
        );
        sqlx::query(&insert_sql)
            .bind(avionics_model_id)
            .bind(source_origin_id)
            .bind(AVIONICS_REUSE_POLICY_VERSION)
            .bind(product_fingerprint)
            .execute(&mut **$transaction)
            .await?;
        true
    }};
}

/// Refresh the positive cache after a current-policy SQLite admission.
///
/// A product whose exact origin is not independently curated remains approved
/// but deliberately receives no reuse attestation.
pub(crate) async fn refresh_reuse_attestation_sqlite(
    db: &AppDb,
    transaction: &mut Transaction<'_, Sqlite>,
    avionics_model_id: i64,
    attestation_source_url: &str,
) -> Result<bool, sqlx::Error> {
    Ok(refresh_reuse_attestation!(
        db,
        transaction,
        avionics_model_id,
        attestation_source_url
    ))
}

/// PostgreSQL counterpart to [`refresh_reuse_attestation_sqlite`].
pub(crate) async fn refresh_reuse_attestation_postgres(
    db: &AppDb,
    transaction: &mut Transaction<'_, Postgres>,
    avionics_model_id: i64,
    attestation_source_url: &str,
) -> Result<bool, sqlx::Error> {
    Ok(refresh_reuse_attestation!(
        db,
        transaction,
        avionics_model_id,
        attestation_source_url
    ))
}

macro_rules! refresh_grounded_evidence_and_reuse_attestation {
    (
        $db:expr,
        $transaction:expr,
        $avionics_model_id:expr,
        $identity_source_url:expr,
        $identity_source_title:expr,
        $identity_evidence_text:expr
    ) => {{
        let avionics_model_id = $avionics_model_id;
        let refresh_evidence_sql = $db.sql(
            r#"
            UPDATE avionics_models
            SET identity_source_url = ?,
                identity_source_title = ?,
                identity_evidence_text = ?,
                identity_evidence_kind = 'authoritative_reference',
                identity_confidence = 'very_high',
                catalog_reviewed_at = CURRENT_TIMESTAMP,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
              AND catalog_status = 'approved'
            "#,
        );
        let refreshed = sqlx::query(&refresh_evidence_sql)
            .bind($identity_source_url)
            .bind($identity_source_title)
            .bind($identity_evidence_text)
            .bind(avionics_model_id)
            .execute(&mut **$transaction)
            .await?
            .rows_affected();
        if refreshed != 1 {
            false
        } else {
            // Compute the positive cache only after the current grounded
            // evidence is visible inside this transaction. If the exact
            // manufacturer origin is unavailable or revoked, the caller must
            // drop the transaction so the tentative evidence update rolls
            // back with the failed attestation.
            refresh_reuse_attestation!($db, $transaction, avionics_model_id, $identity_source_url)
        }
    }};
}

/// Atomically replace an approved product's non-identity evidence with a fresh
/// grounded manufacturer source and fingerprint that exact refreshed row.
///
/// Callers must compare the immutable identity and reviewed catalog snapshot
/// while holding their catalog lock before invoking this helper. A `false`
/// result is deliberately not committable: the exact source origin is not
/// active/authorized (or the refreshed row cannot produce a complete reuse
/// fingerprint), so callers must roll the transaction back.
pub(crate) async fn refresh_grounded_evidence_and_reuse_attestation_sqlite(
    db: &AppDb,
    transaction: &mut Transaction<'_, Sqlite>,
    avionics_model_id: i64,
    identity_source_url: &str,
    identity_source_title: &str,
    identity_evidence_text: &str,
) -> Result<bool, sqlx::Error> {
    Ok(refresh_grounded_evidence_and_reuse_attestation!(
        db,
        transaction,
        avionics_model_id,
        identity_source_url,
        identity_source_title,
        identity_evidence_text
    ))
}

/// PostgreSQL counterpart to
/// [`refresh_grounded_evidence_and_reuse_attestation_sqlite`].
pub(crate) async fn refresh_grounded_evidence_and_reuse_attestation_postgres(
    db: &AppDb,
    transaction: &mut Transaction<'_, Postgres>,
    avionics_model_id: i64,
    identity_source_url: &str,
    identity_source_title: &str,
    identity_evidence_text: &str,
) -> Result<bool, sqlx::Error> {
    Ok(refresh_grounded_evidence_and_reuse_attestation!(
        db,
        transaction,
        avionics_model_id,
        identity_source_url,
        identity_source_title,
        identity_evidence_text
    ))
}

macro_rules! reuse_attestation_is_current {
    ($db:expr, $transaction:expr, $avionics_model_id:expr) => {{
        let attestation_sql = $db.sql(
            r#"
            SELECT
              attestation.avionics_model_id,
              attestation.avionics_authoritative_source_origin_id,
              attestation.policy_version,
              attestation.product_fingerprint
            FROM avionics_product_reuse_attestations attestation
            JOIN avionics_active_authoritative_source_origins source_origin
              ON source_origin.id =
                 attestation.avionics_authoritative_source_origin_id
            WHERE attestation.avionics_model_id = ?
            "#,
        );
        let attestation: Option<ReuseAttestationRow> = sqlx::query_as(&attestation_sql)
            .bind($avionics_model_id)
            .fetch_optional(&mut **$transaction)
            .await?;
        let Some(attestation) = attestation else {
            return Ok(false);
        };
        if attestation.policy_version != AVIONICS_REUSE_POLICY_VERSION {
            false
        } else {
            let fingerprint_sql = $db.sql(REUSE_FINGERPRINT_ROWS_SQL);
            let rows = sqlx::query_as::<_, ReuseFingerprintRow>(&fingerprint_sql)
                .bind(attestation.avionics_authoritative_source_origin_id)
                .bind(attestation.avionics_model_id)
                .fetch_all(&mut **$transaction)
                .await?;
            fingerprint_rows(&rows).as_deref() == Some(attestation.product_fingerprint.as_str())
        }
    }};
}

pub(crate) async fn reuse_attestation_is_current_sqlite(
    db: &AppDb,
    transaction: &mut Transaction<'_, Sqlite>,
    avionics_model_id: i64,
) -> Result<bool, sqlx::Error> {
    Ok(reuse_attestation_is_current!(
        db,
        transaction,
        avionics_model_id
    ))
}

pub(crate) async fn reuse_attestation_is_current_postgres(
    db: &AppDb,
    transaction: &mut Transaction<'_, Postgres>,
    avionics_model_id: i64,
) -> Result<bool, sqlx::Error> {
    Ok(reuse_attestation_is_current!(
        db,
        transaction,
        avionics_model_id
    ))
}

#[cfg(test)]
mod tests {
    use super::{fingerprint_rows, ReuseFingerprintRow, AVIONICS_REUSE_POLICY_VERSION};

    fn row(capability: &str) -> ReuseFingerprintRow {
        ReuseFingerprintRow {
            avionics_model_id: 7,
            avionics_manufacturer_id: 3,
            manufacturer_name: "Garmin".to_string(),
            manufacturer_normalized_name: "garmin".to_string(),
            avionics_manufacturer_identity_id: 2,
            manufacturer_identity_name: "Garmin".to_string(),
            manufacturer_identity_key: "garmin".to_string(),
            model_name: "GIA 63W".to_string(),
            model_normalized_name: "gia 63w".to_string(),
            manufacturer_identifier_kind: Some("manufacturer_model_number".to_string()),
            manufacturer_identifier: Some("GIA 63W".to_string()),
            normalized_manufacturer_identifier: Some("gia63w".to_string()),
            identity_source_url: Some(
                "https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf".to_string(),
            ),
            identity_source_title: Some("Garmin GIA 63/GIA 63W Installation Manual".to_string()),
            identity_evidence_text: Some(
                "GIA 63W Unit Only, (011-01105-00) 010-00386-00".to_string(),
            ),
            identity_evidence_kind: Some("authoritative_reference".to_string()),
            identity_confidence: Some("very_high".to_string()),
            canonical_product_key: "gia63w".to_string(),
            canonical_identifier_key: "gia63w".to_string(),
            source_origin_id: 5,
            source_origin: "https://static.garmin.com".to_string(),
            capability_name: capability.to_string(),
            capability_normalized_name: capability.to_ascii_lowercase(),
        }
    }

    #[test]
    fn fingerprint_is_order_independent_but_identity_and_capability_bound() {
        assert_eq!(AVIONICS_REUSE_POLICY_VERSION, "avionics_reuse_v2");
        let com = row("COM");
        let nav = row("NAV");
        let expected = fingerprint_rows(&[com.clone(), nav.clone()]).unwrap();
        assert_eq!(
            fingerprint_rows(&[nav.clone(), com.clone()]).as_deref(),
            Some(expected.as_str())
        );

        let mut changed_identity = com.clone();
        changed_identity.identity_evidence_text = Some("A different publisher excerpt".to_string());
        assert_ne!(
            fingerprint_rows(&[changed_identity, nav.clone()]).as_deref(),
            Some(expected.as_str())
        );
        assert_ne!(
            fingerprint_rows(&[com, row("GPS")]).as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn fingerprint_is_bound_to_the_attested_origin_and_requires_product_evidence() {
        let mut wrong_origin = row("COM");
        wrong_origin.source_origin = "https://www.garmin.com".to_string();
        assert_ne!(
            fingerprint_rows(&[wrong_origin]),
            fingerprint_rows(&[row("COM")])
        );

        let mut missing_evidence = row("COM");
        missing_evidence.identity_evidence_text = None;
        assert!(fingerprint_rows(&[missing_evidence]).is_none());
    }
}
