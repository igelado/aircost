//! Evidence-backed avionics manufacturer identities and alias review.
//!
//! Raw manufacturer rows preserve source spelling. A trusted identity groups
//! those rows only after deterministic exact-normalization or authoritative
//! alias review. Membership rows are immutable; uncertain semantic matches
//! remain candidates and cannot namespace approved products.

use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Postgres, Sqlite, Transaction};

use crate::db::{AppDb, DatabaseBackend};
use crate::normalize::{
    normalize_avionics_identifier, normalize_avionics_manufacturer_name,
    normalize_avionics_model_name,
};

const DETERMINISTIC_SOURCE_URL: &str =
    "urn:aircost:deterministic:avionics-manufacturer-normalization:v1";
const DETERMINISTIC_SOURCE_TITLE: &str = "Aircost exact manufacturer normalization v1";

#[derive(Debug)]
pub enum ManufacturerIdentityError {
    Validation(String),
    Conflict(String),
    Database(String),
}

impl fmt::Display for ManufacturerIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Conflict(message) | Self::Database(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for ManufacturerIdentityError {}

impl From<sqlx::Error> for ManufacturerIdentityError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

pub type ManufacturerIdentityResult<T> = Result<T, ManufacturerIdentityError>;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ManufacturerIdentityEvidence {
    pub source_url: String,
    pub source_title: String,
    pub evidence_text: String,
}

#[derive(Clone, Debug, FromRow, PartialEq, Eq, Serialize)]
pub struct ManufacturerIdentity {
    pub id: i64,
    pub canonical_name: String,
    pub normalized_identity_key: String,
    pub identity_evidence_kind: String,
    pub identity_source_url: String,
    pub identity_source_title: String,
    pub identity_evidence_text: String,
    pub identity_confidence: String,
}

#[derive(Clone, Debug, FromRow, PartialEq, Eq, Serialize)]
pub struct ManufacturerIdentityMembership {
    pub avionics_manufacturer_id: i64,
    pub avionics_manufacturer_identity_id: i64,
    pub membership_basis: String,
    pub normalized_name_key: String,
    pub evidence_source_url: String,
    pub evidence_source_title: String,
    pub evidence_text: String,
    pub evidence_confidence: String,
}

/// Exact source origins admitted for one already-curated manufacturer claim.
///
/// `canonical_origins` contains only the caller-submitted origins that were
/// admitted, not every origin known for the manufacturer. It is therefore safe
/// to retain as the binding for the subsequent fetch and final-URL check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ManufacturerSourceOriginAdmission {
    pub avionics_manufacturer_id: i64,
    pub effective_manufacturer_identity_id: i64,
    pub canonical_origins: Vec<String>,
}

