//! Literal aircraft observations retained separately from canonical identity.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::html::clean::{clean_publisher_source_html, normalize_source_evidence_span};

const MAX_SOURCE_EXCERPT: usize = 2_000;
const MAX_IDENTITY_EVIDENCE_TOKENS: usize = 32;
const MAX_COMPONENT_MATCHES: usize = 128;
const MAX_IDENTITY_EVIDENCE_SEARCH_STEPS: usize = 4_096;
const OBSERVATION_SCHEMA_VERSION: u8 = 2;
const IDENTITY_EVIDENCE_RESOLVER_VERSION: &str = "publisher_identity_evidence_v2";
const LITERAL_IDENTITY_SPAN_RESOLVER: &str = "literal_identity_token_span_v2";
const BASE_MODEL_VARIANT_SPAN_RESOLVER: &str = "base_model_variant_token_span_v2";
const COMPOSITE_FAMILY_SPAN_RESOLVER: &str = "composite_family_token_span_v2";
const MISSING_IDENTITY_SPAN_RESOLVER: &str = "missing_identity_token_span_v2";
const WORK_LIMITED_IDENTITY_SPAN_RESOLVER: &str = "work_limited_identity_token_span_v2";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AircraftIdentityObservation {
    pub listing_id: i64,
    pub submission_id: Option<i64>,
    pub source_url: Option<String>,
    pub rendered_html_sha256: Option<String>,
    pub manufacturer: String,
    pub model: String,
    pub variant: String,
    pub model_year: i64,
    pub serial_number: Option<String>,
    pub registration_number: Option<String>,
    pub source_excerpt: Option<String>,
    pub source_excerpt_is_exact: bool,
    pub source_kind: String,
    pub observation_sha256: String,
    pub cluster_key: String,
    pub requires_human_review: bool,
    pub review_reasons: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct AircraftObservationLoadReport {
    pub observations: Vec<AircraftIdentityObservation>,
    pub unique_clusters: usize,
    pub retained_html_count: usize,
    pub fallback_count: usize,
    pub human_review_count: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct AircraftObservationStageReport {
    pub eligible: usize,
    pub inserted: usize,
    pub reattached: usize,
    pub already_present: usize,
    pub skipped: usize,
    pub skipped_listing_ids: Vec<i64>,
}

#[derive(Debug)]
pub enum AircraftObservationError {
    InvalidRequest(String),
    Database(String),
}

impl fmt::Display for AircraftObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) | Self::Database(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl std::error::Error for AircraftObservationError {}

impl From<sqlx::Error> for AircraftObservationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

#[derive(Debug, FromRow)]
struct ObservationSourceRow {
    listing_id: i64,
    listing_source_url: Option<String>,
    stored_manufacturer: String,
    stored_model: String,
    stored_variant: String,
    stored_model_year: i64,
    stored_serial_number: Option<String>,
    stored_registration_number: Option<String>,
    submission_id: Option<i64>,
    submission_source_url: Option<String>,
    rendered_html_sha256: Option<String>,
    rendered_html: Option<String>,
    extracted_listing_json: Option<String>,
}

#[derive(Debug, FromRow)]
struct StoredObservationRow {
    id: i64,
    aircraft_sale_listing_id: Option<i64>,
    source_url: Option<String>,
    observed_make: Option<String>,
    observed_family: Option<String>,
    observed_designation: Option<String>,
    observed_generation: Option<String>,
    observed_package: Option<String>,
    model_year: Option<i64>,
    serial_number: Option<String>,
    registration_number: Option<String>,
    market_code: Option<String>,
    exact_source_evidence: String,
    observation_sha256: String,
    legacy_hint_json: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct LiteralAircraftFields {
    manufacturer: Option<String>,
    model: Option<String>,
    variant: Option<String>,
    model_year: Option<i64>,
    serial_number: Option<String>,
    registration_number: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdentityEvidenceStatus {
    Exact,
    Missing,
    WorkLimitExceeded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IdentityEvidenceResolution {
    excerpt: Option<String>,
    status: IdentityEvidenceStatus,
    resolver: &'static str,
}

impl IdentityEvidenceResolution {
    fn is_exact(&self) -> bool {
        self.status == IdentityEvidenceStatus::Exact
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceToken {
    normalized: String,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TokenRange {
    start: usize,
    end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum UniqueIdentitySpan {
    Unique(String),
    Missing,
    WorkLimitExceeded,
}

/// Load literal aircraft identity observations from retained submissions.
///
/// Previously generated catalog labels are fallback hints only. When a retained
/// extraction exists, its literal hierarchy fields win. Registration and serial
/// are different: the current listing values are the admission identifiers used
/// by the FAA gate, so retained model output can never replace them. The function
/// never writes to the catalog and never treats normalization as an identity
/// decision.
pub async fn load_aircraft_identity_observations(
    db: &AppDb,
    limit: i64,
    listing_id: Option<i64>,
) -> Result<AircraftObservationLoadReport, AircraftObservationError> {
    if limit < 1 {
        return Err(AircraftObservationError::InvalidRequest(
            "limit must be at least 1".to_string(),
        ));
    }
    if listing_id.is_some_and(|id| id < 1) {
        return Err(AircraftObservationError::InvalidRequest(
            "listing_id must be a positive integer".to_string(),
        ));
    }

    let rows = load_rows(db, limit, listing_id).await?;
    if let Some(listing_id) = listing_id {
        if rows.is_empty() {
            return Err(AircraftObservationError::InvalidRequest(format!(
                "listing {listing_id} was not found"
            )));
        }
    }

    let observations = rows.iter().map(observation_from_row).collect::<Vec<_>>();
    let unique_clusters = observations
        .iter()
        .map(|observation| observation.cluster_key.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let retained_html_count = observations
        .iter()
        .filter(|observation| observation.source_kind == "retained_submission")
        .count();
    let fallback_count = observations.len().saturating_sub(retained_html_count);
    let human_review_count = observations
        .iter()
        .filter(|observation| observation.requires_human_review)
        .count();

    Ok(AircraftObservationLoadReport {
        observations,
        unique_clusters,
        retained_html_count,
        fallback_count,
        human_review_count,
    })
}

/// Persist only observations whose immutable extracted identity can be bound
/// to one exact retained publisher span by the versioned evidence resolver.
/// This is a staging write, not a catalog approval or merge.
pub async fn stage_aircraft_identity_observations(
    db: &AppDb,
    observations: &[AircraftIdentityObservation],
) -> Result<AircraftObservationStageReport, AircraftObservationError> {
    let mut report = AircraftObservationStageReport::default();
    for observation in observations {
        let Some(exact_source_evidence) = observation
            .source_excerpt
            .as_deref()
            .filter(|_| observation.source_excerpt_is_exact)
        else {
            report.skipped += 1;
            report.skipped_listing_ids.push(observation.listing_id);
            continue;
        };
        report.eligible += 1;

        let legacy_hint_json = serde_json::to_string(&serde_json::json!({
            "source_kind": observation.source_kind,
            "submission_id": observation.submission_id,
            "rendered_html_sha256": observation.rendered_html_sha256,
            "cluster_key": observation.cluster_key,
            "requires_human_review": observation.requires_human_review,
            "review_reasons": observation.review_reasons,
            "literal_fields": {
                "manufacturer": observation.manufacturer,
                "model": observation.model,
                "variant": observation.variant,
                "model_year": observation.model_year,
                "serial_number": observation.serial_number,
                "registration_number": observation.registration_number,
            }
        }))
        .expect("observation staging payload serializes");
        let sql = db.sql(
            r#"
            INSERT INTO aircraft_identity_observations (
              aircraft_sale_listing_id,
              source_url,
              observed_make,
              observed_family,
              observed_designation,
              observed_generation,
              observed_package,
              model_year,
              serial_number,
              registration_number,
              market_code,
              exact_source_evidence,
              observation_sha256,
              legacy_hint_json
            ) VALUES (?, ?, ?, ?, ?, NULL, NULL, ?, ?, ?, NULL, ?, ?, ?)
            ON CONFLICT (observation_sha256) DO NOTHING
            "#,
        );
        let affected = match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query(&sql)
                .bind(observation.listing_id)
                .bind(observation.source_url.as_deref())
                .bind(&observation.manufacturer)
                .bind(&observation.model)
                .bind(&observation.variant)
                .bind(observation.model_year)
                .bind(observation.serial_number.as_deref())
                .bind(observation.registration_number.as_deref())
                .bind(exact_source_evidence)
                .bind(&observation.observation_sha256)
                .bind(&legacy_hint_json)
                .execute(pool)
                .await?
                .rows_affected(),
            DatabaseBackend::Postgres(pool) => sqlx::query(&sql)
                .bind(observation.listing_id)
                .bind(observation.source_url.as_deref())
                .bind(&observation.manufacturer)
                .bind(&observation.model)
                .bind(&observation.variant)
                .bind(observation.model_year)
                .bind(observation.serial_number.as_deref())
                .bind(observation.registration_number.as_deref())
                .bind(exact_source_evidence)
                .bind(&observation.observation_sha256)
                .bind(&legacy_hint_json)
                .execute(pool)
                .await?
                .rows_affected(),
        };
        if affected == 0 {
            let existing = load_stored_observation(db, &observation.observation_sha256).await?;
            validate_stored_observation(
                &existing,
                observation,
                exact_source_evidence,
                &legacy_hint_json,
            )?;
            match existing.aircraft_sale_listing_id {
                Some(listing_id) if listing_id == observation.listing_id => {
                    report.already_present += 1;
                }
                None => {
                    let update = db.sql(
                        r#"
                        UPDATE aircraft_identity_observations
                        SET aircraft_sale_listing_id = ?
                        WHERE id = ?
                          AND aircraft_sale_listing_id IS NULL
                          AND observation_sha256 = ?
                        "#,
                    );
                    let changed = match db.backend() {
                        DatabaseBackend::Sqlite(pool) => sqlx::query(&update)
                            .bind(observation.listing_id)
                            .bind(existing.id)
                            .bind(&observation.observation_sha256)
                            .execute(pool)
                            .await?
                            .rows_affected(),
                        DatabaseBackend::Postgres(pool) => sqlx::query(&update)
                            .bind(observation.listing_id)
                            .bind(existing.id)
                            .bind(&observation.observation_sha256)
                            .execute(pool)
                            .await?
                            .rows_affected(),
                    };
                    if changed == 1 {
                        report.reattached += 1;
                    } else {
                        let current =
                            load_stored_observation(db, &observation.observation_sha256).await?;
                        if current.aircraft_sale_listing_id == Some(observation.listing_id) {
                            report.already_present += 1;
                        } else {
                            return Err(AircraftObservationError::InvalidRequest(format!(
                                "aircraft observation {} was concurrently attached to a different listing",
                                observation.observation_sha256
                            )));
                        }
                    }
                }
                Some(listing_id) => {
                    return Err(AircraftObservationError::InvalidRequest(format!(
                        "aircraft observation {} is already attached to listing {listing_id}, not replay listing {}",
                        observation.observation_sha256, observation.listing_id
                    )));
                }
            }
        } else {
            report.inserted += 1;
        }
    }
    Ok(report)
}

async fn load_stored_observation(
    db: &AppDb,
    observation_sha256: &str,
) -> Result<StoredObservationRow, AircraftObservationError> {
    let sql = db.sql(
        r#"
        SELECT id, aircraft_sale_listing_id, source_url, observed_make,
               observed_family, observed_designation, observed_generation,
               observed_package, model_year, serial_number,
               registration_number, market_code, exact_source_evidence,
               observation_sha256, legacy_hint_json
        FROM aircraft_identity_observations
        WHERE observation_sha256 = ?
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, StoredObservationRow>(&sql)
                .bind(observation_sha256)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, StoredObservationRow>(&sql)
                .bind(observation_sha256)
                .fetch_optional(pool)
                .await?
        }
    };
    row.ok_or_else(|| {
        AircraftObservationError::Database(
            "aircraft observation conflict disappeared before validation".to_string(),
        )
    })
}

fn validate_stored_observation(
    stored: &StoredObservationRow,
    expected: &AircraftIdentityObservation,
    exact_source_evidence: &str,
    legacy_hint_json: &str,
) -> Result<(), AircraftObservationError> {
    let exact_match = stored.source_url == expected.source_url
        && stored.observed_make.as_deref() == Some(expected.manufacturer.as_str())
        && stored.observed_family.as_deref() == Some(expected.model.as_str())
        && stored.observed_designation.as_deref() == Some(expected.variant.as_str())
        && stored.observed_generation.is_none()
        && stored.observed_package.is_none()
        && stored.model_year == Some(expected.model_year)
        && stored.serial_number == expected.serial_number
        && stored.registration_number == expected.registration_number
        && stored.market_code.is_none()
        && stored.exact_source_evidence == exact_source_evidence
        && stored.observation_sha256 == expected.observation_sha256
        && stored.legacy_hint_json.as_deref() == Some(legacy_hint_json);
    if exact_match {
        Ok(())
    } else {
        Err(AircraftObservationError::InvalidRequest(format!(
            "aircraft observation hash {} collides with different immutable observation material",
            expected.observation_sha256
        )))
    }
}

fn observation_from_row(row: &ObservationSourceRow) -> AircraftIdentityObservation {
    let extracted = row
        .extracted_listing_json
        .as_deref()
        .and_then(parse_literal_fields)
        .unwrap_or_default();
    let manufacturer = usable(extracted.manufacturer)
        .unwrap_or_else(|| row.stored_manufacturer.trim().to_string());
    let model = usable(extracted.model).unwrap_or_else(|| row.stored_model.trim().to_string());
    let variant =
        usable(extracted.variant).unwrap_or_else(|| row.stored_variant.trim().to_string());
    let model_year = extracted.model_year.unwrap_or(row.stored_model_year);
    let extracted_serial_number = usable(extracted.serial_number);
    let extracted_registration_number = usable(extracted.registration_number);
    let serial_number = usable(row.stored_serial_number.clone());
    let registration_number = usable(row.stored_registration_number.clone());
    let source_url = row
        .submission_source_url
        .clone()
        .or_else(|| row.listing_source_url.clone());

    let publisher_text = row
        .rendered_html
        .as_deref()
        .map(clean_publisher_source_html)
        .unwrap_or_default();
    let evidence = resolve_identity_evidence(&publisher_text, &manufacturer, &model, &variant);
    let source_excerpt = evidence.excerpt.clone();
    let source_excerpt_is_exact = evidence.is_exact();

    let mut review_reasons = Vec::new();
    if row.rendered_html.is_none() {
        review_reasons.push("retained rendered HTML is unavailable".to_string());
    }
    if row.extracted_listing_json.is_none() {
        review_reasons.push("retained literal extraction is unavailable".to_string());
    }
    if extracted_registration_number != registration_number {
        review_reasons.push(
            "retained extraction registration differs from the current listing; the current listing value is used for FAA admission"
                .to_string(),
        );
    }
    if extracted_serial_number != serial_number {
        review_reasons.push(
            "retained extraction serial differs from the current listing; the current listing value is used for FAA admission"
                .to_string(),
        );
    }
    match evidence.status {
        IdentityEvidenceStatus::Exact => {}
        IdentityEvidenceStatus::Missing => review_reasons.push(
            "identity labels were not found in one bounded publisher-authored source span"
                .to_string(),
        ),
        IdentityEvidenceStatus::WorkLimitExceeded => review_reasons.push(
            "publisher source repeated identity labels beyond the bounded evidence-search work limit"
                .to_string(),
        ),
    }
    if manufacturer.trim().is_empty() || model.trim().is_empty() || variant.trim().is_empty() {
        review_reasons.push("one or more literal hierarchy fields are empty".to_string());
    }

    let cluster_key = observation_cluster_key(&manufacturer, &model, &variant, model_year);
    let observation_sha256 = observation_fingerprint(
        row.listing_id,
        row.submission_id,
        row.rendered_html_sha256.as_deref(),
        &manufacturer,
        &model,
        &variant,
        model_year,
        serial_number.as_deref(),
        registration_number.as_deref(),
        source_excerpt
            .as_deref()
            .filter(|_| source_excerpt_is_exact),
        evidence.resolver,
    );

    AircraftIdentityObservation {
        listing_id: row.listing_id,
        submission_id: row.submission_id,
        source_url,
        rendered_html_sha256: row.rendered_html_sha256.clone(),
        manufacturer,
        model,
        variant,
        model_year,
        serial_number,
        registration_number,
        source_excerpt,
        source_excerpt_is_exact,
        source_kind: if row.rendered_html.is_some() {
            "retained_submission".to_string()
        } else {
            "stored_listing_fallback".to_string()
        },
        observation_sha256,
        cluster_key,
        requires_human_review: !review_reasons.is_empty(),
        review_reasons,
    }
}

/// Recompute the retained publisher proof and its versioned observation
/// fingerprint before an approved hierarchy is written.
///
/// The caller remains responsible for checking the selected submission,
/// source URL, rendered-HTML digest, literal extraction, and FAA admission.
/// This helper binds only the source-evidence decision to the exact resolver
/// used when the observation was loaded.
pub(crate) fn retained_source_identity_evidence_matches(
    rendered_html: &str,
    expected: &AircraftIdentityObservation,
) -> bool {
    let publisher_text = clean_publisher_source_html(rendered_html);
    let evidence = resolve_identity_evidence(
        &publisher_text,
        &expected.manufacturer,
        &expected.model,
        &expected.variant,
    );
    let Some(exact_source_evidence) = evidence
        .excerpt
        .as_deref()
        .filter(|_| evidence.is_exact() && expected.source_excerpt_is_exact)
    else {
        return false;
    };
    if expected.source_excerpt.as_deref() != Some(exact_source_evidence) {
        return false;
    }

    observation_fingerprint(
        expected.listing_id,
        expected.submission_id,
        expected.rendered_html_sha256.as_deref(),
        &expected.manufacturer,
        &expected.model,
        &expected.variant,
        expected.model_year,
        expected.serial_number.as_deref(),
        expected.registration_number.as_deref(),
        Some(exact_source_evidence),
        evidence.resolver,
    ) == expected.observation_sha256
}

async fn load_rows(
    db: &AppDb,
    limit: i64,
    listing_id: Option<i64>,
) -> Result<Vec<ObservationSourceRow>, sqlx::Error> {
    let predicate = if listing_id.is_some() {
        "WHERE listing.id = ?"
    } else {
        ""
    };
    let raw_sql = format!(
        r#"
        SELECT
          listing.id AS listing_id,
          listing.source_url AS listing_source_url,
          manufacturer.name AS stored_manufacturer,
          model.name AS stored_model,
          variant.name AS stored_variant,
          listing.model_year AS stored_model_year,
          listing.serial_number AS stored_serial_number,
          listing.registration_number AS stored_registration_number,
          submission.id AS submission_id,
          submission.source_url AS submission_source_url,
          submission.rendered_html_sha256,
          submission.rendered_html,
          submission.extracted_listing_json
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer
          ON manufacturer.id = model.aircraft_manufacturer_id
        LEFT JOIN plugin_submissions submission
          ON submission.id = (
            SELECT candidate.id
            FROM plugin_submissions candidate
            WHERE candidate.canonical_listing_id = listing.id
               OR (
                 candidate.canonical_listing_id IS NULL
                 AND listing.source_url IS NOT NULL
                 AND candidate.source_url = listing.source_url
               )
            ORDER BY
              CASE WHEN candidate.canonical_listing_id IS NOT NULL THEN 0 ELSE 1 END,
              candidate.submitted_at DESC,
              candidate.id DESC
            LIMIT 1
          )
        {predicate}
        ORDER BY listing.id
        LIMIT ?
        "#
    );
    let sql = db.sql(&raw_sql);

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let query = sqlx::query_as::<_, ObservationSourceRow>(&sql);
            let query = if let Some(listing_id) = listing_id {
                query.bind(listing_id)
            } else {
                query
            };
            query.bind(limit).fetch_all(pool).await
        }
        DatabaseBackend::Postgres(pool) => {
            let query = sqlx::query_as::<_, ObservationSourceRow>(&sql);
            let query = if let Some(listing_id) = listing_id {
                query.bind(listing_id)
            } else {
                query
            };
            query.bind(limit).fetch_all(pool).await
        }
    }
}

fn parse_literal_fields(value: &str) -> Option<LiteralAircraftFields> {
    let value = serde_json::from_str::<Value>(value).ok()?;
    serde_json::from_value(value).ok()
}

fn usable(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn observation_cluster_key(
    manufacturer: &str,
    model: &str,
    variant: &str,
    model_year: i64,
) -> String {
    [manufacturer, model, variant]
        .into_iter()
        .map(retrieval_key)
        .chain(std::iter::once(model_year.to_string()))
        .collect::<Vec<_>>()
        .join(":")
}

fn retrieval_key(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[allow(clippy::too_many_arguments)]
fn observation_fingerprint(
    listing_id: i64,
    submission_id: Option<i64>,
    rendered_html_sha256: Option<&str>,
    manufacturer: &str,
    model: &str,
    variant: &str,
    model_year: i64,
    serial_number: Option<&str>,
    registration_number: Option<&str>,
    exact_source_evidence: Option<&str>,
    evidence_resolver: &str,
) -> String {
    let material = serde_json::json!({
        "listing_id": listing_id,
        "submission_id": submission_id,
        "rendered_html_sha256": rendered_html_sha256,
        "manufacturer": manufacturer,
        "model": model,
        "variant": variant,
        "model_year": model_year,
        "serial_number": serial_number,
        "registration_number": registration_number,
        "exact_source_evidence": exact_source_evidence,
        "identity_evidence_resolver": evidence_resolver,
        "identity_evidence_resolver_version": IDENTITY_EVIDENCE_RESOLVER_VERSION,
        "observation_schema_version": OBSERVATION_SCHEMA_VERSION,
    });
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(&material).expect("observation material serializes"));
    format!("{:x}", hasher.finalize())
}

fn resolve_identity_evidence(
    publisher_text: &str,
    manufacturer: &str,
    model: &str,
    variant: &str,
) -> IdentityEvidenceResolution {
    if publisher_text.trim().is_empty() {
        return IdentityEvidenceResolution {
            excerpt: None,
            status: IdentityEvidenceStatus::Missing,
            resolver: MISSING_IDENTITY_SPAN_RESOLVER,
        };
    }

    match unique_identity_span(publisher_text, &[manufacturer, model, variant]) {
        UniqueIdentitySpan::Unique(excerpt) => {
            return IdentityEvidenceResolution {
                excerpt: Some(excerpt),
                status: IdentityEvidenceStatus::Exact,
                resolver: LITERAL_IDENTITY_SPAN_RESOLVER,
            };
        }
        UniqueIdentitySpan::Missing => {}
        UniqueIdentitySpan::WorkLimitExceeded => {
            return work_limited_identity_evidence(publisher_text);
        }
    }

    if base_model_variant_phrase_is_compatible(model, variant) {
        match unique_identity_span(publisher_text, &[manufacturer, variant]) {
            UniqueIdentitySpan::Unique(excerpt) => {
                return IdentityEvidenceResolution {
                    excerpt: Some(excerpt),
                    status: IdentityEvidenceStatus::Exact,
                    resolver: BASE_MODEL_VARIANT_SPAN_RESOLVER,
                };
            }
            UniqueIdentitySpan::Missing => {}
            UniqueIdentitySpan::WorkLimitExceeded => {
                return work_limited_identity_evidence(publisher_text);
            }
        }
    }

    let Some(family_name) = composite_family_name_component(manufacturer, model, variant) else {
        return unresolved_identity_evidence(publisher_text);
    };
    match unique_identity_span(
        publisher_text,
        &[manufacturer, variant, family_name.as_str()],
    ) {
        UniqueIdentitySpan::Unique(excerpt) => IdentityEvidenceResolution {
            excerpt: Some(excerpt),
            status: IdentityEvidenceStatus::Exact,
            resolver: COMPOSITE_FAMILY_SPAN_RESOLVER,
        },
        UniqueIdentitySpan::Missing => unresolved_identity_evidence(publisher_text),
        UniqueIdentitySpan::WorkLimitExceeded => work_limited_identity_evidence(publisher_text),
    }
}

fn unresolved_identity_evidence(publisher_text: &str) -> IdentityEvidenceResolution {
    IdentityEvidenceResolution {
        excerpt: (!publisher_text.trim().is_empty())
            .then(|| prefix_at_boundary(publisher_text, MAX_SOURCE_EXCERPT).to_string()),
        status: IdentityEvidenceStatus::Missing,
        resolver: MISSING_IDENTITY_SPAN_RESOLVER,
    }
}

fn work_limited_identity_evidence(publisher_text: &str) -> IdentityEvidenceResolution {
    IdentityEvidenceResolution {
        excerpt: (!publisher_text.trim().is_empty())
            .then(|| prefix_at_boundary(publisher_text, MAX_SOURCE_EXCERPT).to_string()),
        status: IdentityEvidenceStatus::WorkLimitExceeded,
        resolver: WORK_LIMITED_IDENTITY_SPAN_RESOLVER,
    }
}

/// Extract the safest bounded source span that contains every identity
/// component as complete alphanumeric tokens.
///
/// Repeated title, breadcrumb, heading, and detail copies corroborate the same
/// fixed extracted components; they are not an identity ambiguity. The
/// shortest valid token span is the strongest local proof, with publisher
/// occurrence order as a deterministic tie-breaker.
fn unique_identity_span(source: &str, components: &[&str]) -> UniqueIdentitySpan {
    let tokens = source_tokens(source);
    if tokens.is_empty() || components.is_empty() {
        return UniqueIdentitySpan::Missing;
    }

    let mut component_matches = Vec::with_capacity(components.len());
    for component in components {
        let phrase = normalized_tokens(component);
        if phrase.is_empty() {
            return UniqueIdentitySpan::Missing;
        }
        let matches = phrase_matches(&tokens, &phrase);
        if matches.is_empty() {
            return UniqueIdentitySpan::Missing;
        }
        if matches.len() > MAX_COMPONENT_MATCHES {
            return UniqueIdentitySpan::WorkLimitExceeded;
        }
        component_matches.push(matches);
    }

    component_matches.sort_by_key(Vec::len);
    let mut best = None;
    let mut search_steps = 0;
    let mut work_limit_exceeded = false;
    for anchor in &component_matches[0] {
        select_best_candidate_range(
            source,
            &tokens,
            &component_matches,
            1,
            *anchor,
            &mut best,
            &mut search_steps,
            &mut work_limit_exceeded,
        );
        if work_limit_exceeded {
            return UniqueIdentitySpan::WorkLimitExceeded;
        }
    }

    best.map(|range| {
        let start = tokens[range.start].start;
        let end = tokens[range.end - 1].end;
        UniqueIdentitySpan::Unique(source[start..end].to_string())
    })
    .unwrap_or(UniqueIdentitySpan::Missing)
}

fn select_best_candidate_range(
    source: &str,
    tokens: &[SourceToken],
    component_matches: &[Vec<TokenRange>],
    component_index: usize,
    current: TokenRange,
    best: &mut Option<TokenRange>,
    search_steps: &mut usize,
    work_limit_exceeded: &mut bool,
) {
    if *work_limit_exceeded {
        return;
    }
    if *search_steps >= MAX_IDENTITY_EVIDENCE_SEARCH_STEPS {
        *work_limit_exceeded = true;
        return;
    }
    *search_steps += 1;

    let current_width = current.end.saturating_sub(current.start);
    if current_width > MAX_IDENTITY_EVIDENCE_TOKENS
        || best.is_some_and(|best| current_width > best.end.saturating_sub(best.start))
    {
        return;
    }
    if component_index == component_matches.len() {
        let start = tokens[current.start].start;
        let end = tokens[current.end - 1].end;
        let excerpt = &source[start..end];
        if excerpt.is_empty() || crosses_hard_span_boundary(excerpt) {
            return;
        }
        let candidate_key = (current_width, start, end);
        let should_replace = best.is_none_or(|best| {
            let best_start = tokens[best.start].start;
            let best_end = tokens[best.end - 1].end;
            candidate_key < (best.end.saturating_sub(best.start), best_start, best_end)
        });
        if should_replace {
            *best = Some(current);
        }
        return;
    }

    for matched in &component_matches[component_index] {
        let combined = TokenRange {
            start: current.start.min(matched.start),
            end: current.end.max(matched.end),
        };
        select_best_candidate_range(
            source,
            tokens,
            component_matches,
            component_index + 1,
            combined,
            best,
            search_steps,
            work_limit_exceeded,
        );
        if *work_limit_exceeded {
            return;
        }
    }
}

fn source_tokens(source: &str) -> Vec<SourceToken> {
    let mut tokens = Vec::new();
    let mut token_start = None;
    for (index, character) in source.char_indices() {
        if character.is_alphanumeric() {
            token_start.get_or_insert(index);
        } else if let Some(start) = token_start.take() {
            tokens.push(SourceToken {
                normalized: normalize_source_evidence_span(&source[start..index]),
                start,
                end: index,
            });
        }
    }
    if let Some(start) = token_start {
        tokens.push(SourceToken {
            normalized: normalize_source_evidence_span(&source[start..]),
            start,
            end: source.len(),
        });
    }
    tokens
}

fn normalized_tokens(value: &str) -> Vec<String> {
    normalize_source_evidence_span(value)
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn phrase_matches(tokens: &[SourceToken], phrase: &[String]) -> Vec<TokenRange> {
    if phrase.len() > tokens.len() {
        return Vec::new();
    }
    tokens
        .windows(phrase.len())
        .enumerate()
        .filter_map(|(start, candidate)| {
            candidate
                .iter()
                .map(|token| token.normalized.as_str())
                .eq(phrase.iter().map(String::as_str))
                .then_some(TokenRange {
                    start,
                    end: start + phrase.len(),
                })
        })
        .collect()
}

fn base_model_variant_phrase_is_compatible(model: &str, variant: &str) -> bool {
    let model_tokens = normalized_tokens(model);
    let variant_tokens = normalized_tokens(variant);
    if model_tokens.len() != 1 || variant_tokens.is_empty() || model_tokens == variant_tokens {
        return false;
    }
    let digit_bearing_variant_tokens = variant_tokens
        .iter()
        .filter(|token| token.chars().any(|character| character.is_ascii_digit()))
        .collect::<Vec<_>>();
    digit_bearing_variant_tokens.len() == 1
        && designator_suffix_is_compatible(&model_tokens[0], digit_bearing_variant_tokens[0])
}

fn composite_family_name_component(
    manufacturer: &str,
    model: &str,
    variant: &str,
) -> Option<String> {
    let model_tokens = normalized_tokens(model);
    let variant_tokens = normalized_tokens(variant);
    if model_tokens.is_empty()
        || variant_tokens.is_empty()
        || model_tokens == variant_tokens
        || !composite_designator_is_compatible(&model_tokens, &variant_tokens)
    {
        return None;
    }

    let mut alphabetic_runs = Vec::<Vec<&str>>::new();
    let mut current = Vec::new();
    for token in &model_tokens {
        if token.chars().all(char::is_alphabetic) {
            current.push(token.as_str());
        } else if !current.is_empty() {
            alphabetic_runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        alphabetic_runs.push(current);
    }
    alphabetic_runs.retain(|run| run.iter().map(|token| token.len()).sum::<usize>() >= 3);
    if alphabetic_runs.len() != 1 {
        return None;
    }

    let family_tokens = &alphabetic_runs[0];
    let manufacturer_tokens = normalized_tokens(manufacturer);
    if words_contain_phrase(&manufacturer_tokens, family_tokens)
        || words_contain_phrase(&variant_tokens, family_tokens)
    {
        return None;
    }
    Some(family_tokens.join(" "))
}

/// Require one unambiguous digit-bearing model token to be either the exact
/// variant or its complete leading designator. A variant may add only a short
/// alphabetic suffix. This proves `182` -> `182T` without treating `182` as a
/// substring of `T182T`, accepting `172` -> `182T`, or guessing how multiple
/// numeric components relate.
fn composite_designator_is_compatible(model_tokens: &[String], variant_tokens: &[String]) -> bool {
    let digit_bearing_model_tokens = model_tokens
        .iter()
        .filter(|token| token.chars().any(|character| character.is_ascii_digit()))
        .collect::<Vec<_>>();
    if digit_bearing_model_tokens.len() != 1 || variant_tokens.len() != 1 {
        return false;
    }
    designator_suffix_is_compatible(digit_bearing_model_tokens[0], &variant_tokens[0])
}

fn designator_suffix_is_compatible(model_designator: &str, exact_variant: &str) -> bool {
    if !model_designator
        .chars()
        .any(|character| character.is_ascii_digit())
        || !exact_variant
            .chars()
            .any(|character| character.is_ascii_digit())
    {
        return false;
    }
    let Some(suffix) = exact_variant.strip_prefix(model_designator) else {
        return false;
    };
    suffix.len() <= 3
        && suffix
            .chars()
            .all(|character| character.is_ascii_alphabetic())
}

fn words_contain_phrase(words: &[String], phrase: &[&str]) -> bool {
    phrase.len() <= words.len()
        && words.windows(phrase.len()).any(|candidate| {
            candidate
                .iter()
                .map(String::as_str)
                .eq(phrase.iter().copied())
        })
}

fn crosses_hard_span_boundary(value: &str) -> bool {
    value
        .chars()
        .any(|character| matches!(character, '\n' | '\r' | '.' | '!' | '?' | ';'))
}

#[cfg(test)]
fn identity_excerpt<'a>(
    source: &str,
    labels: impl Iterator<Item = &'a String>,
) -> (Option<String>, bool) {
    let labels = labels
        .map(String::as_str)
        .filter(|label| !label.trim().is_empty())
        .collect::<Vec<_>>();
    match unique_identity_span(source, &labels) {
        UniqueIdentitySpan::Unique(excerpt) => (Some(excerpt), true),
        UniqueIdentitySpan::Missing | UniqueIdentitySpan::WorkLimitExceeded => (
            (!source.trim().is_empty())
                .then(|| prefix_at_boundary(source, MAX_SOURCE_EXCERPT).to_string()),
            false,
        ),
    }
}

fn prefix_at_boundary(value: &str, limit: usize) -> &str {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub fn group_observations_by_cluster(
    observations: &[AircraftIdentityObservation],
) -> BTreeMap<&str, Vec<&AircraftIdentityObservation>> {
    let mut grouped = BTreeMap::<&str, Vec<&AircraftIdentityObservation>>::new();
    for observation in observations {
        grouped
            .entry(observation.cluster_key.as_str())
            .or_default()
            .push(observation);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::{
        group_observations_by_cluster, identity_excerpt, observation_cluster_key,
        observation_fingerprint, observation_from_row, parse_literal_fields,
        resolve_identity_evidence, retained_source_identity_evidence_matches,
        stage_aircraft_identity_observations, AircraftIdentityObservation, IdentityEvidenceStatus,
        ObservationSourceRow, BASE_MODEL_VARIANT_SPAN_RESOLVER, COMPOSITE_FAMILY_SPAN_RESOLVER,
        LITERAL_IDENTITY_SPAN_RESOLVER,
    };
    use crate::db::{AppDb, DatabaseBackend};

    #[test]
    fn retained_literal_fields_are_not_mechanically_rewritten() {
        let parsed = parse_literal_fields(
            r#"{"manufacturer":"Textron Aviation","model":"182 Skylane","variant":"T182T","model_year":2006}"#,
        )
        .expect("literal extraction should parse");
        assert_eq!(parsed.manufacturer.as_deref(), Some("Textron Aviation"));
        assert_eq!(parsed.model.as_deref(), Some("182 Skylane"));
        assert_eq!(parsed.variant.as_deref(), Some("T182T"));
    }

    #[test]
    fn cluster_key_preserves_material_designation_and_year_differences() {
        assert_ne!(
            observation_cluster_key("Cessna", "182", "182T", 2005),
            observation_cluster_key("Cessna", "182", "T182T", 2005)
        );
        assert_ne!(
            observation_cluster_key("Cirrus", "SR22", "G6", 2020),
            observation_cluster_key("Cirrus", "SR22", "G6 GTS", 2020)
        );
        assert_ne!(
            observation_cluster_key("Cessna", "182", "182T", 2005),
            observation_cluster_key("Cessna", "182", "182T", 2006)
        );
    }

    #[test]
    fn excerpt_requires_every_literal_label_for_exactness() {
        let source = "2006 Cessna 182 T182T Turbo Skylane with Garmin equipment";
        let manufacturer = "Cessna".to_string();
        let model = "182".to_string();
        let variant = "T182T".to_string();
        let (excerpt, exact) =
            identity_excerpt(source, [&manufacturer, &model, &variant].into_iter());
        assert!(exact);
        assert!(excerpt.expect("excerpt").contains("T182T"));

        let wrong = "182I".to_string();
        let (_, exact) = identity_excerpt(source, [&manufacturer, &model, &wrong].into_iter());
        assert!(!exact);
    }

    #[test]
    fn exact_literal_evidence_is_token_bounded() {
        let manufacturer = "Cessna".to_string();
        let model = "182".to_string();
        let variant = "182T".to_string();

        let (_, exact) = identity_excerpt(
            "2022 Cessna 182T Skylane",
            [&manufacturer, &model, &variant].into_iter(),
        );
        assert!(
            !exact,
            "the model token 182 must not be fabricated from 182T"
        );

        let (_, exact) = identity_excerpt(
            "2006 Cessna T182T Turbo Skylane",
            [&manufacturer, &variant].into_iter(),
        );
        assert!(!exact, "the exact variant 182T must not match inside T182T");
    }

    #[test]
    fn composite_model_uses_unique_make_variant_and_family_name_span() {
        for variant in ["182T", "182S", "182R", "182Q", "182P"] {
            let source = format!("2022 CESSNA {variant} SKYLANE • Available now");
            let resolved = resolve_identity_evidence(&source, "Cessna", "182 Skylane", variant);

            assert_eq!(resolved.status, IdentityEvidenceStatus::Exact);
            assert_eq!(resolved.resolver, COMPOSITE_FAMILY_SPAN_RESOLVER);
            assert_eq!(
                resolved.excerpt.as_deref(),
                Some(format!("CESSNA {variant} SKYLANE").as_str())
            );
        }
    }

    #[test]
    fn composite_model_requires_the_exact_variant_token() {
        let turbo = resolve_identity_evidence(
            "2006 Cessna T182T Turbo Skylane",
            "Cessna",
            "182 Skylane",
            "182T",
        );
        assert_eq!(turbo.status, IdentityEvidenceStatus::Missing);

        let suffixed =
            resolve_identity_evidence("2022 Cessna 182T Skylane", "Cessna", "182 Skylane", "182");
        assert_eq!(suffixed.status, IdentityEvidenceStatus::Missing);
    }

    #[test]
    fn composite_model_requires_unambiguous_designator_compatibility() {
        let wrong_base =
            resolve_identity_evidence("2022 Cessna 182T Skylane", "Cessna", "172 Skylane", "182T");
        assert_eq!(wrong_base.status, IdentityEvidenceStatus::Missing);

        let prefixed_variant = resolve_identity_evidence(
            "2006 Cessna T182T Skylane",
            "Cessna",
            "182 Skylane",
            "T182T",
        );
        assert_eq!(
            prefixed_variant.status,
            IdentityEvidenceStatus::Missing,
            "182 must not be treated as a component inside T182T"
        );

        let numeric_prefix =
            resolve_identity_evidence("2022 Cessna 182T Skylane", "Cessna", "18 Skylane", "182T");
        assert_eq!(numeric_prefix.status, IdentityEvidenceStatus::Missing);

        let multiple_numeric_components = resolve_identity_evidence(
            "2022 Cessna 182T Skylane",
            "Cessna",
            "182 182T Skylane",
            "182T",
        );
        assert_eq!(
            multiple_numeric_components.status,
            IdentityEvidenceStatus::Missing
        );

        let valid_alphanumeric_base =
            resolve_identity_evidence("2022 Cirrus SR22T Vision", "Cirrus", "SR22 Vision", "SR22T");
        assert_eq!(
            valid_alphanumeric_base.status,
            IdentityEvidenceStatus::Exact
        );
    }

    #[test]
    fn base_model_uses_the_exact_full_variant_phrase_from_real_listing_shapes() {
        for (variant, source, excerpt) in [
            ("182P", "1974 CESSNA 182P For Sale", "CESSNA 182P"),
            (
                "182Q Skylane",
                "1979 CESSNA 182Q SKYLANE - Low Time",
                "CESSNA 182Q SKYLANE",
            ),
            (
                "Turbo 182T Skylane",
                "2006 CESSNA TURBO 182T SKYLANE For Sale",
                "CESSNA TURBO 182T SKYLANE",
            ),
        ] {
            let resolved = resolve_identity_evidence(source, "Cessna", "182", variant);

            assert_eq!(resolved.status, IdentityEvidenceStatus::Exact);
            assert_eq!(resolved.resolver, BASE_MODEL_VARIANT_SPAN_RESOLVER);
            assert_eq!(resolved.excerpt.as_deref(), Some(excerpt));
        }
    }

    #[test]
    fn base_model_variant_phrase_rejects_prefixed_mismatched_and_ambiguous_designators() {
        for (model, variant, source) in [
            ("182", "T182T", "2006 Cessna T182T"),
            ("172", "182T", "2006 Cessna 182T"),
            ("18", "182T Skylane", "2006 Cessna 182T Skylane"),
            (
                "182",
                "Turbo 182T 206 Skylane",
                "2006 Cessna Turbo 182T 206 Skylane",
            ),
        ] {
            let resolved = resolve_identity_evidence(source, "Cessna", model, variant);
            assert_eq!(
                resolved.status,
                IdentityEvidenceStatus::Missing,
                "{model:?} must not be related mechanically to {variant:?}"
            );
        }
    }

    #[test]
    fn repeated_identical_publisher_titles_are_not_ambiguous() {
        let resolved = resolve_identity_evidence(
            "2022 Cessna 182T Skylane 2022 Cessna 182T Skylane",
            "Cessna",
            "182 Skylane",
            "182T",
        );

        assert_eq!(resolved.status, IdentityEvidenceStatus::Exact);
        assert_eq!(resolved.excerpt.as_deref(), Some("Cessna 182T Skylane"));
    }

    #[test]
    fn distinct_matching_publisher_spans_choose_shortest_then_earliest() {
        let shortest = resolve_identity_evidence(
            "Cessna 182T Skylane. Skylane edition Cessna 182T",
            "Cessna",
            "182 Skylane",
            "182T",
        );

        assert_eq!(shortest.status, IdentityEvidenceStatus::Exact);
        assert_eq!(shortest.excerpt.as_deref(), Some("Cessna 182T Skylane"));

        let earliest = resolve_identity_evidence(
            "Skylane Cessna 182T. Cessna 182T Skylane",
            "Cessna",
            "182 Skylane",
            "182T",
        );

        assert_eq!(earliest.status, IdentityEvidenceStatus::Exact);
        assert_eq!(earliest.excerpt.as_deref(), Some("Skylane Cessna 182T"));
    }

    #[test]
    fn repeated_real_page_identity_sections_choose_one_safe_source_proof() {
        let publisher_text = crate::html::clean::clean_publisher_source_html(
            r#"
            <html>
              <head><title>2022 CESSNA 182T SKYLANE For Sale</title></head>
              <body>
                <nav>Aircraft / Cessna / 182T Skylane</nav>
                <h1>2022 Cessna 182T Skylane</h1>
                <section>
                  <h2>General</h2>
                  <p>This Cessna aircraft is a low-time 182T in the Skylane family.</p>
                </section>
              </body>
            </html>
            "#,
        );

        let resolved = resolve_identity_evidence(&publisher_text, "Cessna", "182 Skylane", "182T");

        assert_eq!(resolved.status, IdentityEvidenceStatus::Exact);
        assert_eq!(resolved.resolver, COMPOSITE_FAMILY_SPAN_RESOLVER);
        assert_eq!(resolved.excerpt.as_deref(), Some("CESSNA 182T SKYLANE"));
    }

    #[test]
    fn repeated_token_page_exceeding_search_work_limit_fails_closed() {
        let source = format!(
            "{}{}{}",
            "Cessna. ".repeat(65),
            "182T. ".repeat(65),
            "Skylane. ".repeat(65)
        );

        let resolved = resolve_identity_evidence(&source, "Cessna", "182 Skylane", "182T");

        assert_eq!(resolved.status, IdentityEvidenceStatus::WorkLimitExceeded);
        assert!(!resolved.is_exact());
    }

    #[test]
    fn existing_literal_identity_path_remains_exact() {
        let resolved = resolve_identity_evidence(
            "2005 Cessna Skylane 182T for sale",
            "Cessna",
            "Skylane",
            "182T",
        );

        assert_eq!(resolved.status, IdentityEvidenceStatus::Exact);
        assert_eq!(resolved.resolver, LITERAL_IDENTITY_SPAN_RESOLVER);
        assert_eq!(resolved.excerpt.as_deref(), Some("Cessna Skylane 182T"));
    }

    #[test]
    fn retained_publisher_title_resolves_composite_model_without_rewriting_fields() {
        let row = ObservationSourceRow {
            listing_id: 8,
            listing_source_url: Some("https://example.test/listing/8".to_string()),
            stored_manufacturer: "Legacy Make".to_string(),
            stored_model: "Legacy Model".to_string(),
            stored_variant: "Legacy Variant".to_string(),
            stored_model_year: 2022,
            stored_serial_number: Some("18200001".to_string()),
            stored_registration_number: Some("N182AA".to_string()),
            submission_id: Some(10),
            submission_source_url: None,
            rendered_html_sha256: Some("b".repeat(64)),
            rendered_html: Some(
                r#"
                <html>
                  <head><title>2022 CESSNA 182T SKYLANE For Sale</title></head>
                  <body><p>Well-equipped aircraft.</p></body>
                </html>
                "#
                .to_string(),
            ),
            extracted_listing_json: Some(
                serde_json::json!({
                    "manufacturer": "Cessna",
                    "model": "182 Skylane",
                    "variant": "182T",
                    "model_year": 2022,
                    "registration_number": "N182AA",
                    "serial_number": "18200001"
                })
                .to_string(),
            ),
        };

        let observation = observation_from_row(&row);

        assert_eq!(observation.manufacturer, "Cessna");
        assert_eq!(observation.model, "182 Skylane");
        assert_eq!(observation.variant, "182T");
        assert_eq!(
            observation.source_excerpt.as_deref(),
            Some("CESSNA 182T SKYLANE")
        );
        assert!(observation.source_excerpt_is_exact);
        assert!(!observation.requires_human_review);
        assert!(retained_source_identity_evidence_matches(
            row.rendered_html.as_deref().expect("retained HTML"),
            &observation
        ));

        let mut wrong_fingerprint = observation.clone();
        wrong_fingerprint.observation_sha256 = "0".repeat(64);
        assert!(!retained_source_identity_evidence_matches(
            row.rendered_html.as_deref().expect("retained HTML"),
            &wrong_fingerprint
        ));
    }

    #[test]
    fn observation_rejects_identity_present_only_in_hidden_or_script_text() {
        let row = ObservationSourceRow {
            listing_id: 8,
            listing_source_url: Some("https://example.test/listing/8".to_string()),
            stored_manufacturer: "Cessna".to_string(),
            stored_model: "182 Skylane".to_string(),
            stored_variant: "182T".to_string(),
            stored_model_year: 2022,
            stored_serial_number: Some("18200001".to_string()),
            stored_registration_number: Some("N182AA".to_string()),
            submission_id: Some(10),
            submission_source_url: None,
            rendered_html_sha256: Some("b".repeat(64)),
            rendered_html: Some(
                r#"
                <html><body>
                  <p>Aircraft listing</p>
                  <div hidden>2022 Cessna 182T Skylane</div>
                  <script>const identity = "2022 Cessna 182T Skylane";</script>
                </body></html>
                "#
                .to_string(),
            ),
            extracted_listing_json: Some(
                serde_json::json!({
                    "manufacturer": "Cessna",
                    "model": "182 Skylane",
                    "variant": "182T",
                    "model_year": 2022,
                    "registration_number": "N182AA",
                    "serial_number": "18200001"
                })
                .to_string(),
            ),
        };

        let observation = observation_from_row(&row);

        assert_eq!(observation.model, "182 Skylane");
        assert_eq!(observation.variant, "182T");
        assert!(!observation.source_excerpt_is_exact);
        assert!(observation.requires_human_review);
    }

    #[test]
    fn observation_fingerprint_binds_exact_evidence_and_resolver() {
        let fingerprint = |evidence, resolver| {
            observation_fingerprint(
                1,
                Some(2),
                Some("rendered-digest"),
                "Cessna",
                "182 Skylane",
                "182T",
                2022,
                Some("18200001"),
                Some("N182AA"),
                evidence,
                resolver,
            )
        };

        assert_ne!(
            fingerprint(Some("Cessna 182T Skylane"), COMPOSITE_FAMILY_SPAN_RESOLVER),
            fingerprint(Some("Cessna Skylane 182T"), COMPOSITE_FAMILY_SPAN_RESOLVER)
        );
        assert_ne!(
            fingerprint(Some("Cessna 182T Skylane"), COMPOSITE_FAMILY_SPAN_RESOLVER),
            fingerprint(Some("Cessna 182T Skylane"), LITERAL_IDENTITY_SPAN_RESOLVER)
        );
    }

    #[test]
    fn faa_admission_identifiers_always_come_from_the_current_listing() {
        let row = ObservationSourceRow {
            listing_id: 7,
            listing_source_url: Some("https://example.test/listing/7".to_string()),
            stored_manufacturer: "Cessna".to_string(),
            stored_model: "182".to_string(),
            stored_variant: "182J".to_string(),
            stored_model_year: 1966,
            stored_serial_number: Some("CURRENT-SERIAL".to_string()),
            stored_registration_number: Some("C-FOREIGN".to_string()),
            submission_id: Some(9),
            submission_source_url: None,
            rendered_html_sha256: Some("a".repeat(64)),
            rendered_html: Some(
                "1966 Cessna 182 182J, registration C-FOREIGN, serial CURRENT-SERIAL".to_string(),
            ),
            extracted_listing_json: Some(
                serde_json::json!({
                    "manufacturer": "Cessna",
                    "model": "182",
                    "variant": "182J",
                    "model_year": 1966,
                    "registration_number": "N3510F",
                    "serial_number": "18257510"
                })
                .to_string(),
            ),
        };

        let observation = observation_from_row(&row);

        assert_eq!(
            observation.registration_number.as_deref(),
            Some("C-FOREIGN")
        );
        assert_eq!(observation.serial_number.as_deref(), Some("CURRENT-SERIAL"));
        assert!(observation.requires_human_review);
        assert!(observation
            .review_reasons
            .iter()
            .any(|reason| reason.contains("registration differs")));
        assert!(observation
            .review_reasons
            .iter()
            .any(|reason| reason.contains("serial differs")));
    }

    #[test]
    fn grouping_never_merges_materially_different_clusters() {
        fn observation(cluster_key: &str, listing_id: i64) -> AircraftIdentityObservation {
            AircraftIdentityObservation {
                listing_id,
                submission_id: None,
                source_url: None,
                rendered_html_sha256: None,
                manufacturer: String::new(),
                model: String::new(),
                variant: String::new(),
                model_year: 2000,
                serial_number: None,
                registration_number: None,
                source_excerpt: None,
                source_excerpt_is_exact: false,
                source_kind: String::new(),
                observation_sha256: String::new(),
                cluster_key: cluster_key.to_string(),
                requires_human_review: true,
                review_reasons: vec![],
            }
        }
        let observations = vec![
            observation("cessna:182:182t:2005", 1),
            observation("cessna:182:t182t:2005", 2),
        ];
        assert_eq!(group_observations_by_cluster(&observations).len(), 2);
    }

    #[tokio::test]
    async fn replay_reattaches_only_an_identical_detached_observation() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let variant_id: i64 = match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query_scalar(
                "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            DatabaseBackend::Postgres(_) => unreachable!(),
        };
        let listing_id: i64 = match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query_scalar(
                r#"
                INSERT INTO aircraft_sale_listings (
                  aircraft_model_variant_id, created_by_user_id, source_url,
                  model_year, asking_price_usd, airframe_hours
                ) VALUES (?, ?, 'https://example.test/replay', 2020, 100000, 500)
                RETURNING id
                "#,
            )
            .bind(variant_id)
            .bind(user.id)
            .fetch_one(pool)
            .await
            .unwrap(),
            DatabaseBackend::Postgres(_) => unreachable!(),
        };
        let observation = AircraftIdentityObservation {
            listing_id,
            submission_id: Some(77),
            source_url: Some("https://example.test/replay".to_string()),
            rendered_html_sha256: Some("a".repeat(64)),
            manufacturer: "Cessna".to_string(),
            model: "182".to_string(),
            variant: "182T".to_string(),
            model_year: 2020,
            serial_number: Some("18200001".to_string()),
            registration_number: Some("N123AB".to_string()),
            source_excerpt: Some("Cessna 182T".to_string()),
            source_excerpt_is_exact: true,
            source_kind: "retained_submission".to_string(),
            observation_sha256: "b".repeat(64),
            cluster_key: "cessna:182:182t:2020".to_string(),
            requires_human_review: false,
            review_reasons: Vec::new(),
        };
        let inserted = stage_aircraft_identity_observations(&db, &[observation.clone()])
            .await
            .unwrap();
        assert_eq!(inserted.inserted, 1);
        if let DatabaseBackend::Sqlite(pool) = db.backend() {
            sqlx::query(
                "UPDATE aircraft_identity_observations SET aircraft_sale_listing_id = NULL WHERE observation_sha256 = ?",
            )
            .bind(&observation.observation_sha256)
            .execute(pool)
            .await
            .unwrap();
        }

        let reattached = stage_aircraft_identity_observations(&db, &[observation.clone()])
            .await
            .unwrap();
        assert_eq!(reattached.reattached, 1);
        let attached_listing: Option<i64> = match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query_scalar(
                "SELECT aircraft_sale_listing_id FROM aircraft_identity_observations WHERE observation_sha256 = ?",
            )
            .bind(&observation.observation_sha256)
            .fetch_one(pool)
            .await
            .unwrap(),
            DatabaseBackend::Postgres(_) => unreachable!(),
        };
        assert_eq!(attached_listing, Some(listing_id));

        if let DatabaseBackend::Sqlite(pool) = db.backend() {
            sqlx::query(
                "UPDATE aircraft_identity_observations SET aircraft_sale_listing_id = NULL, exact_source_evidence = 'different' WHERE observation_sha256 = ?",
            )
            .bind(&observation.observation_sha256)
            .execute(pool)
            .await
            .unwrap();
        }
        let collision = stage_aircraft_identity_observations(&db, &[observation])
            .await
            .unwrap_err();
        assert!(collision.to_string().contains("different immutable"));
    }
}
