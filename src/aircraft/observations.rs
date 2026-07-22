//! Literal aircraft observations retained separately from canonical identity.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::html::clean::clean_listing_html_with_limit;

const MAX_LOCAL_HTML_TEXT: usize = 4_000_000;
const MAX_SOURCE_EXCERPT: usize = 2_000;

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

#[derive(Clone, Debug, Default, Deserialize)]
struct LiteralAircraftFields {
    manufacturer: Option<String>,
    model: Option<String>,
    variant: Option<String>,
    model_year: Option<i64>,
    serial_number: Option<String>,
    registration_number: Option<String>,
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

/// Persist only observations whose literal labels can be located in retained
/// source text. This is a staging write, not a catalog approval or merge.
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
            report.already_present += 1;
        } else {
            report.inserted += 1;
        }
    }
    Ok(report)
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

    let cleaned = row
        .rendered_html
        .as_deref()
        .map(|html| clean_listing_html_with_limit(html, MAX_LOCAL_HTML_TEXT))
        .unwrap_or_default();
    let (source_excerpt, source_excerpt_is_exact) = identity_excerpt(
        &cleaned,
        [&manufacturer, &model, &variant]
            .into_iter()
            .filter(|value| !value.trim().is_empty()),
    );

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
    if !source_excerpt_is_exact {
        review_reasons
            .push("identity labels were not found verbatim in retained source text".to_string());
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
        "observation_schema_version": 1,
    });
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(&material).expect("observation material serializes"));
    format!("{:x}", hasher.finalize())
}

fn identity_excerpt<'a>(
    source: &str,
    labels: impl Iterator<Item = &'a String>,
) -> (Option<String>, bool) {
    if source.trim().is_empty() {
        return (None, false);
    }
    let lowercase = source.to_lowercase();
    let labels = labels
        .map(|label| label.trim())
        .filter(|label| !label.is_empty())
        .collect::<Vec<_>>();
    let anchors = labels
        .iter()
        .filter_map(|label| lowercase.find(&label.to_lowercase()))
        .collect::<Vec<_>>();
    if anchors.is_empty() {
        return (
            Some(prefix_at_boundary(source, MAX_SOURCE_EXCERPT).to_string()),
            false,
        );
    }
    let anchor = *anchors.iter().min().unwrap_or(&0);
    let mut start = anchor.saturating_sub(MAX_SOURCE_EXCERPT / 4);
    while start > 0 && !source.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + MAX_SOURCE_EXCERPT).min(source.len());
    while end > start && !source.is_char_boundary(end) {
        end -= 1;
    }
    (
        Some(source[start..end].to_string()),
        anchors.len() == labels.len(),
    )
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
        observation_from_row, parse_literal_fields, AircraftIdentityObservation,
        ObservationSourceRow,
    };

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
        let source = "2006 Cessna T182T Turbo Skylane with Garmin equipment";
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
}