impl ManufacturerSourceOriginAdmission {
    /// Require a server fetch to finish on the exact origin that was requested
    /// and admitted. Cross-origin redirects remain forbidden even when both
    /// origins are independently authorized for the manufacturer.
    pub fn require_authorized_final_url(
        &self,
        requested_url: &str,
        final_url: &str,
    ) -> ManufacturerIdentityResult<String> {
        let requested_origin = canonical_exact_https_origin(requested_url)?;
        if self
            .canonical_origins
            .binary_search(&requested_origin)
            .is_err()
        {
            return Err(ManufacturerIdentityError::Validation(format!(
                "requested source origin {requested_origin:?} is not bound to this manufacturer admission"
            )));
        }
        let final_origin = canonical_exact_https_origin(final_url)?;
        if final_origin != requested_origin {
            return Err(ManufacturerIdentityError::Validation(format!(
                "authoritative source redirected across exact origins from {requested_origin:?} to {final_origin:?}"
            )));
        }
        Ok(final_origin)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ManufacturerProductAdmission<'a> {
    pub manufacturer: &'a str,
    pub model: &'a str,
    pub manufacturer_identifier_kind: &'a str,
    pub manufacturer_identifier: &'a str,
    pub evidence: ManufacturerIdentityEvidence,
    /// Additional server-accepted structured claim sources (for example,
    /// collision adjudications) that authorized this write.
    pub additional_evidence_source_urls: &'a [String],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdmittedManufacturerProductScope {
    pub avionics_manufacturer_id: i64,
    pub avionics_manufacturer_identity_id: i64,
    pub normalized_manufacturer: String,
    pub canonical_product_key: String,
    pub canonical_identifier_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ManufacturerProductAdmissionOutcome {
    Admitted(AdmittedManufacturerProductScope),
    PendingAliasReview,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ManufacturerAliasApproval {
    pub membership: ManufacturerIdentityMembership,
    pub effective_manufacturer_identity_id: i64,
    pub identity_merge_created: bool,
    pub blocking_product_collision_count: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasCandidateBasis {
    ExactProductName,
    ExactStableIdentifier,
    SemanticSimilarity,
    GroundedAlias,
}

impl AliasCandidateBasis {
    fn as_str(&self) -> &'static str {
        match self {
            Self::ExactProductName => "exact_product_name",
            Self::ExactStableIdentifier => "exact_stable_identifier",
            Self::SemanticSimilarity => "semantic_similarity",
            Self::GroundedAlias => "grounded_alias",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct StageManufacturerAliasCandidateRequest {
    pub avionics_manufacturer_id: i64,
    pub candidate_manufacturer_identity_id: i64,
    pub candidate_basis: AliasCandidateBasis,
    pub matched_avionics_model_id: Option<i64>,
    pub reason: String,
    pub evidence: Option<ManufacturerIdentityEvidence>,
    pub confidence: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ApproveManufacturerAliasCandidateRequest {
    pub candidate_id: i64,
    pub evidence: ManufacturerIdentityEvidence,
    pub reviewed_by_user_id: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RejectManufacturerAliasCandidateRequest {
    pub candidate_id: i64,
    pub reason: String,
    pub reviewed_by_user_id: i64,
}

#[derive(Clone, Debug, FromRow, PartialEq, Eq, Serialize)]
pub struct ManufacturerAliasCandidate {
    pub id: i64,
    pub avionics_manufacturer_id: i64,
    pub candidate_manufacturer_identity_id: i64,
    pub candidate_basis: String,
    pub matched_avionics_model_id: Option<i64>,
    pub reason: String,
    pub evidence_source_url: Option<String>,
    pub evidence_source_title: Option<String>,
    pub evidence_text: Option<String>,
    pub confidence: String,
    pub review_status: String,
    pub decision_reason: Option<String>,
    pub reviewed_by_user_id: Option<i64>,
}

#[derive(Clone, Debug, FromRow, PartialEq, Eq, Serialize)]
pub struct LegacyManufacturerAliasSignal {
    pub candidate_basis: String,
    pub left_avionics_manufacturer_id: i64,
    pub left_manufacturer: String,
    pub left_avionics_model_id: i64,
    pub left_model: String,
    pub right_avionics_manufacturer_id: i64,
    pub right_manufacturer: String,
    pub right_avionics_model_id: i64,
    pub right_model: String,
}

#[derive(Clone, Debug, FromRow)]
struct ManufacturerRow {
    id: i64,
    name: String,
    normalized_name_key: String,
}

#[derive(Clone, Debug, FromRow)]
struct CandidateRow {
    id: i64,
    avionics_manufacturer_id: i64,
    candidate_manufacturer_identity_id: i64,
    review_status: String,
}

#[derive(Clone, Debug, FromRow)]
struct ApprovedMergeCandidateRow {
    id: i64,
    avionics_manufacturer_id: i64,
    candidate_manufacturer_identity_id: i64,
    decision_evidence_source_url: String,
    decision_evidence_source_title: String,
    decision_evidence_text: String,
    reviewed_by_user_id: i64,
}

fn validate_authoritative_evidence(
    evidence: &ManufacturerIdentityEvidence,
) -> ManufacturerIdentityResult<()> {
    if evidence.source_url.trim().is_empty()
        || evidence.source_title.trim().is_empty()
        || evidence.evidence_text.trim().is_empty()
    {
        return Err(ManufacturerIdentityError::Validation(
            "manufacturer identity evidence requires a source URL, title, and evidence text"
                .to_string(),
        ));
    }
    let source_url = evidence.source_url.trim().to_ascii_lowercase();
    if !source_url.starts_with("https://") {
        return Err(ManufacturerIdentityError::Validation(
            "manufacturer identity evidence must use an authoritative HTTPS source".to_string(),
        ));
    }
    if [
        "/listing/",
        "/listings/",
        "/aircraft-for-sale/",
        "/classifieds/",
    ]
    .iter()
    .any(|marker| source_url.contains(marker))
    {
        return Err(ManufacturerIdentityError::Validation(
            "ordinary sale listings cannot establish a manufacturer identity".to_string(),
        ));
    }
    let parsed = url::Url::parse(evidence.source_url.trim()).map_err(|_| {
        ManufacturerIdentityError::Validation(
            "manufacturer identity evidence URL is invalid".to_string(),
        )
    })?;
    let host = parsed
        .host_str()
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
        return Err(ManufacturerIdentityError::Validation(
            "marketplace or broker pages cannot establish a manufacturer identity".to_string(),
        ));
    }
    if evidence.source_title.trim().chars().count() < 4
        || evidence.evidence_text.trim().chars().count() < 20
    {
        return Err(ManufacturerIdentityError::Validation(
            "manufacturer identity evidence must contain a specific title and supporting fact"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_confidence(confidence: &str) -> ManufacturerIdentityResult<&str> {
    let confidence = confidence.trim();
    if !matches!(confidence, "very_high" | "high" | "medium" | "low") {
        return Err(ManufacturerIdentityError::Validation(format!(
            "unsupported manufacturer alias confidence {confidence:?}"
        )));
    }
    Ok(confidence)
}

fn manufacturer_acronym(value: &str) -> String {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.chars().next())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn plausible_semantic_alias(left: &str, right: &str) -> bool {
    let left_key = normalize_avionics_manufacturer_name(left)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    let right_key = normalize_avionics_manufacturer_name(right)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if left_key.is_empty() || right_key.is_empty() || left_key == right_key {
        return false;
    }
    let shorter = left_key.len().min(right_key.len());
    let prefix_match =
        shorter >= 3 && (left_key.starts_with(&right_key) || right_key.starts_with(&left_key));
    let left_acronym = manufacturer_acronym(left);
    let right_acronym = manufacturer_acronym(right);
    let acronym_match = (left_acronym.len() >= 2 && left_acronym == right_key)
        || (right_acronym.len() >= 2 && right_acronym == left_key);
    prefix_match || acronym_match
}

fn compact_identity_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Return the canonical exact HTTPS origin for a source URL.
///
/// Authority origins deliberately use ordinary DNS hosts on the default HTTPS
/// port. IP literals, credentials, non-HTTPS schemes, and non-default ports
/// cannot become catalog authority. The path, query, and fragment never widen
/// the returned origin.
pub fn canonical_exact_https_origin(source_url: &str) -> ManufacturerIdentityResult<String> {
    let source_url = source_url.trim();
    let parsed = url::Url::parse(source_url).map_err(|_| {
        ManufacturerIdentityError::Validation(
            "authoritative source must be an absolute HTTPS URL".to_string(),
        )
    })?;
    if parsed.scheme() != "https" {
        return Err(ManufacturerIdentityError::Validation(
            "authoritative source must use HTTPS".to_string(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ManufacturerIdentityError::Validation(
            "authoritative source URL cannot contain credentials".to_string(),
        ));
    }
    if parsed.port().is_some() {
        return Err(ManufacturerIdentityError::Validation(
            "authoritative source URL must use the default HTTPS port".to_string(),
        ));
    }
    let url::Host::Domain(host) = parsed.host().ok_or_else(|| {
        ManufacturerIdentityError::Validation(
            "authoritative source URL must contain a DNS host".to_string(),
        )
    })?
    else {
        return Err(ManufacturerIdentityError::Validation(
            "authoritative source URL must use a DNS host, not an IP literal".to_string(),
        ));
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let valid_host = host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if !valid_host {
        return Err(ManufacturerIdentityError::Validation(
            "authoritative source URL has an unsupported DNS host".to_string(),
        ));
    }
    Ok(format!("https://{host}"))
}

/// Admit caller-supplied direct-source URLs for an existing manufacturer.
///
/// The raw manufacturer spelling must already have an approved identity
/// membership. Origins are inherited only by following the append-only
/// effective identity graph, and revoked approvals are excluded by the active
/// authority view. No parent-domain or subdomain matching is performed.
pub async fn authorize_manufacturer_source_urls(
    db: &AppDb,
    manufacturer: &str,
    source_urls: &[String],
) -> ManufacturerIdentityResult<ManufacturerSourceOriginAdmission> {
    let normalized_manufacturer = normalize_avionics_manufacturer_name(manufacturer.trim());
    if normalized_manufacturer.is_empty() {
        return Err(ManufacturerIdentityError::Validation(
            "authoritative source admission requires a manufacturer".to_string(),
        ));
    }
    if source_urls.is_empty() {
        return Err(ManufacturerIdentityError::Validation(
            "authoritative source admission requires at least one source URL".to_string(),
        ));
    }

    let mut requested_origins = BTreeSet::new();
    for source_url in source_urls {
        requested_origins.insert(canonical_exact_https_origin(source_url)?);
    }

    let membership_sql = db.sql(
        r#"SELECT manufacturer.id,
                  membership.avionics_manufacturer_identity_id
           FROM avionics_manufacturers manufacturer
           JOIN avionics_manufacturer_effective_memberships membership
             ON membership.avionics_manufacturer_id = manufacturer.id
           WHERE manufacturer.normalized_name = ?"#,
    );
    let membership: Option<(i64, i64)> = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as(&membership_sql)
                .bind(normalized_manufacturer.as_str())
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as(&membership_sql)
                .bind(normalized_manufacturer.as_str())
                .fetch_optional(pool)
                .await?
        }
    };
    let (avionics_manufacturer_id, effective_manufacturer_identity_id) =
        membership.ok_or_else(|| {
            ManufacturerIdentityError::Conflict(format!(
                "manufacturer {manufacturer:?} has no approved effective identity for direct-source admission"
            ))
        })?;

    let authorized_sql = db.sql(
        r#"SELECT DISTINCT source_origin.https_origin
           FROM avionics_active_authoritative_source_origins source_origin
           JOIN avionics_manufacturer_effective_identities origin_identity
             ON origin_identity.identity_id =
                source_origin.avionics_manufacturer_identity_id
           WHERE source_origin.authority_kind = 'manufacturer_primary'
             AND origin_identity.avionics_manufacturer_identity_id = ?
           ORDER BY source_origin.https_origin"#,
    );
    let authorized_origins: Vec<String> = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar(&authorized_sql)
                .bind(effective_manufacturer_identity_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar(&authorized_sql)
                .bind(effective_manufacturer_identity_id)
                .fetch_all(pool)
                .await?
        }
    };
    let authorized_origins = authorized_origins.into_iter().collect::<BTreeSet<_>>();
    let missing = requested_origins
        .difference(&authorized_origins)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(ManufacturerIdentityError::Validation(format!(
            "manufacturer {manufacturer:?} does not authorize exact source origin(s): {}",
            missing.join(", ")
        )));
    }

    Ok(ManufacturerSourceOriginAdmission {
        avionics_manufacturer_id,
        effective_manufacturer_identity_id,
        canonical_origins: requested_origins.into_iter().collect(),
    })
}

/// Reject evidence from any exact HTTPS origin that has an append-only
/// revocation, including ordinary Search/URL Context evidence that did not
/// enter through the direct-source admission path.
///
/// Revocation is deliberately origin-global: once any curated authority row
/// for an exact origin is revoked, a different manufacturer row cannot make
/// that same origin silently trustworthy again.
pub async fn require_source_urls_not_revoked(
    db: &AppDb,
    source_urls: &[String],
) -> ManufacturerIdentityResult<()> {
    let origins = source_urls
        .iter()
        .map(|source_url| canonical_exact_https_origin(source_url))
        .collect::<ManufacturerIdentityResult<BTreeSet<_>>>()?;
    if origins.is_empty() {
        return Ok(());
    }
    let revoked = revoked_authoritative_source_origins(db).await?;
    if let Some(origin) = origins.intersection(&revoked).next() {
        return Err(ManufacturerIdentityError::Validation(format!(
            "authoritative evidence origin {origin:?} has been revoked"
        )));
    }
    Ok(())
}

pub(crate) async fn revoked_authoritative_source_origins(
    db: &AppDb,
) -> ManufacturerIdentityResult<BTreeSet<String>> {
    let sql = db.sql(
        r#"SELECT DISTINCT source_origin.https_origin
           FROM avionics_authoritative_source_origins source_origin
           JOIN avionics_authoritative_source_origin_revocations revocation
             ON revocation.avionics_authoritative_source_origin_id =
                source_origin.id
           ORDER BY source_origin.https_origin"#,
    );
    let origins = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, String>(&sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, String>(&sql)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(origins.into_iter().collect())
}

macro_rules! admit_manufacturer_product_scope {
    ($db:expr, $transaction:expr, $request:expr) => {{
        let request = $request;
        validate_authoritative_evidence(&request.evidence)?;
        let evidence_origins = std::iter::once(&request.evidence.source_url)
            .chain(request.additional_evidence_source_urls.iter())
            .map(|source_url| canonical_exact_https_origin(source_url))
            .collect::<ManufacturerIdentityResult<BTreeSet<_>>>()?;
        // Hold revocation state stable from this final check through the
        // caller's product write and commit. SQLite's no-op write obtains the
        // writer reservation; PostgreSQL SHARE locks conflict with inserts.
        let authority_lock = match $db.backend() {
            DatabaseBackend::Sqlite(_) => $db.sql(
                r#"INSERT INTO avionics_authoritative_source_origin_revocations (
                     avionics_authoritative_source_origin_id,
                     revoked_by_user_id, reason
                   )
                   SELECT 0, 0, '' WHERE 0"#,
            ),
            DatabaseBackend::Postgres(_) => $db.sql(
                "LOCK TABLE avionics_authoritative_source_origins, avionics_authoritative_source_origin_revocations IN SHARE MODE",
            ),
        };
        sqlx::query(&authority_lock)
            .execute(&mut **$transaction)
            .await?;
        let revoked_source = $db.sql(
            r#"SELECT EXISTS (
                 SELECT 1
                 FROM avionics_authoritative_source_origins source_origin
                 JOIN avionics_authoritative_source_origin_revocations revocation
                   ON revocation.avionics_authoritative_source_origin_id =
                      source_origin.id
                 WHERE source_origin.https_origin = ?
               )"#,
        );
        for evidence_origin in evidence_origins {
            let evidence_origin_revoked = match $db.backend() {
                DatabaseBackend::Sqlite(_) => {
                    sqlx::query_scalar::<_, i64>(&revoked_source)
                        .bind(evidence_origin.as_str())
                        .fetch_one(&mut **$transaction)
                        .await?
                        != 0
                }
                DatabaseBackend::Postgres(_) => {
                    sqlx::query_scalar::<_, bool>(&revoked_source)
                        .bind(evidence_origin.as_str())
                        .fetch_one(&mut **$transaction)
                        .await?
                }
            };
            if evidence_origin_revoked {
                return Err(ManufacturerIdentityError::Validation(format!(
                    "manufacturer product evidence origin {evidence_origin:?} has been revoked"
                )));
            }
        }
        let normalized_manufacturer =
            normalize_avionics_manufacturer_name(request.manufacturer.trim());
        let canonical_product_key =
            compact_identity_key(&normalize_avionics_model_name(request.model.trim()));
        let canonical_identifier_key =
            normalize_avionics_identifier(request.manufacturer_identifier.trim());
        if normalized_manufacturer.is_empty()
            || canonical_product_key.is_empty()
            || canonical_identifier_key.is_empty()
        {
            return Err(ManufacturerIdentityError::Validation(
                "manufacturer product admission requires deterministic manufacturer, product, and identifier keys"
                    .to_string(),
            ));
        }
        if !matches!(
            request.manufacturer_identifier_kind.trim(),
            "manufacturer_part_number" | "manufacturer_model_number" | "sku"
        ) {
            return Err(ManufacturerIdentityError::Validation(format!(
                "unsupported manufacturer identifier kind {:?}",
                request.manufacturer_identifier_kind.trim()
            )));
        }

        let insert_manufacturer = $db.sql(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
        );
        sqlx::query(&insert_manufacturer)
            .bind(request.manufacturer.trim())
            .bind(normalized_manufacturer.as_str())
            .execute(&mut **$transaction)
            .await?;
        let select_manufacturer =
            $db.sql("SELECT id FROM avionics_manufacturers WHERE normalized_name = ?");
        let manufacturer_id: i64 = sqlx::query_scalar(&select_manufacturer)
            .bind(normalized_manufacturer.as_str())
            .fetch_one(&mut **$transaction)
            .await?;

        let select_effective_membership = $db.sql(
            r#"SELECT avionics_manufacturer_identity_id
               FROM avionics_manufacturer_effective_memberships
               WHERE avionics_manufacturer_id = ?"#,
        );
        let existing_effective_identity: Option<i64> =
            sqlx::query_scalar(&select_effective_membership)
                .bind(manufacturer_id)
                .fetch_optional(&mut **$transaction)
                .await?;

        // Keep the original identity ID for an immutable deterministic
        // membership. If that identity was later redirected, the effective
        // view below resolves it without trying to forge a membership against
        // the survivor's different normalization key.
        let exact_identity: Option<(i64, i64)> = if existing_effective_identity.is_none() {
            let select_exact_identity = $db.sql(
                r#"SELECT identity.id,
                          effective.avionics_manufacturer_identity_id
                   FROM avionics_manufacturer_identities identity
                   JOIN avionics_manufacturer_effective_identities effective
                     ON effective.identity_id = identity.id
                   WHERE identity.normalized_identity_key = ?"#,
            );
            sqlx::query_as(&select_exact_identity)
                .bind(normalized_manufacturer.as_str())
                .fetch_optional(&mut **$transaction)
                .await?
        } else {
            None
        };
        let current_effective_identity =
            existing_effective_identity.or(exact_identity.map(|identity| identity.1));

        // Cross-namespace matches are alias-review signals, not product
        // collisions. This check is mandatory even for an already-admitted
        // raw maker: two existing identities must not acquire products with
        // the same stable identifier or canonical product key independently.
        let select_product_alias_signals = $db.sql(
            r#"SELECT DISTINCT
                 graph.avionics_manufacturer_identity_id,
                 graph.avionics_model_id,
                 CASE
                   WHEN graph.manufacturer_identifier_kind = ?
                    AND graph.canonical_identifier_key = ?
                   THEN 'exact_stable_identifier'
                   ELSE 'exact_product_name'
                 END AS candidate_basis
               FROM avionics_approved_product_graph_identities graph
               WHERE (
                   graph.manufacturer_identifier_kind = ?
                   AND graph.canonical_identifier_key = ?
                 )
                  OR graph.canonical_product_key = ?
               ORDER BY graph.avionics_manufacturer_identity_id,
                        graph.avionics_model_id"#,
        );
        let product_alias_signals: Vec<(i64, i64, String)> =
            sqlx::query_as(&select_product_alias_signals)
                .bind(request.manufacturer_identifier_kind.trim())
                .bind(canonical_identifier_key.as_str())
                .bind(request.manufacturer_identifier_kind.trim())
                .bind(canonical_identifier_key.as_str())
                .bind(canonical_product_key.as_str())
                .fetch_all(&mut **$transaction)
                .await?;
        let mut alias_targets =
            std::collections::BTreeMap::<i64, (String, Option<i64>)>::new();
        for (candidate_identity_id, matched_model_id, basis) in product_alias_signals {
            if Some(candidate_identity_id) != current_effective_identity {
                let entry = alias_targets
                    .entry(candidate_identity_id)
                    .or_insert((basis.clone(), Some(matched_model_id)));
                if basis == "exact_stable_identifier" {
                    *entry = (basis, Some(matched_model_id));
                }
            }
        }
        // Name similarity helps admit a previously unseen raw maker, but it
        // cannot repeatedly challenge an already evidence-backed identity.
        if existing_effective_identity.is_none() {
            let select_identity_names = $db.sql(
                r#"SELECT effective.avionics_manufacturer_identity_id,
                          identity.canonical_name
                   FROM avionics_manufacturer_identities identity
                   JOIN avionics_manufacturer_effective_identities effective
                     ON effective.identity_id = identity.id
                   ORDER BY effective.avionics_manufacturer_identity_id,
                            identity.id"#,
            );
            let identity_names: Vec<(i64, String)> = sqlx::query_as(&select_identity_names)
                .fetch_all(&mut **$transaction)
                .await?;
            for (candidate_identity_id, candidate_name) in identity_names {
                if Some(candidate_identity_id) != current_effective_identity
                    && plausible_semantic_alias(request.manufacturer, &candidate_name)
                {
                    alias_targets
                        .entry(candidate_identity_id)
                        .or_insert(("semantic_similarity".to_string(), None));
                }
            }
        }
        if !alias_targets.is_empty() {
            let insert_candidate = $db.sql(
                r#"INSERT INTO avionics_manufacturer_alias_candidates (
                     avionics_manufacturer_id,
                     candidate_manufacturer_identity_id,
                     candidate_basis, matched_avionics_model_id,
                     reason, confidence
                   ) VALUES (?, ?, ?, ?, ?, ?)
                   ON CONFLICT DO NOTHING"#,
            );
            for (candidate_identity_id, (basis, matched_model_id)) in alias_targets {
                let reason = match basis.as_str() {
                    "exact_stable_identifier" =>
                        "The proposed product shares an exact stable manufacturer identifier with an approved product under this evidence-backed manufacturer identity.",
                    "exact_product_name" =>
                        "The proposed product shares an exact canonical product name with an approved product under this evidence-backed manufacturer identity.",
                    _ =>
                        "The proposed manufacturer name is a plausible semantic alias of this evidence-backed manufacturer identity.",
                };
                let confidence = if basis == "semantic_similarity" {
                    "medium"
                } else {
                    "high"
                };
                sqlx::query(&insert_candidate)
                    .bind(manufacturer_id)
                    .bind(candidate_identity_id)
                    .bind(&basis)
                    .bind(matched_model_id)
                    .bind(reason)
                    .bind(confidence)
                    .execute(&mut **$transaction)
                    .await?;
            }
            ManufacturerProductAdmissionOutcome::PendingAliasReview
        } else if let Some(effective_identity_id) = existing_effective_identity {
            ManufacturerProductAdmissionOutcome::Admitted(AdmittedManufacturerProductScope {
                avionics_manufacturer_id: manufacturer_id,
                avionics_manufacturer_identity_id: effective_identity_id,
                normalized_manufacturer,
                canonical_product_key,
                canonical_identifier_key,
            })
        } else {
            let (membership_identity_id, new_identity) =
                if let Some((original_identity_id, _)) = exact_identity {
                    (original_identity_id, false)
                } else {
                    let insert_identity = $db.sql(
                        r#"INSERT INTO avionics_manufacturer_identities (
                             canonical_name, normalized_identity_key,
                             identity_evidence_kind,
                             identity_source_url, identity_source_title,
                             identity_evidence_text, identity_confidence
                           ) VALUES (
                             ?, ?, 'authoritative_reference',
                             ?, ?, ?, 'very_high'
                           )
                           RETURNING id"#,
                    );
                    let identity_id: i64 = sqlx::query_scalar(&insert_identity)
                        .bind(request.manufacturer.trim())
                        .bind(normalized_manufacturer.as_str())
                        .bind(request.evidence.source_url.trim())
                        .bind(request.evidence.source_title.trim())
                        .bind(request.evidence.evidence_text.trim())
                        .fetch_one(&mut **$transaction)
                        .await?;
                    (identity_id, true)
                };
            let insert_membership = $db.sql(
                r#"INSERT INTO avionics_manufacturer_identity_memberships (
                     avionics_manufacturer_id,
                     avionics_manufacturer_identity_id,
                     membership_basis, normalized_name_key,
                     evidence_source_url, evidence_source_title,
                     evidence_text, evidence_confidence
                   ) VALUES (?, ?, ?, ?, ?, ?, ?, 'very_high')"#,
            );
            let (basis, source_url, source_title, evidence_text) = if new_identity {
                (
                    "authoritative_primary",
                    request.evidence.source_url.trim(),
                    request.evidence.source_title.trim(),
                    request.evidence.evidence_text.trim(),
                )
            } else {
                (
                    "deterministic_exact",
                    DETERMINISTIC_SOURCE_URL,
                    DETERMINISTIC_SOURCE_TITLE,
                    "The stored manufacturer spelling has the same exact deterministic normalization key as this evidence-backed identity.",
                )
            };
            sqlx::query(&insert_membership)
                .bind(manufacturer_id)
                .bind(membership_identity_id)
                .bind(basis)
                .bind(normalized_manufacturer.as_str())
                .bind(source_url)
                .bind(source_title)
                .bind(evidence_text)
                .execute(&mut **$transaction)
                .await?;
            let insert_exact_memberships = $db.sql(
                r#"INSERT INTO avionics_manufacturer_identity_memberships (
                     avionics_manufacturer_id,
                     avionics_manufacturer_identity_id,
                     membership_basis, normalized_name_key,
                     evidence_source_url, evidence_source_title,
                     evidence_text, evidence_confidence
                   )
                   SELECT manufacturer_key.avionics_manufacturer_id, ?,
                          'deterministic_exact',
                          manufacturer_key.canonical_manufacturer_key,
                          'urn:aircost:deterministic:avionics-manufacturer-normalization:v1',
                          'Aircost exact manufacturer normalization v1',
                          'The stored manufacturer spelling has the same exact deterministic normalization key as this evidence-backed identity.',
                          'very_high'
                   FROM avionics_manufacturer_canonical_keys manufacturer_key
                   LEFT JOIN avionics_manufacturer_identity_memberships membership
                     ON membership.avionics_manufacturer_id
                       = manufacturer_key.avionics_manufacturer_id
                   WHERE manufacturer_key.canonical_manufacturer_key = ?
                     AND membership.avionics_manufacturer_id IS NULL
                   ON CONFLICT DO NOTHING"#,
            );
            sqlx::query(&insert_exact_memberships)
                .bind(membership_identity_id)
                .bind(normalized_manufacturer.as_str())
                .execute(&mut **$transaction)
                .await?;
            let effective_identity_id: i64 =
                sqlx::query_scalar(&select_effective_membership)
                    .bind(manufacturer_id)
                    .fetch_one(&mut **$transaction)
                    .await?;
            ManufacturerProductAdmissionOutcome::Admitted(AdmittedManufacturerProductScope {
                avionics_manufacturer_id: manufacturer_id,
                avionics_manufacturer_identity_id: effective_identity_id,
                normalized_manufacturer,
                canonical_product_key,
                canonical_identifier_key,
            })
        }
    }};
}

pub(crate) async fn admit_manufacturer_product_scope_sqlite(
    db: &AppDb,
    transaction: &mut Transaction<'_, Sqlite>,
    request: &ManufacturerProductAdmission<'_>,
) -> ManufacturerIdentityResult<ManufacturerProductAdmissionOutcome> {
    Ok(admit_manufacturer_product_scope!(db, transaction, request))
}

pub(crate) async fn admit_manufacturer_product_scope_postgres(
    db: &AppDb,
    transaction: &mut Transaction<'_, Postgres>,
    request: &ManufacturerProductAdmission<'_>,
) -> ManufacturerIdentityResult<ManufacturerProductAdmissionOutcome> {
    Ok(admit_manufacturer_product_scope!(db, transaction, request))
}

macro_rules! stage_batch_manufacturer_alias_collision {
    (
        $db:expr,
        $transaction:expr,
        $avionics_manufacturer_id:expr,
        $candidate_manufacturer_identity_id:expr,
        $candidate_basis:expr
    ) => {{
        let avionics_manufacturer_id = $avionics_manufacturer_id;
        let candidate_manufacturer_identity_id = $candidate_manufacturer_identity_id;
        let candidate_basis = $candidate_basis;
        if avionics_manufacturer_id <= 0 || candidate_manufacturer_identity_id <= 0 {
            return Err(ManufacturerIdentityError::Validation(
                "batch alias collision requires positive manufacturer and identity IDs".to_string(),
            ));
        }
        if !matches!(
            candidate_basis,
            "exact_stable_identifier" | "exact_product_name"
        ) {
            return Err(ManufacturerIdentityError::Validation(format!(
                "unsupported batch alias collision basis {candidate_basis:?}"
            )));
        }
        let select_source_identity = $db.sql(
            r#"SELECT avionics_manufacturer_identity_id
               FROM avionics_manufacturer_effective_memberships
               WHERE avionics_manufacturer_id = ?"#,
        );
        let source_identity_id: i64 = sqlx::query_scalar(&select_source_identity)
            .bind(avionics_manufacturer_id)
            .fetch_optional(&mut **$transaction)
            .await?
            .ok_or_else(|| {
                ManufacturerIdentityError::Conflict(format!(
                    "batch alias source manufacturer {avionics_manufacturer_id} has no effective identity"
                ))
            })?;
        let select_target_identity = $db.sql(
            r#"SELECT avionics_manufacturer_identity_id
               FROM avionics_manufacturer_effective_identities
               WHERE identity_id = ?"#,
        );
        let target_identity_id: i64 = sqlx::query_scalar(&select_target_identity)
            .bind(candidate_manufacturer_identity_id)
            .fetch_optional(&mut **$transaction)
            .await?
            .ok_or_else(|| {
                ManufacturerIdentityError::Conflict(format!(
                    "batch alias target identity {candidate_manufacturer_identity_id} has no effective root"
                ))
            })?;
        if source_identity_id == target_identity_id {
            return Err(ManufacturerIdentityError::Conflict(format!(
                "batch alias collision cannot target the source manufacturer's own effective identity {source_identity_id}"
            )));
        }
        let (reason, confidence) = match candidate_basis {
            "exact_stable_identifier" => (
                "Two products proposed in the same listing-review batch under distinct evidence-backed manufacturer identities share an exact stable manufacturer identifier kind and value.",
                "high",
            ),
            "exact_product_name" => (
                "Two products proposed in the same listing-review batch under distinct evidence-backed manufacturer identities share an exact canonical product name.",
                "medium",
            ),
            _ => unreachable!("batch alias basis was validated"),
        };
        let insert_candidate = $db.sql(
            r#"INSERT INTO avionics_manufacturer_alias_candidates (
                 avionics_manufacturer_id,
                 candidate_manufacturer_identity_id,
                 candidate_basis, matched_avionics_model_id,
                 reason, confidence
               ) VALUES (?, ?, ?, NULL, ?, ?)
               ON CONFLICT DO NOTHING"#,
        );
        sqlx::query(&insert_candidate)
            .bind(avionics_manufacturer_id)
            .bind(target_identity_id)
            .bind(candidate_basis)
            .bind(reason)
            .bind(confidence)
            .execute(&mut **$transaction)
            .await?
            .rows_affected()
    }};
}

pub(crate) async fn stage_batch_manufacturer_alias_collision_sqlite(
    db: &AppDb,
    transaction: &mut Transaction<'_, Sqlite>,
    avionics_manufacturer_id: i64,
    candidate_manufacturer_identity_id: i64,
    candidate_basis: &str,
) -> ManufacturerIdentityResult<u64> {
    Ok(stage_batch_manufacturer_alias_collision!(
        db,
        transaction,
        avionics_manufacturer_id,
        candidate_manufacturer_identity_id,
        candidate_basis
    ))
}

pub(crate) async fn stage_batch_manufacturer_alias_collision_postgres(
    db: &AppDb,
    transaction: &mut Transaction<'_, Postgres>,
    avionics_manufacturer_id: i64,
    candidate_manufacturer_identity_id: i64,
    candidate_basis: &str,
) -> ManufacturerIdentityResult<u64> {
    Ok(stage_batch_manufacturer_alias_collision!(
        db,
        transaction,
        avionics_manufacturer_id,
        candidate_manufacturer_identity_id,
        candidate_basis
    ))
}

fn identity_select_sql() -> &'static str {
    r#"SELECT id, canonical_name, normalized_identity_key,
              identity_evidence_kind, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_confidence
       FROM avionics_manufacturer_identities
       WHERE id = ?"#
}

fn membership_select_sql() -> &'static str {
    r#"SELECT avionics_manufacturer_id, avionics_manufacturer_identity_id,
              membership_basis, normalized_name_key, evidence_source_url,
              evidence_source_title, evidence_text, evidence_confidence
       FROM avionics_manufacturer_identity_memberships
       WHERE avionics_manufacturer_id = ?"#
}

/// Ensure one raw manufacturer has a stable identity before approving one of
/// its products. Exact-safe spellings join an existing deterministic group;
/// otherwise the authoritative source establishes a new primary identity.
pub async fn ensure_manufacturer_identity(
    db: &AppDb,
    avionics_manufacturer_id: i64,
    evidence: &ManufacturerIdentityEvidence,
) -> ManufacturerIdentityResult<ManufacturerIdentityMembership> {
    if avionics_manufacturer_id <= 0 {
        return Err(ManufacturerIdentityError::Validation(
            "avionics_manufacturer_id must be positive".to_string(),
        ));
    }
    validate_authoritative_evidence(evidence)?;

    let manufacturer_sql = db.sql(
        r#"SELECT manufacturer.id, manufacturer.name,
                  canonical_key.canonical_manufacturer_key AS normalized_name_key
           FROM avionics_manufacturers manufacturer
           JOIN avionics_manufacturer_canonical_keys canonical_key
             ON canonical_key.avionics_manufacturer_id = manufacturer.id
           WHERE manufacturer.id = ?"#,
    );
    let membership_sql = db.sql(membership_select_sql());
    let identity_by_key_sql = db.sql(
        r#"SELECT id, canonical_name, normalized_identity_key,
                  identity_evidence_kind, identity_source_url,
                  identity_source_title, identity_evidence_text,
                  identity_confidence
           FROM avionics_manufacturer_identities
           WHERE normalized_identity_key = ?"#,
    );
    let insert_identity_sql = db.sql(
        r#"INSERT INTO avionics_manufacturer_identities (
             canonical_name, normalized_identity_key, identity_evidence_kind,
             identity_source_url, identity_source_title,
             identity_evidence_text, identity_confidence
           ) VALUES (?, ?, 'authoritative_reference', ?, ?, ?, 'very_high')
           RETURNING id"#,
    );
    let insert_membership_sql = db.sql(
        r#"INSERT INTO avionics_manufacturer_identity_memberships (
             avionics_manufacturer_id, avionics_manufacturer_identity_id,
             membership_basis, normalized_name_key, evidence_source_url,
             evidence_source_title, evidence_text, evidence_confidence
           ) VALUES (?, ?, ?, ?, ?, ?, ?, 'very_high')"#,
    );
    let insert_exact_memberships_sql = db.sql(
        r#"INSERT INTO avionics_manufacturer_identity_memberships (
             avionics_manufacturer_id, avionics_manufacturer_identity_id,
             membership_basis, normalized_name_key, evidence_source_url,
             evidence_source_title, evidence_text, evidence_confidence
           )
           SELECT manufacturer_key.avionics_manufacturer_id, ?,
                  'deterministic_exact',
                  manufacturer_key.canonical_manufacturer_key,
                  'urn:aircost:deterministic:avionics-manufacturer-normalization:v1',
                  'Aircost exact manufacturer normalization v1',
                  'The stored manufacturer spelling has the same exact deterministic normalization key as this identity.',
                  'very_high'
           FROM avionics_manufacturer_canonical_keys manufacturer_key
           LEFT JOIN avionics_manufacturer_identity_memberships membership
             ON membership.avionics_manufacturer_id
               = manufacturer_key.avionics_manufacturer_id
           WHERE manufacturer_key.canonical_manufacturer_key = ?
             AND manufacturer_key.avionics_manufacturer_id <> ?
             AND membership.avionics_manufacturer_id IS NULL"#,
    );
    let identity_lock_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            r#"INSERT INTO avionics_manufacturer_identities (
                 canonical_name, normalized_identity_key,
                 identity_evidence_kind, identity_source_url,
                 identity_source_title, identity_evidence_text,
                 identity_confidence
               )
               SELECT '', '', 'authoritative_reference', '', '', '', 'very_high'
               WHERE 0"#,
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            "LOCK TABLE avionics_manufacturers, avionics_manufacturer_canonical_keys, avionics_manufacturer_identities, avionics_manufacturer_identity_memberships, avionics_manufacturer_identity_merges, avionics_manufacturer_alias_candidates IN SHARE ROW EXCLUSIVE MODE",
        ),
    };

    macro_rules! ensure {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&identity_lock_sql)
                .execute(&mut *transaction)
                .await?;
            if let Some(existing) =
                sqlx::query_as::<_, ManufacturerIdentityMembership>(&membership_sql)
                    .bind(avionics_manufacturer_id)
                    .fetch_optional(&mut *transaction)
                    .await?
            {
                transaction.commit().await?;
                return Ok(existing);
            }
            let manufacturer: ManufacturerRow =
                sqlx::query_as::<_, ManufacturerRow>(&manufacturer_sql)
                    .bind(avionics_manufacturer_id)
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or_else(|| {
                        ManufacturerIdentityError::Validation(format!(
                            "avionics manufacturer {avionics_manufacturer_id} does not exist"
                        ))
                    })?;
            let existing_identity =
                sqlx::query_as::<_, ManufacturerIdentity>(&identity_by_key_sql)
                    .bind(&manufacturer.normalized_name_key)
                    .fetch_optional(&mut *transaction)
                    .await?;
            let (identity_id, basis, source_url, source_title, evidence_text) =
                if let Some(identity) = existing_identity {
                    (
                        identity.id,
                        "deterministic_exact",
                        DETERMINISTIC_SOURCE_URL,
                        DETERMINISTIC_SOURCE_TITLE,
                        "The stored manufacturer spelling has the same exact deterministic normalization key as this identity.",
                    )
                } else {
                    let identity_id: i64 = sqlx::query_scalar(&insert_identity_sql)
                        .bind(manufacturer.name.trim())
                        .bind(&manufacturer.normalized_name_key)
                        .bind(evidence.source_url.trim())
                        .bind(evidence.source_title.trim())
                        .bind(evidence.evidence_text.trim())
                        .fetch_one(&mut *transaction)
                        .await?;
                    (
                        identity_id,
                        "authoritative_primary",
                        evidence.source_url.trim(),
                        evidence.source_title.trim(),
                        evidence.evidence_text.trim(),
                    )
                };
            sqlx::query(&insert_membership_sql)
                .bind(manufacturer.id)
                .bind(identity_id)
                .bind(basis)
                .bind(&manufacturer.normalized_name_key)
                .bind(source_url)
                .bind(source_title)
                .bind(evidence_text)
                .execute(&mut *transaction)
                .await?;
            sqlx::query(&insert_exact_memberships_sql)
                .bind(identity_id)
                .bind(&manufacturer.normalized_name_key)
                .bind(manufacturer.id)
                .execute(&mut *transaction)
                .await?;
            let membership =
                sqlx::query_as::<_, ManufacturerIdentityMembership>(&membership_sql)
                    .bind(manufacturer.id)
                    .fetch_one(&mut *transaction)
                    .await?;
            transaction.commit().await?;
            Ok::<_, ManufacturerIdentityError>(membership)
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => ensure!(pool),
        DatabaseBackend::Postgres(pool) => ensure!(pool),
    }
}

/// Report non-authoritative legacy catalog signals without turning either raw
/// spelling into a curated identity. These rows are review input only.
pub async fn list_legacy_manufacturer_alias_signals(
    db: &AppDb,
) -> ManufacturerIdentityResult<Vec<LegacyManufacturerAliasSignal>> {
    let sql = db.sql(
        r#"SELECT candidate_basis, left_avionics_manufacturer_id,
                  left_manufacturer, left_avionics_model_id, left_model,
                  right_avionics_manufacturer_id, right_manufacturer,
                  right_avionics_model_id, right_model
           FROM avionics_legacy_manufacturer_alias_signals
           ORDER BY candidate_basis, left_avionics_manufacturer_id,
                    right_avionics_manufacturer_id, left_avionics_model_id,
                    right_avionics_model_id"#,
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as(&sql).fetch_all(pool).await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_as(&sql).fetch_all(pool).await?),
    }
}

/// Once one manufacturer identity has authoritative evidence, convert exact
/// cross-maker legacy product signals into idempotent pending review work.
///
/// This never assigns a membership or approves a merge. Raw makers that
/// already resolve to any identity are left for explicit identity-to-identity
/// review rather than being silently reparented.
pub async fn stage_exact_alias_candidates_for_identity(
    db: &AppDb,
    manufacturer_identity_id: i64,
) -> ManufacturerIdentityResult<u64> {
    if manufacturer_identity_id <= 0 {
        return Err(ManufacturerIdentityError::Validation(
            "manufacturer_identity_id must be positive".to_string(),
        ));
    }
    let sql = db.sql(
        r#"INSERT INTO avionics_manufacturer_alias_candidates (
             avionics_manufacturer_id, candidate_manufacturer_identity_id,
             candidate_basis, matched_avionics_model_id, reason, confidence
           )
           SELECT DISTINCT
             signal.right_avionics_manufacturer_id,
             ?,
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
           FROM avionics_legacy_manufacturer_alias_signals signal
           JOIN avionics_manufacturer_effective_memberships left_membership
             ON left_membership.avionics_manufacturer_id
               = signal.left_avionics_manufacturer_id
            AND left_membership.avionics_manufacturer_identity_id = ?
           LEFT JOIN avionics_manufacturer_identity_memberships right_membership
             ON right_membership.avionics_manufacturer_id
               = signal.right_avionics_manufacturer_id
           WHERE right_membership.avionics_manufacturer_id IS NULL
             AND NOT EXISTS (
               SELECT 1
               FROM avionics_manufacturer_alias_candidates pending
               WHERE pending.avionics_manufacturer_id
                   = signal.right_avionics_manufacturer_id
                 AND pending.candidate_manufacturer_identity_id = ?
                 AND pending.review_status = 'pending'
             )
           ON CONFLICT DO NOTHING"#,
    );
    let changed = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query(&sql)
            .bind(manufacturer_identity_id)
            .bind(manufacturer_identity_id)
            .bind(manufacturer_identity_id)
            .execute(pool)
            .await?
            .rows_affected(),
        DatabaseBackend::Postgres(pool) => sqlx::query(&sql)
            .bind(manufacturer_identity_id)
            .bind(manufacturer_identity_id)
            .bind(manufacturer_identity_id)
            .execute(pool)
            .await?
            .rows_affected(),
    };
    Ok(changed)
}

/// Preserve an uncertain semantic alias as review state. This never changes a
/// manufacturer identity or product namespace.
pub async fn stage_manufacturer_alias_candidate(
    db: &AppDb,
    request: &StageManufacturerAliasCandidateRequest,
) -> ManufacturerIdentityResult<ManufacturerAliasCandidate> {
    if request.avionics_manufacturer_id <= 0 || request.candidate_manufacturer_identity_id <= 0 {
        return Err(ManufacturerIdentityError::Validation(
            "manufacturer and candidate identity IDs must be positive".to_string(),
        ));
    }
    let confidence = validate_confidence(&request.confidence)?;
    if request.reason.trim().is_empty() {
        return Err(ManufacturerIdentityError::Validation(
            "manufacturer alias candidate requires a reason".to_string(),
        ));
    }
    if let Some(evidence) = &request.evidence {
        validate_authoritative_evidence(evidence)?;
    }
    let evidence_url = request
        .evidence
        .as_ref()
        .map(|evidence| evidence.source_url.trim());
    let evidence_title = request
        .evidence
        .as_ref()
        .map(|evidence| evidence.source_title.trim());
    let evidence_text = request
        .evidence
        .as_ref()
        .map(|evidence| evidence.evidence_text.trim());
    let insert_sql = db.sql(
        r#"INSERT INTO avionics_manufacturer_alias_candidates (
             avionics_manufacturer_id, candidate_manufacturer_identity_id,
             candidate_basis, matched_avionics_model_id, reason,
             evidence_source_url, evidence_source_title, evidence_text,
             confidence
           ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
           RETURNING id"#,
    );
    let select_sql = db.sql(
        r#"SELECT id, avionics_manufacturer_id,
                  candidate_manufacturer_identity_id, candidate_basis,
                  matched_avionics_model_id, reason, evidence_source_url,
                  evidence_source_title, evidence_text, confidence,
                  review_status, decision_reason, reviewed_by_user_id
           FROM avionics_manufacturer_alias_candidates WHERE id = ?"#,
    );
    let select_pending_sql = db.sql(
        r#"SELECT id, avionics_manufacturer_id,
                  candidate_manufacturer_identity_id, candidate_basis,
                  matched_avionics_model_id, reason, evidence_source_url,
                  evidence_source_title, evidence_text, confidence,
                  review_status, decision_reason, reviewed_by_user_id
           FROM avionics_manufacturer_alias_candidates
           WHERE avionics_manufacturer_id = ?
             AND candidate_manufacturer_identity_id = ?
             AND review_status = 'pending'
           ORDER BY id
           LIMIT 1"#,
    );

    macro_rules! stage {
        ($pool:expr) => {{
            if let Some(existing) =
                sqlx::query_as::<_, ManufacturerAliasCandidate>(&select_pending_sql)
                    .bind(request.avionics_manufacturer_id)
                    .bind(request.candidate_manufacturer_identity_id)
                    .fetch_optional($pool)
                    .await?
            {
                return Ok(existing);
            }
            let candidate_id: i64 = sqlx::query_scalar(&insert_sql)
                .bind(request.avionics_manufacturer_id)
                .bind(request.candidate_manufacturer_identity_id)
                .bind(request.candidate_basis.as_str())
                .bind(request.matched_avionics_model_id)
                .bind(request.reason.trim())
                .bind(evidence_url)
                .bind(evidence_title)
                .bind(evidence_text)
                .bind(confidence)
                .fetch_one($pool)
                .await?;
            Ok::<_, ManufacturerIdentityError>(
                sqlx::query_as::<_, ManufacturerAliasCandidate>(&select_sql)
                    .bind(candidate_id)
                    .fetch_one($pool)
                    .await?,
            )
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => stage!(pool),
        DatabaseBackend::Postgres(pool) => stage!(pool),
    }
}

/// Approve one pending alias with independent authoritative evidence and add
/// its immutable membership. A manufacturer already assigned elsewhere cannot
/// be silently moved.
pub async fn approve_manufacturer_alias_candidate(
    db: &AppDb,
    request: &ApproveManufacturerAliasCandidateRequest,
) -> ManufacturerIdentityResult<ManufacturerAliasApproval> {
    if request.candidate_id <= 0 || request.reviewed_by_user_id <= 0 {
        return Err(ManufacturerIdentityError::Validation(
            "candidate_id and reviewed_by_user_id must be positive".to_string(),
        ));
    }
    validate_authoritative_evidence(&request.evidence)?;

    let candidate_sql = db.sql(
        r#"SELECT id, avionics_manufacturer_id,
                  candidate_manufacturer_identity_id, review_status
           FROM avionics_manufacturer_alias_candidates WHERE id = ?"#,
    );
    let manufacturer_key_sql = db.sql(
        r#"SELECT canonical_manufacturer_key
           FROM avionics_manufacturer_canonical_keys
           WHERE avionics_manufacturer_id = ?"#,
    );
    let current_membership_sql = db.sql(membership_select_sql());
    let effective_identity_sql = db.sql(
        r#"SELECT avionics_manufacturer_identity_id
           FROM avionics_manufacturer_effective_identities
           WHERE identity_id = ?"#,
    );
    let product_collision_sql = db.sql(
        r#"SELECT COUNT(*)
           FROM avionics_approved_product_graph_identities merged_product
           JOIN avionics_approved_product_graph_identities survivor_product
             ON survivor_product.avionics_manufacturer_identity_id = ?
            AND (
              survivor_product.canonical_product_key
                = merged_product.canonical_product_key
              OR (
                survivor_product.manufacturer_identifier_kind
                  = merged_product.manufacturer_identifier_kind
                AND survivor_product.canonical_identifier_key
                  = merged_product.canonical_identifier_key
              )
            )
           WHERE merged_product.avionics_manufacturer_identity_id = ?"#,
    );
    let approve_sql = db.sql(
        r#"UPDATE avionics_manufacturer_alias_candidates
           SET review_status = 'approved',
               decision_reason = 'Authoritative evidence confirms the semantic manufacturer alias.',
               decision_evidence_source_url = ?,
               decision_evidence_source_title = ?,
               decision_evidence_text = ?,
               reviewed_by_user_id = ?,
               reviewed_at = CURRENT_TIMESTAMP
           WHERE id = ? AND review_status = 'pending'"#,
    );
    let insert_membership_sql = db.sql(
        r#"INSERT INTO avionics_manufacturer_identity_memberships (
             avionics_manufacturer_id, avionics_manufacturer_identity_id,
             membership_basis, normalized_name_key, evidence_source_url,
             evidence_source_title, evidence_text, evidence_confidence
           ) VALUES (?, ?, 'authoritative_alias', ?, ?, ?, ?, 'very_high')"#,
    );
    let insert_merge_sql = db.sql(
        r#"INSERT INTO avionics_manufacturer_identity_merges (
             merged_identity_id, survivor_identity_id, alias_candidate_id,
             evidence_source_url, evidence_source_title, evidence_text,
             decided_by_user_id
           ) VALUES (?, ?, ?, ?, ?, ?, ?)"#,
    );
    let membership_sql = db.sql(membership_select_sql());
    let postgres_lock_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => None,
        DatabaseBackend::Postgres(_) => Some(db.sql(
            "LOCK TABLE avionics_manufacturer_identities, avionics_manufacturer_identity_memberships, avionics_manufacturer_identity_merges, avionics_manufacturer_alias_candidates, avionics_approved_product_identities IN SHARE ROW EXCLUSIVE MODE",
        )),
    };

    macro_rules! approve {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            if let Some(lock_sql) = &postgres_lock_sql {
                sqlx::query(lock_sql).execute(&mut *transaction).await?;
            }
            let candidate: CandidateRow = sqlx::query_as(&candidate_sql)
                .bind(request.candidate_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    ManufacturerIdentityError::Validation(format!(
                        "manufacturer alias candidate {} does not exist",
                        request.candidate_id
                    ))
                })?;
            if candidate.review_status != "pending" {
                return Err(ManufacturerIdentityError::Conflict(format!(
                    "manufacturer alias candidate {} is already {}",
                    candidate.id, candidate.review_status
                )));
            }
            // This is deliberately performed before resolving roots and
            // checking collisions. SQLite obtains its single-writer lock
            // here; PostgreSQL already holds explicit table locks above. Any
            // later failure rolls this one-way decision back with the rest of
            // the transaction.
            let approved = sqlx::query(&approve_sql)
                .bind(request.evidence.source_url.trim())
                .bind(request.evidence.source_title.trim())
                .bind(request.evidence.evidence_text.trim())
                .bind(request.reviewed_by_user_id)
                .bind(candidate.id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if approved != 1 {
                return Err(ManufacturerIdentityError::Conflict(
                    "manufacturer alias candidate changed during review".to_string(),
                ));
            }
            let existing_membership =
                sqlx::query_as::<_, ManufacturerIdentityMembership>(&current_membership_sql)
                    .bind(candidate.avionics_manufacturer_id)
                    .fetch_optional(&mut *transaction)
                    .await?;
            let target_identity_id: i64 = sqlx::query_scalar(&effective_identity_sql)
                .bind(candidate.candidate_manufacturer_identity_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    ManufacturerIdentityError::Conflict(format!(
                        "candidate manufacturer identity {} has no acyclic effective root",
                        candidate.candidate_manufacturer_identity_id
                    ))
                })?;
            let mut blocking_product_collision_count = 0_i64;
            let current_identity_id = if let Some(existing) = &existing_membership {
                let current_identity_id: i64 = sqlx::query_scalar(&effective_identity_sql)
                    .bind(existing.avionics_manufacturer_identity_id)
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or_else(|| {
                        ManufacturerIdentityError::Conflict(format!(
                            "manufacturer {} has no acyclic effective identity root",
                            candidate.avionics_manufacturer_id
                        ))
                    })?;
                if current_identity_id == target_identity_id {
                    return Err(ManufacturerIdentityError::Conflict(format!(
                        "manufacturer {} already resolves to identity {}",
                        candidate.avionics_manufacturer_id, target_identity_id
                    )));
                }
                blocking_product_collision_count = sqlx::query_scalar(&product_collision_sql)
                    .bind(target_identity_id)
                    .bind(current_identity_id)
                    .fetch_one(&mut *transaction)
                    .await?;
                Some(current_identity_id)
            } else {
                None
            };
            if blocking_product_collision_count != 0 {
                let membership = existing_membership.expect(
                    "a product collision can only exist for an assigned manufacturer identity",
                );
                transaction.commit().await?;
                return Ok(ManufacturerAliasApproval {
                    membership,
                    effective_manufacturer_identity_id: target_identity_id,
                    identity_merge_created: false,
                    blocking_product_collision_count,
                });
            }
            let normalized_name_key: String = sqlx::query_scalar(&manufacturer_key_sql)
                .bind(candidate.avionics_manufacturer_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    ManufacturerIdentityError::Validation(format!(
                        "manufacturer {} does not exist",
                        candidate.avionics_manufacturer_id
                    ))
                })?;
            let identity_merge_created = if let Some(current_identity_id) = current_identity_id {
                sqlx::query(&insert_merge_sql)
                    .bind(current_identity_id)
                    .bind(target_identity_id)
                    .bind(candidate.id)
                    .bind(request.evidence.source_url.trim())
                    .bind(request.evidence.source_title.trim())
                    .bind(request.evidence.evidence_text.trim())
                    .bind(request.reviewed_by_user_id)
                    .execute(&mut *transaction)
                    .await?;
                true
            } else {
                sqlx::query(&insert_membership_sql)
                    .bind(candidate.avionics_manufacturer_id)
                    .bind(target_identity_id)
                    .bind(normalized_name_key)
                    .bind(request.evidence.source_url.trim())
                    .bind(request.evidence.source_title.trim())
                    .bind(request.evidence.evidence_text.trim())
                    .execute(&mut *transaction)
                    .await?;
                false
            };
            let membership = sqlx::query_as::<_, ManufacturerIdentityMembership>(&membership_sql)
                .bind(candidate.avionics_manufacturer_id)
                .fetch_one(&mut *transaction)
                .await?;
            transaction.commit().await?;
            Ok::<_, ManufacturerIdentityError>(ManufacturerAliasApproval {
                membership,
                effective_manufacturer_identity_id: target_identity_id,
                identity_merge_created,
                blocking_product_collision_count: 0,
            })
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => approve!(pool),
        DatabaseBackend::Postgres(pool) => approve!(pool),
    }
}

/// Complete a previously human-approved identity redirect after exact product
/// collisions have been explicitly adjudicated. The original approval
/// evidence is reused verbatim; this function cannot invent a second decision
/// or bypass the no-collision precondition.
pub async fn finalize_approved_manufacturer_identity_merge(
    db: &AppDb,
    candidate_id: i64,
) -> ManufacturerIdentityResult<ManufacturerAliasApproval> {
    if candidate_id <= 0 {
        return Err(ManufacturerIdentityError::Validation(
            "candidate_id must be positive".to_string(),
        ));
    }
    let candidate_sql = db.sql(
        r#"SELECT id, avionics_manufacturer_id,
                  candidate_manufacturer_identity_id,
                  decision_evidence_source_url,
                  decision_evidence_source_title,
                  decision_evidence_text, reviewed_by_user_id
           FROM avionics_manufacturer_alias_candidates
           WHERE id = ? AND review_status = 'approved'"#,
    );
    let membership_sql = db.sql(membership_select_sql());
    let effective_identity_sql = db.sql(
        r#"SELECT avionics_manufacturer_identity_id
           FROM avionics_manufacturer_effective_identities
           WHERE identity_id = ?"#,
    );
    let product_collision_sql = db.sql(
        r#"SELECT COUNT(*)
           FROM avionics_approved_product_graph_identities merged_product
           JOIN avionics_approved_product_graph_identities survivor_product
             ON survivor_product.avionics_manufacturer_identity_id = ?
            AND (
              survivor_product.canonical_product_key
                = merged_product.canonical_product_key
              OR (
                survivor_product.manufacturer_identifier_kind
                  = merged_product.manufacturer_identifier_kind
                AND survivor_product.canonical_identifier_key
                  = merged_product.canonical_identifier_key
              )
            )
           WHERE merged_product.avionics_manufacturer_identity_id = ?"#,
    );
    let insert_merge_sql = db.sql(
        r#"INSERT INTO avionics_manufacturer_identity_merges (
             merged_identity_id, survivor_identity_id, alias_candidate_id,
             evidence_source_url, evidence_source_title, evidence_text,
             decided_by_user_id
           ) VALUES (?, ?, ?, ?, ?, ?, ?)"#,
    );
    let postgres_lock_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => None,
        DatabaseBackend::Postgres(_) => Some(db.sql(
            "LOCK TABLE avionics_manufacturer_identities, avionics_manufacturer_identity_memberships, avionics_manufacturer_identity_merges, avionics_manufacturer_alias_candidates, avionics_approved_product_identities IN SHARE ROW EXCLUSIVE MODE",
        )),
    };

    macro_rules! finalize {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            if let Some(lock_sql) = &postgres_lock_sql {
                sqlx::query(lock_sql).execute(&mut *transaction).await?;
            }
            let candidate: ApprovedMergeCandidateRow =
                sqlx::query_as(&candidate_sql)
                    .bind(candidate_id)
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or_else(|| {
                        ManufacturerIdentityError::Conflict(format!(
                            "manufacturer alias candidate {candidate_id} is missing or not approved"
                        ))
                    })?;
            // SQLite obtains its writer serialization before root/collision
            // resolution without mutating immutable evidence rows.
            if matches!(db.backend(), DatabaseBackend::Sqlite(_)) {
                let sqlite_lock_sql = db.sql(
                    "INSERT INTO avionics_manufacturer_identity_merges (merged_identity_id, survivor_identity_id, alias_candidate_id, evidence_source_url, evidence_source_title, evidence_text, decided_by_user_id) SELECT 0, 0, 0, '', '', '', 0 WHERE 0",
                );
                sqlx::query(&sqlite_lock_sql)
                    .execute(&mut *transaction)
                    .await?;
            }
            let membership =
                sqlx::query_as::<_, ManufacturerIdentityMembership>(&membership_sql)
                    .bind(candidate.avionics_manufacturer_id)
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or_else(|| {
                        ManufacturerIdentityError::Conflict(
                            "approved alias no longer has an assigned identity to redirect"
                                .to_string(),
                        )
                    })?;
            let merged_identity_id: i64 = sqlx::query_scalar(&effective_identity_sql)
                .bind(membership.avionics_manufacturer_identity_id)
                .fetch_one(&mut *transaction)
                .await?;
            let survivor_identity_id: i64 = sqlx::query_scalar(&effective_identity_sql)
                .bind(candidate.candidate_manufacturer_identity_id)
                .fetch_one(&mut *transaction)
                .await?;
            if merged_identity_id == survivor_identity_id {
                return Err(ManufacturerIdentityError::Conflict(
                    "approved manufacturer alias already resolves to its target identity"
                        .to_string(),
                ));
            }
            let collision_count: i64 = sqlx::query_scalar(&product_collision_sql)
                .bind(survivor_identity_id)
                .bind(merged_identity_id)
                .fetch_one(&mut *transaction)
                .await?;
            if collision_count != 0 {
                return Err(ManufacturerIdentityError::Conflict(format!(
                    "manufacturer identity merge remains blocked by {collision_count} exact product collision(s)"
                )));
            }
            sqlx::query(&insert_merge_sql)
                .bind(merged_identity_id)
                .bind(survivor_identity_id)
                .bind(candidate.id)
                .bind(&candidate.decision_evidence_source_url)
                .bind(&candidate.decision_evidence_source_title)
                .bind(&candidate.decision_evidence_text)
                .bind(candidate.reviewed_by_user_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            Ok::<_, ManufacturerIdentityError>(ManufacturerAliasApproval {
                membership,
                effective_manufacturer_identity_id: survivor_identity_id,
                identity_merge_created: true,
                blocking_product_collision_count: 0,
            })
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => finalize!(pool),
        DatabaseBackend::Postgres(pool) => finalize!(pool),
    }
}

pub async fn reject_manufacturer_alias_candidate(
    db: &AppDb,
    request: &RejectManufacturerAliasCandidateRequest,
) -> ManufacturerIdentityResult<()> {
    if request.candidate_id <= 0
        || request.reviewed_by_user_id <= 0
        || request.reason.trim().is_empty()
    {
        return Err(ManufacturerIdentityError::Validation(
            "candidate_id and reviewed_by_user_id must be positive and rejection reason must be nonblank".to_string(),
        ));
    }
    let sql = db.sql(
        r#"UPDATE avionics_manufacturer_alias_candidates
           SET review_status = 'rejected', decision_reason = ?,
               reviewed_by_user_id = ?, reviewed_at = CURRENT_TIMESTAMP
           WHERE id = ? AND review_status = 'pending'"#,
    );
    let changed = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query(&sql)
            .bind(request.reason.trim())
            .bind(request.reviewed_by_user_id)
            .bind(request.candidate_id)
            .execute(pool)
            .await?
            .rows_affected(),
        DatabaseBackend::Postgres(pool) => sqlx::query(&sql)
            .bind(request.reason.trim())
            .bind(request.reviewed_by_user_id)
            .bind(request.candidate_id)
            .execute(pool)
            .await?
            .rows_affected(),
    };
    if changed != 1 {
        return Err(ManufacturerIdentityError::Conflict(format!(
            "manufacturer alias candidate {} is missing or no longer pending",
            request.candidate_id
        )));
    }
    Ok(())
}

pub async fn get_manufacturer_identity(
    db: &AppDb,
    identity_id: i64,
) -> ManufacturerIdentityResult<Option<ManufacturerIdentity>> {
    let sql = db.sql(identity_select_sql());
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as(&sql)
            .bind(identity_id)
            .fetch_optional(pool)
            .await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_as(&sql)
            .bind(identity_id)
            .fetch_optional(pool)
            .await?),
    }
}

/// Test-fixture helper for modules that exercise downstream approved-product
/// behavior without going through Gemini catalog curation.
#[cfg(test)]
pub(crate) async fn ensure_test_manufacturer_identity(
    db: &AppDb,
    avionics_manufacturer_id: i64,
) -> ManufacturerIdentityResult<ManufacturerIdentityMembership> {
    ensure_manufacturer_identity(
        db,
        avionics_manufacturer_id,
        &ManufacturerIdentityEvidence {
            source_url: "https://manufacturer.example/test-fixture".to_string(),
            source_title: "Manufacturer test fixture".to_string(),
            evidence_text:
                "Authoritative test-fixture evidence establishes this manufacturer identity."
                    .to_string(),
        },
    )
    .await
}

#[cfg(test)]
pub(crate) async fn ensure_test_manufacturer_identity_for_model(
    db: &AppDb,
    avionics_model_id: i64,
) -> ManufacturerIdentityResult<ManufacturerIdentityMembership> {
    let sql = db.sql("SELECT avionics_manufacturer_id FROM avionics_models WHERE id = ?");
    let manufacturer_id: i64 = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar(&sql)
                .bind(avionics_model_id)
                .fetch_one(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar(&sql)
                .bind(avionics_model_id)
                .fetch_one(pool)
                .await?
        }
    };
    ensure_test_manufacturer_identity(db, manufacturer_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(db: &AppDb) -> &sqlx::SqlitePool {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("manufacturer identity tests require SQLite");
        };
        pool
    }

    async fn insert_manufacturer(db: &AppDb, name: &str, normalized_name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES (?, ?) RETURNING id",
        )
        .bind(name)
        .bind(normalized_name)
        .fetch_one(pool(db))
        .await
        .unwrap()
    }

    async fn identity(db: &AppDb, manufacturer_id: i64) -> ManufacturerIdentityMembership {
        ensure_manufacturer_identity(
            db,
            manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: "https://manufacturer.example/identity".to_string(),
                source_title: "Official manufacturer identity".to_string(),
                evidence_text:
                    "The official manufacturer source identifies this avionics manufacturer."
                        .to_string(),
            },
        )
        .await
        .unwrap()
    }

    async fn reviewer(db: &AppDb) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (email, display_name, auth_subject) VALUES ('reviewer@example.test', 'Reviewer', 'manufacturer-reviewer') RETURNING id",
        )
        .fetch_one(pool(db))
        .await
        .unwrap()
    }

    async fn stage_alias(
        db: &AppDb,
        manufacturer_id: i64,
        target_identity_id: i64,
    ) -> ManufacturerAliasCandidate {
        stage_manufacturer_alias_candidate(
            db,
            &StageManufacturerAliasCandidateRequest {
                avionics_manufacturer_id: manufacturer_id,
                candidate_manufacturer_identity_id: target_identity_id,
                candidate_basis: AliasCandidateBasis::GroundedAlias,
                matched_avionics_model_id: None,
                reason: "Official sources identify these manufacturer names as aliases."
                    .to_string(),
                evidence: Some(ManufacturerIdentityEvidence {
                    source_url: "https://manufacturer.example/history".to_string(),
                    source_title: "Official manufacturer history".to_string(),
                    evidence_text:
                        "The official manufacturer history connects both product brand names."
                            .to_string(),
                }),
                confidence: "very_high".to_string(),
            },
        )
        .await
        .unwrap()
    }

    async fn approve_alias(
        db: &AppDb,
        candidate_id: i64,
        reviewer_id: i64,
    ) -> ManufacturerAliasApproval {
        approve_manufacturer_alias_candidate(
            db,
            &ApproveManufacturerAliasCandidateRequest {
                candidate_id,
                evidence: ManufacturerIdentityEvidence {
                    source_url: "https://manufacturer.example/history".to_string(),
                    source_title: "Official manufacturer history".to_string(),
                    evidence_text:
                        "The official manufacturer history confirms both names are the same maker."
                            .to_string(),
                },
                reviewed_by_user_id: reviewer_id,
            },
        )
        .await
        .unwrap()
    }

    async fn approve_product(
        db: &AppDb,
        manufacturer_id: i64,
        model: &str,
        identifier: &str,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Test capability', 'test capability') ON CONFLICT DO NOTHING",
        )
        .execute(pool(db))
        .await
        .unwrap();
        let model_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO avionics_models (
                 avionics_manufacturer_id, name, normalized_name,
                 manufacturer_identifier_kind, manufacturer_identifier,
                 normalized_manufacturer_identifier, identity_source_url,
                 identity_source_title, identity_evidence_text,
                 identity_evidence_kind, identity_confidence,
                 catalog_reviewed_at
               ) VALUES (
                 ?, ?, ?, 'manufacturer_model_number', ?, ?,
                 'https://manufacturer.example/product',
                 'Official product data sheet',
                 'The official data sheet identifies this exact catalog product.',
                 'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
               ) RETURNING id"#,
        )
        .bind(manufacturer_id)
        .bind(model)
        .bind(model.to_ascii_lowercase())
        .bind(identifier)
        .bind(identifier.to_ascii_lowercase())
        .fetch_one(pool(db))
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) SELECT ?, id FROM avionics_types WHERE normalized_name='test capability'",
        )
        .bind(model_id)
        .execute(pool(db))
        .await
        .unwrap();
        sqlx::query("UPDATE avionics_models SET catalog_status='approved' WHERE id=?")
            .bind(model_id)
            .execute(pool(db))
            .await
            .unwrap();
        model_id
    }

    async fn approve_source_origin(
        db: &AppDb,
        identity_id: i64,
        reviewer_id: i64,
        origin: &str,
    ) -> i64 {
        sqlx::query_scalar(
            r#"INSERT INTO avionics_authoritative_source_origins (
                 authority_kind, avionics_manufacturer_identity_id,
                 regulator_key, https_origin, evidence_source_url,
                 evidence_source_title, evidence_text, approval_basis,
                 approved_by_user_id, approval_reason
               ) VALUES (
                 'manufacturer_primary', ?, NULL, ?, ? || '/products',
                 'Official manufacturer product catalog',
                 'The official manufacturer catalog identifies its avionics products.',
                 'human_review', ?,
                 'Reviewer confirmed this exact first-party manufacturer origin.'
               ) RETURNING id"#,
        )
        .bind(identity_id)
        .bind(origin)
        .bind(origin)
        .bind(reviewer_id)
        .fetch_one(pool(db))
        .await
        .unwrap()
    }

    #[test]
    fn canonical_source_origin_is_exact_and_rejects_unsafe_authorities() {
        assert_eq!(
            canonical_exact_https_origin(
                "https://STATIC.GARMIN.COM/pumac/manual.pdf?download=1#page=2"
            )
            .unwrap(),
            "https://static.garmin.com"
        );
        for rejected in [
            "http://static.garmin.com/manual.pdf",
            "https://user@static.garmin.com/manual.pdf",
            "https://static.garmin.com:8443/manual.pdf",
            "https://127.0.0.1/manual.pdf",
            "https://localhost/manual.pdf",
        ] {
            assert!(
                canonical_exact_https_origin(rejected).is_err(),
                "{rejected:?} must not become an authoritative origin"
            );
        }
    }

    #[tokio::test]
    async fn approved_alias_inherits_exact_origin_and_revocation_fails_closed() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let reviewer_id = reviewer(&db).await;
        let old_maker = insert_manufacturer(&db, "Old Avionics", "oldavionics").await;
        let current_maker = insert_manufacturer(&db, "Current Avionics", "currentavionics").await;
        let _old_identity = identity(&db, old_maker).await;
        let current_identity = identity(&db, current_maker).await;
        let origin_id = approve_source_origin(
            &db,
            current_identity.avionics_manufacturer_identity_id,
            reviewer_id,
            "https://current.example",
        )
        .await;
        let alias_candidate = stage_alias(
            &db,
            old_maker,
            current_identity.avionics_manufacturer_identity_id,
        )
        .await;
        approve_alias(&db, alias_candidate.id, reviewer_id).await;

        let admission = authorize_manufacturer_source_urls(
            &db,
            "Old Avionics",
            &["https://current.example/products/unit-1".to_string()],
        )
        .await
        .unwrap();
        assert_eq!(
            admission.effective_manufacturer_identity_id,
            current_identity.avionics_manufacturer_identity_id
        );
        assert_eq!(
            admission.canonical_origins,
            vec!["https://current.example".to_string()]
        );
        assert_eq!(
            admission
                .require_authorized_final_url(
                    "https://current.example/products/unit-1",
                    "https://current.example/products/unit-1.pdf",
                )
                .unwrap(),
            "https://current.example"
        );
        assert!(admission
            .require_authorized_final_url(
                "https://current.example/products/unit-1",
                "https://cdn.current.example/products/unit-1.pdf",
            )
            .is_err());

        for unapproved in [
            "https://cdn.current.example/products/unit-1",
            "https://example/products/unit-1",
            "https://current.example.invalid/products/unit-1",
        ] {
            assert!(
                authorize_manufacturer_source_urls(&db, "Old Avionics", &[unapproved.to_string()],)
                    .await
                    .is_err(),
                "{unapproved:?} must not inherit authority from another exact origin"
            );
        }

        sqlx::query(
            r#"INSERT INTO avionics_authoritative_source_origin_revocations (
                 avionics_authoritative_source_origin_id,
                 revoked_by_user_id, reason
               ) VALUES (?, ?, ?)"#,
        )
        .bind(origin_id)
        .bind(reviewer_id)
        .bind("Manufacturer ownership or source integrity can no longer be trusted.")
        .execute(pool(&db))
        .await
        .unwrap();

        assert!(authorize_manufacturer_source_urls(
            &db,
            "Current Avionics",
            &["https://current.example/products/unit-1".to_string()],
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("does not authorize exact source origin"));
        assert!(sqlx::query(
            "DELETE FROM avionics_authoritative_source_origin_revocations WHERE avionics_authoritative_source_origin_id=?",
        )
        .bind(origin_id)
        .execute(pool(&db))
        .await
        .unwrap_err()
        .to_string()
            .contains("permanent audit records"));
    }

    #[tokio::test]
    async fn product_admission_rechecks_revocation_inside_the_write_transaction() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let reviewer_id = reviewer(&db).await;
        let maker = insert_manufacturer(&db, "Transaction Avionics", "transactionavionics").await;
        let membership = identity(&db, maker).await;
        approve_source_origin(
            &db,
            membership.avionics_manufacturer_identity_id,
            reviewer_id,
            "https://transaction.example",
        )
        .await;
        let origin_id = approve_source_origin(
            &db,
            membership.avionics_manufacturer_identity_id,
            reviewer_id,
            "https://collision.transaction.example",
        )
        .await;
        sqlx::query(
            r#"INSERT INTO avionics_authoritative_source_origin_revocations (
                 avionics_authoritative_source_origin_id,
                 revoked_by_user_id, reason
               ) VALUES (?, ?, ?)"#,
        )
        .bind(origin_id)
        .bind(reviewer_id)
        .bind("The source was revoked after grounding and before product persistence.")
        .execute(pool(&db))
        .await
        .unwrap();

        let additional_evidence_source_urls =
            vec!["https://collision.transaction.example/comparison/tx-100".to_string()];
        let request = ManufacturerProductAdmission {
            manufacturer: "Transaction Avionics",
            model: "TX 100",
            manufacturer_identifier_kind: "manufacturer_model_number",
            manufacturer_identifier: "TX 100",
            evidence: ManufacturerIdentityEvidence {
                source_url: "https://transaction.example/products/tx-100".to_string(),
                source_title: "Transaction Avionics TX 100".to_string(),
                evidence_text:
                    "The TX 100 product page identifies manufacturer model number TX 100."
                        .to_string(),
            },
            additional_evidence_source_urls: &additional_evidence_source_urls,
        };
        let mut transaction = pool(&db).begin().await.unwrap();
        let error = admit_manufacturer_product_scope_sqlite(&db, &mut transaction, &request)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("has been revoked"));
        transaction.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn exact_safe_spellings_attach_to_one_evidence_backed_identity() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let spaced = insert_manufacturer(&db, "Bendix King", "bendix king").await;
        let compact = insert_manufacturer(&db, "BendixKing", "bendixking").await;

        let first = identity(&db, spaced).await;
        let second: ManufacturerIdentityMembership = sqlx::query_as(membership_select_sql())
            .bind(compact)
            .fetch_one(pool(&db))
            .await
            .unwrap();

        assert_eq!(
            first.avionics_manufacturer_identity_id,
            second.avionics_manufacturer_identity_id
        );
        assert_eq!(second.membership_basis, "deterministic_exact");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM avionics_manufacturer_identities")
                .fetch_one(pool(&db))
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn approved_alias_redirect_preserves_original_memberships() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let reviewer_id = reviewer(&db).await;
        let old_maker = insert_manufacturer(&db, "Old Brand", "old brand").await;
        let current_maker = insert_manufacturer(&db, "Current Brand", "current brand").await;
        let old_identity = identity(&db, old_maker).await;
        let current_identity = identity(&db, current_maker).await;
        let candidate = stage_alias(
            &db,
            old_maker,
            current_identity.avionics_manufacturer_identity_id,
        )
        .await;

        let approval = approve_alias(&db, candidate.id, reviewer_id).await;

        assert!(approval.identity_merge_created);
        assert_eq!(approval.blocking_product_collision_count, 0);
        assert_eq!(
            approval.membership.avionics_manufacturer_identity_id,
            old_identity.avionics_manufacturer_identity_id
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT avionics_manufacturer_identity_id FROM avionics_manufacturer_effective_memberships WHERE avionics_manufacturer_id=?"
            )
            .bind(old_maker)
            .fetch_one(pool(&db))
            .await
            .unwrap(),
            current_identity.avionics_manufacturer_identity_id
        );
    }

    #[tokio::test]
    async fn product_collision_preserves_approval_then_finalize_requires_adjudication() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let reviewer_id = reviewer(&db).await;
        let old_maker = insert_manufacturer(&db, "L-3 Communications", "l 3 communications").await;
        let current_maker = insert_manufacturer(&db, "L3Harris", "l3harris").await;
        let _old_identity = identity(&db, old_maker).await;
        let current_identity = identity(&db, current_maker).await;
        let duplicate_product = approve_product(&db, old_maker, "WX-500", "WX-500-A").await;
        let _survivor_product = approve_product(&db, current_maker, "WX-500", "WX-500-B").await;
        let candidate = stage_alias(
            &db,
            old_maker,
            current_identity.avionics_manufacturer_identity_id,
        )
        .await;

        let approval = approve_alias(&db, candidate.id, reviewer_id).await;
        assert!(!approval.identity_merge_created);
        assert_eq!(approval.blocking_product_collision_count, 1);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT review_status FROM avionics_manufacturer_alias_candidates WHERE id=?"
            )
            .bind(candidate.id)
            .fetch_one(pool(&db))
            .await
            .unwrap(),
            "approved"
        );
        assert!(
            finalize_approved_manufacturer_identity_merge(&db, candidate.id)
                .await
                .unwrap_err()
                .to_string()
                .contains("remains blocked")
        );

        let demotion =
            sqlx::query("UPDATE avionics_models SET catalog_status='unreviewed' WHERE id=?")
                .bind(duplicate_product)
                .execute(pool(&db))
                .await
                .unwrap_err();
        assert!(demotion
            .to_string()
            .contains("approved avionics product cannot be demoted"));
        assert!(
            finalize_approved_manufacturer_identity_merge(&db, candidate.id)
                .await
                .unwrap_err()
                .to_string()
                .contains("remains blocked")
        );
    }

    #[tokio::test]
    async fn bounded_chain_resolves_to_latest_root_and_rejects_cycle() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let reviewer_id = reviewer(&db).await;
        let first = insert_manufacturer(&db, "First Brand", "first brand").await;
        let second = insert_manufacturer(&db, "Second Brand", "second brand").await;
        let third = insert_manufacturer(&db, "Third Brand", "third brand").await;
        let _first_identity = identity(&db, first).await;
        let second_identity = identity(&db, second).await;
        let third_identity = identity(&db, third).await;
        let first_alias = stage_alias(
            &db,
            first,
            second_identity.avionics_manufacturer_identity_id,
        )
        .await;
        approve_alias(&db, first_alias.id, reviewer_id).await;
        let second_alias = stage_alias(
            &db,
            second,
            third_identity.avionics_manufacturer_identity_id,
        )
        .await;

        let second_approval = approve_alias(&db, second_alias.id, reviewer_id).await;
        assert!(second_approval.identity_merge_created);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT avionics_manufacturer_identity_id FROM avionics_manufacturer_effective_memberships WHERE avionics_manufacturer_id=?"
            )
            .bind(first)
            .fetch_one(pool(&db))
            .await
            .unwrap(),
            third_identity.avionics_manufacturer_identity_id
        );

        let cycle_candidate = stage_alias(
            &db,
            third,
            second_identity.avionics_manufacturer_identity_id,
        )
        .await;
        let error = approve_manufacturer_alias_candidate(
            &db,
            &ApproveManufacturerAliasCandidateRequest {
                candidate_id: cycle_candidate.id,
                evidence: ManufacturerIdentityEvidence {
                    source_url: "https://manufacturer.example/history".to_string(),
                    source_title: "Official manufacturer history".to_string(),
                    evidence_text:
                        "The official manufacturer history confirms the requested alias."
                            .to_string(),
                },
                reviewed_by_user_id: reviewer_id,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("already resolves"));
    }

    #[tokio::test]
    async fn merge_chain_cannot_exceed_thirty_two_edges() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let reviewer_id = reviewer(&db).await;
        let mut makers = Vec::new();
        let mut identities = Vec::new();
        for index in 0..34 {
            let name = format!("Chain Brand {index}");
            let normalized = format!("chain brand {index}");
            let maker = insert_manufacturer(&db, &name, &normalized).await;
            makers.push(maker);
            identities.push(identity(&db, maker).await);
        }
        for index in 0..32 {
            let candidate = stage_alias(
                &db,
                makers[index],
                identities[index + 1].avionics_manufacturer_identity_id,
            )
            .await;
            let approval = approve_alias(&db, candidate.id, reviewer_id).await;
            assert!(approval.identity_merge_created);
        }
        let overflow = stage_alias(
            &db,
            makers[32],
            identities[33].avionics_manufacturer_identity_id,
        )
        .await;
        let error = approve_manufacturer_alias_candidate(
            &db,
            &ApproveManufacturerAliasCandidateRequest {
                candidate_id: overflow.id,
                evidence: ManufacturerIdentityEvidence {
                    source_url: "https://manufacturer.example/history".to_string(),
                    source_title: "Official manufacturer history".to_string(),
                    evidence_text:
                        "The official manufacturer history confirms the requested alias."
                            .to_string(),
                },
                reviewed_by_user_id: reviewer_id,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("manufacturer identity merge"));
    }
}
