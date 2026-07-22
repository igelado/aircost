use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::aircraft::faa::{
    audit_listing_admission, ListingAdmissionEvidence, ListingAdmissionReport,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::models::is_plausible_asking_price_usd;

use super::types::{
    source_backed_component_observation, SourceBackedValuationFact, TrainingListing, ValuationError,
};
use super::FEATURE_SCHEMA_VERSION;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SnapshotPolicy {
    pub capture_time: String,
    pub max_listing_age_days: i64,
    pub require_source_backed_or_verified: bool,
    pub minimum_model_year: i64,
}

impl Default for SnapshotPolicy {
    fn default() -> Self {
        Self {
            capture_time: current_capture_time(),
            max_listing_age_days: 180,
            require_source_backed_or_verified: true,
            minimum_model_year: 1900,
        }
    }
}

const FAA_ADMISSION_MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
struct SnapshotFaaAdmissionManifest {
    schema_version: u32,
    included_listings: BTreeMap<i64, ListingAdmissionEvidence>,
}

#[derive(Serialize)]
struct PersistedSnapshotPolicy<'a> {
    #[serde(flatten)]
    policy: &'a SnapshotPolicy,
    faa_admission: SnapshotFaaAdmissionManifest,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SnapshotReport {
    pub applied: bool,
    pub snapshot_id: Option<i64>,
    pub capture_time: String,
    pub input_sha256: String,
    pub feature_schema_version: u32,
    pub included_count: usize,
    pub excluded_count: usize,
    pub duplicate_group_count: usize,
    pub missing_airframe_hours: usize,
    pub counts_by_manufacturer: BTreeMap<i64, usize>,
    pub counts_by_model: BTreeMap<i64, usize>,
    pub counts_by_variant: BTreeMap<i64, usize>,
    pub exclusions: BTreeMap<String, usize>,
}

#[derive(Clone, Debug)]
struct PreparedRow {
    listing_id: i64,
    added_at: String,
    duplicate_group_key: String,
    included: bool,
    exclusion_reason: Option<String>,
    feature_json: String,
    target_price_usd: Option<f64>,
    row_sha256: String,
    training: TrainingListing,
}

#[derive(Debug, FromRow)]
struct ListingRow {
    id: i64,
    manufacturer_id: i64,
    model_id: i64,
    variant_id: i64,
    is_verified: bool,
    source_url: Option<String>,
    model_year: i64,
    asking_price_usd: f64,
    currency: String,
    added_at: String,
    status: String,
    ingestion_state: String,
    registration_number: Option<String>,
    serial_number: Option<String>,
    airframe_hours: f64,
    engine_hours: Option<f64>,
    engine_time_basis: String,
    engine_time_evidence: Option<String>,
    engine_time_confidence: Option<String>,
    propeller_hours: Option<f64>,
    propeller_time_basis: String,
    propeller_time_evidence: Option<String>,
    propeller_time_confidence: Option<String>,
}

#[derive(Debug, FromRow)]
struct EquipmentRow {
    listing_id: i64,
    namespace: String,
    manufacturer: String,
    model: String,
    quantity: i64,
}

#[derive(Debug, FromRow)]
struct FactRow {
    listing_id: i64,
    fact_kind: String,
    fact_value: String,
    evidence_text: String,
    source_url: Option<String>,
    source_confidence: String,
}

#[derive(Debug, FromRow)]
struct SnapshotDbRow {
    id: i64,
    capture_time: String,
}

#[derive(Debug, FromRow)]
struct FrozenDbRow {
    source_listing_id: i64,
    feature_json: String,
}

pub async fn create_snapshot(
    db: &AppDb,
    policy: &SnapshotPolicy,
    apply: bool,
) -> Result<SnapshotReport, ValuationError> {
    let capture_day = parse_date_days(&policy.capture_time).ok_or_else(|| {
        ValuationError::InvalidQuery("capture_time must begin with YYYY-MM-DD".to_string())
    })?;
    let snapshot_year = parse_year(&policy.capture_time).ok_or_else(|| {
        ValuationError::InvalidQuery("capture_time must begin with a four-digit year".to_string())
    })?;
    if policy.max_listing_age_days < 0 {
        return Err(ValuationError::InvalidQuery(
            "max listing age cannot be negative".to_string(),
        ));
    }
    let listings = load_listing_rows(db).await?;
    let listing_ids = listings
        .iter()
        .map(|listing| listing.id)
        .collect::<BTreeSet<_>>();
    let faa_admission = audit_listing_admission(db, Some(&listing_ids))
        .await
        .map_err(|error| ValuationError::Database(error.to_string()))?;
    let equipment = load_equipment(db).await?;
    let facts = load_facts(db).await?;
    let mut prepared = Vec::with_capacity(listings.len());
    for listing in listings {
        let mut tokens = equipment.get(&listing.id).cloned().unwrap_or_default();
        let valuation_facts = facts.get(&listing.id).cloned().unwrap_or_default();
        tokens.extend(
            valuation_facts
                .iter()
                .filter(|fact| fact.confidence == "high")
                .map(fact_token),
        );
        tokens.sort();
        let duplicate_group_key = duplicate_key(&listing);
        let reason = faa_admission
            .exclusion_reason(listing.id)
            .map(|reason| format!("faa_{reason}"))
            .or_else(|| exclusion_reason(&listing, policy, capture_day, snapshot_year));
        let technical_field_count = technical_field_count(
            listing.engine_hours.is_some(),
            listing.propeller_hours.is_some(),
            listing.registration_number.is_some(),
            listing.serial_number.is_some(),
            !tokens.is_empty(),
        );
        let training = TrainingListing {
            listing_id: listing.id,
            duplicate_group_key: duplicate_group_key.clone(),
            category_key: None,
            manufacturer_id: listing.manufacturer_id,
            model_id: listing.model_id,
            variant_id: listing.variant_id,
            model_year: listing.model_year,
            snapshot_year,
            asking_price_usd: listing.asking_price_usd,
            airframe_hours: finite_nonnegative(listing.airframe_hours),
            engine_times: vec![source_backed_component_observation(
                listing.engine_hours,
                &listing.engine_time_basis,
                listing.engine_time_evidence.as_deref(),
                listing.engine_time_confidence.as_deref(),
                1,
            )],
            propeller_times: vec![source_backed_component_observation(
                listing.propeller_hours,
                &listing.propeller_time_basis,
                listing.propeller_time_evidence.as_deref(),
                listing.propeller_time_confidence.as_deref(),
                1,
            )],
            equipment_tokens: tokens,
            valuation_facts,
            technical_field_count,
        };
        let feature_json = serde_json::to_string(&training)?;
        let target = is_plausible_asking_price_usd(listing.asking_price_usd)
            .then_some(listing.asking_price_usd);
        let row_sha256 = sha256_hex(
            format!(
                "{}\n{}\n{}\n{}\n{}",
                listing.id,
                duplicate_group_key,
                feature_json,
                target.map(|value| value.to_string()).unwrap_or_default(),
                faa_admission
                    .admission_evidence(listing.id)
                    .map(serde_json::to_string)
                    .transpose()?
                    .unwrap_or_default()
            )
            .as_bytes(),
        );
        prepared.push(PreparedRow {
            listing_id: listing.id,
            added_at: listing.added_at,
            duplicate_group_key,
            included: reason.is_none(),
            exclusion_reason: reason,
            feature_json,
            target_price_usd: target,
            row_sha256,
            training,
        });
    }
    deduplicate(&mut prepared);
    prepared.sort_by_key(|row| row.listing_id);
    let frozen_included_listings = prepared
        .iter()
        .filter(|row| row.included)
        .map(|row| {
            faa_admission
                .admission_evidence(row.listing_id)
                .cloned()
                .map(|evidence| (row.listing_id, evidence))
                .ok_or_else(|| {
                    ValuationError::ValidationGate(format!(
                        "listing {} was selected without frozen FAA admission evidence",
                        row.listing_id
                    ))
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let policy_json = serde_json::to_string(&PersistedSnapshotPolicy {
        policy,
        faa_admission: SnapshotFaaAdmissionManifest {
            schema_version: FAA_ADMISSION_MANIFEST_VERSION,
            included_listings: frozen_included_listings.clone(),
        },
    })?;
    let mut snapshot_material = format!("{policy_json}\n{FEATURE_SCHEMA_VERSION}\n");
    for row in &prepared {
        snapshot_material.push_str(&format!(
            "{}|{}|{}|{}|{}\n",
            row.listing_id,
            row.duplicate_group_key,
            row.included,
            row.exclusion_reason.as_deref().unwrap_or_default(),
            row.row_sha256
        ));
    }
    let input_sha256 = sha256_hex(snapshot_material.as_bytes());
    let included_count = prepared.iter().filter(|row| row.included).count();
    let excluded_count = prepared.len() - included_count;
    let mut report = build_report(
        policy,
        &input_sha256,
        &prepared,
        included_count,
        excluded_count,
    );
    if apply {
        let included_ids = frozen_included_listings
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        let final_admission = audit_listing_admission(db, Some(&included_ids))
            .await
            .map_err(|error| ValuationError::Database(error.to_string()))?;
        if final_admission.excluded_count != 0
            || !admission_evidence_matches(&final_admission, &frozen_included_listings)
        {
            return Err(ValuationError::ValidationGate(
                "FAA admission identity or release changed while building the valuation snapshot; retry the operation"
                    .to_string(),
            ));
        }
        let snapshot_id = persist_snapshot(
            db,
            policy,
            &policy_json,
            &input_sha256,
            &prepared,
            included_count,
            excluded_count,
        )
        .await?;
        report.applied = true;
        report.snapshot_id = Some(snapshot_id);
    }
    Ok(report)
}

pub async fn load_snapshot(
    db: &AppDb,
    snapshot_id: i64,
) -> Result<Vec<TrainingListing>, ValuationError> {
    let version_sql = db.sql("SELECT feature_schema_version FROM valuation_snapshots WHERE id = ?");
    let feature_schema_version = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_scalar::<_, i64>(&version_sql)
            .bind(snapshot_id)
            .fetch_optional(pool)
            .await?
            .map(|version| version as u32),
        DatabaseBackend::Postgres(pool) => sqlx::query_scalar::<_, i32>(&version_sql)
            .bind(snapshot_id)
            .fetch_optional(pool)
            .await?
            .map(|version| version as u32),
    }
    .ok_or_else(|| ValuationError::Database(format!("snapshot {snapshot_id} not found")))?;
    if feature_schema_version != FEATURE_SCHEMA_VERSION {
        return Err(ValuationError::InvalidArtifact(format!(
            "snapshot {snapshot_id} feature schema {feature_schema_version} does not match current schema {FEATURE_SCHEMA_VERSION}"
        )));
    }
    require_snapshot_faa_admission(db, snapshot_id).await?;
    let sql = db.sql(
        r#"
        SELECT source_listing_id, feature_json
        FROM valuation_snapshot_rows
        WHERE snapshot_id = ? AND inclusion_flag = TRUE
        ORDER BY source_listing_id
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, FrozenDbRow>(&sql)
                .bind(snapshot_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, FrozenDbRow>(&sql)
                .bind(snapshot_id)
                .fetch_all(pool)
                .await?
        }
    };
    rows.into_iter()
        .map(|row| {
            let training: TrainingListing = serde_json::from_str(&row.feature_json)?;
            if training.listing_id != row.source_listing_id {
                return Err(ValuationError::InvalidArtifact(format!(
                    "snapshot {snapshot_id} row source listing {} disagrees with feature listing {}",
                    row.source_listing_id, training.listing_id
                )));
            }
            Ok(training)
        })
        .collect()
}

/// Re-evaluate the frozen included listing IDs against the current regulator
/// release. This keeps exclusion accounting observable without modifying the
/// immutable valuation snapshot.
pub async fn snapshot_faa_admission_report(
    db: &AppDb,
    snapshot_id: i64,
) -> Result<ListingAdmissionReport, ValuationError> {
    let sql = db.sql(
        r#"
        SELECT source_listing_id
        FROM valuation_snapshot_rows
        WHERE snapshot_id = ? AND inclusion_flag = TRUE
        ORDER BY source_listing_id
        "#,
    );
    let listing_ids = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(snapshot_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(snapshot_id)
                .fetch_all(pool)
                .await?
        }
    }
    .into_iter()
    .collect::<BTreeSet<_>>();
    audit_listing_admission(db, Some(&listing_ids))
        .await
        .map_err(|error| ValuationError::Database(error.to_string()))
}

/// Reject a legacy or externally modified snapshot when even one frozen
/// included row is not currently FAA-admitted. Filtering a frozen snapshot at
/// load time would change its training material without changing its hash, so
/// callers must build a fresh snapshot instead.
pub async fn require_snapshot_faa_admission(
    db: &AppDb,
    snapshot_id: i64,
) -> Result<ListingAdmissionReport, ValuationError> {
    let report = snapshot_faa_admission_report(db, snapshot_id).await?;
    if report.excluded_count == 0 {
        let manifest = load_snapshot_faa_manifest(db, snapshot_id).await?;
        if manifest.schema_version != FAA_ADMISSION_MANIFEST_VERSION {
            return Err(ValuationError::ValidationGate(format!(
                "valuation snapshot {snapshot_id} FAA admission manifest schema {} does not match current schema {FAA_ADMISSION_MANIFEST_VERSION}",
                manifest.schema_version
            )));
        }
        if !admission_evidence_matches(&report, &manifest.included_listings) {
            return Err(ValuationError::ValidationGate(format!(
                "valuation snapshot {snapshot_id} FAA admission identity or provenance changed; create a fresh valuation snapshot"
            )));
        }
        return Ok(report);
    }
    let reasons = report
        .exclusions
        .iter()
        .map(|(reason, count)| format!("{reason}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(ValuationError::ValidationGate(format!(
        "valuation snapshot {snapshot_id} contains {} FAA-ineligible included listing(s) ({reasons}); create a fresh valuation snapshot",
        report.excluded_count
    )))
}

fn admission_evidence_matches(
    report: &ListingAdmissionReport,
    frozen: &BTreeMap<i64, ListingAdmissionEvidence>,
) -> bool {
    frozen.len() == report.admitted_count
        && frozen
            .iter()
            .all(|(listing_id, evidence)| report.admission_evidence(*listing_id) == Some(evidence))
}

async fn load_snapshot_faa_manifest(
    db: &AppDb,
    snapshot_id: i64,
) -> Result<SnapshotFaaAdmissionManifest, ValuationError> {
    let sql = db.sql("SELECT selection_policy_json FROM valuation_snapshots WHERE id = ?");
    let policy_json = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, String>(&sql)
                .bind(snapshot_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, String>(&sql)
                .bind(snapshot_id)
                .fetch_optional(pool)
                .await?
        }
    }
    .ok_or_else(|| ValuationError::Database(format!("snapshot {snapshot_id} not found")))?;
    let policy: serde_json::Value = serde_json::from_str(&policy_json)?;
    let manifest = policy.get("faa_admission").ok_or_else(|| {
        ValuationError::ValidationGate(format!(
            "valuation snapshot {snapshot_id} predates the mandatory FAA admission manifest; create a fresh valuation snapshot"
        ))
    })?;
    serde_json::from_value(manifest.clone()).map_err(|error| {
        ValuationError::InvalidArtifact(format!(
            "valuation snapshot {snapshot_id} has an invalid FAA admission manifest: {error}"
        ))
    })
}

pub async fn newest_snapshot(
    db: &AppDb,
) -> Result<Option<(i64, Vec<TrainingListing>)>, ValuationError> {
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, SnapshotDbRow>(
                "SELECT id, capture_time FROM valuation_snapshots ORDER BY id DESC LIMIT 1",
            )
            .fetch_optional(pool)
            .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, SnapshotDbRow>(
                "SELECT id, capture_time FROM valuation_snapshots ORDER BY id DESC LIMIT 1",
            )
            .fetch_optional(pool)
            .await?
        }
    };
    match row {
        Some(row) => {
            let _capture_time = row.capture_time;
            Ok(Some((row.id, load_snapshot(db, row.id).await?)))
        }
        None => Ok(None),
    }
}

async fn load_listing_rows(db: &AppDb) -> Result<Vec<ListingRow>, ValuationError> {
    let sql = r#"
        SELECT
          listing.id,
          manufacturer.id AS manufacturer_id,
          model.id AS model_id,
          variant.id AS variant_id,
          listing.is_verified,
          listing.source_url,
          listing.model_year,
          listing.asking_price_usd,
          listing.currency,
          listing.added_at,
          listing.status,
          listing.ingestion_state,
          listing.registration_number,
          listing.serial_number,
          listing.airframe_hours,
          listing.engine_hours,
          listing.engine_time_basis,
          listing.engine_time_evidence,
          listing.engine_time_confidence,
          listing.propeller_hours,
          listing.propeller_time_basis,
          listing.propeller_time_evidence,
          listing.propeller_time_confidence
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer
          ON manufacturer.id = model.aircraft_manufacturer_id
        ORDER BY listing.id
    "#;
    Ok(match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ListingRow>(sql).fetch_all(pool).await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ListingRow>(sql).fetch_all(pool).await?
        }
    })
}

async fn load_facts(
    db: &AppDb,
) -> Result<BTreeMap<i64, Vec<SourceBackedValuationFact>>, ValuationError> {
    let sql = r#"
        SELECT
          aircraft_sale_listing_id AS listing_id,
          fact_kind,
          fact_value,
          evidence_text,
          source_url,
          source_confidence
        FROM aircraft_sale_listing_facts
        ORDER BY aircraft_sale_listing_id, fact_kind, id
    "#;
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_as::<_, FactRow>(sql).fetch_all(pool).await?,
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, FactRow>(sql).fetch_all(pool).await?
        }
    };
    let mut facts = BTreeMap::new();
    for row in rows {
        facts
            .entry(row.listing_id)
            .or_insert_with(Vec::new)
            .push(SourceBackedValuationFact {
                kind: row.fact_kind,
                value: row.fact_value,
                evidence_text: row.evidence_text,
                source_url: row.source_url,
                confidence: row.source_confidence,
            });
    }
    Ok(facts)
}

async fn load_equipment(db: &AppDb) -> Result<BTreeMap<i64, Vec<String>>, ValuationError> {
    let sql = r#"
        SELECT
          link.aircraft_sale_listing_id AS listing_id,
          'avionics' AS namespace,
          manufacturer.name AS manufacturer,
          model.name AS model,
          link.quantity
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model ON model.id = link.avionics_model_id
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        WHERE link.source = 'listing'
          AND link.configuration_action IN ('installed', 'replaces')
          AND link.source_confidence = 'high'
          AND model.catalog_status = 'approved'
          AND (
            link.replaces_avionics_model_id IS NULL
            OR EXISTS (
              SELECT 1
              FROM avionics_models replaced_model
              WHERE replaced_model.id = link.replaces_avionics_model_id
                AND replaced_model.catalog_status = 'approved'
            )
          )
        UNION ALL
        SELECT
          listing.id AS listing_id,
          'engine' AS namespace,
          manufacturer.name AS manufacturer,
          model.name AS model,
          1 AS quantity
        FROM aircraft_sale_listings listing
        JOIN engine_models model ON model.id = listing.installed_engine_model_id
        JOIN engine_manufacturers manufacturer
          ON manufacturer.id = model.engine_manufacturer_id
        WHERE listing.installed_engine_confidence = 'high'
        UNION ALL
        SELECT
          listing.id AS listing_id,
          'propeller' AS namespace,
          manufacturer.name AS manufacturer,
          model.name AS model,
          1 AS quantity
        FROM aircraft_sale_listings listing
        JOIN propeller_models model ON model.id = listing.installed_propeller_model_id
        JOIN propeller_manufacturers manufacturer
          ON manufacturer.id = model.propeller_manufacturer_id
        WHERE listing.installed_propeller_confidence = 'high'
        ORDER BY listing_id, namespace, manufacturer, model
    "#;
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, EquipmentRow>(sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, EquipmentRow>(sql)
                .fetch_all(pool)
                .await?
        }
    };
    let mut equipment = BTreeMap::new();
    for row in rows {
        let token = equipment_feature_token(&row.namespace, &row.manufacturer, &row.model);
        for _ in 0..row.quantity.max(1) {
            equipment
                .entry(row.listing_id)
                .or_insert_with(Vec::new)
                .push(token.clone());
        }
    }
    Ok(equipment)
}

fn fact_token(fact: &SourceBackedValuationFact) -> String {
    equipment_feature_token("fact", &fact.kind, &fact.value)
}

pub(crate) fn equipment_feature_token(
    namespace: &str,
    manufacturer_or_kind: &str,
    model_or_value: &str,
) -> String {
    format!(
        "{}:{}:{}",
        normalize_token_segment(namespace),
        normalize_token_segment(manufacturer_or_kind),
        normalize_token_segment(model_or_value)
    )
}

pub(crate) fn technical_field_count(
    has_engine_time: bool,
    has_propeller_time: bool,
    has_registration: bool,
    has_serial: bool,
    has_usable_feature_tokens: bool,
) -> u32 {
    1 + u32::from(has_engine_time)
        + u32::from(has_propeller_time)
        + u32::from(has_registration)
        + u32::from(has_serial)
        + u32::from(has_usable_feature_tokens)
}

fn normalize_token_segment(value: &str) -> String {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("_")
}

fn exclusion_reason(
    listing: &ListingRow,
    policy: &SnapshotPolicy,
    capture_day: i64,
    snapshot_year: i64,
) -> Option<String> {
    if listing.ingestion_state != "ready" {
        return Some(format!("ingestion_{}", listing.ingestion_state));
    }
    if listing.status != "active" {
        return Some("inactive".to_string());
    }
    if listing.currency != "USD" || !is_plausible_asking_price_usd(listing.asking_price_usd) {
        return Some("non_usd_or_implausible_price".to_string());
    }
    if listing.model_year < policy.minimum_model_year
        || listing.model_year > snapshot_year
        || !listing.airframe_hours.is_finite()
        || !(0.0..=100_000.0).contains(&listing.airframe_hours)
        || [listing.engine_hours, listing.propeller_hours]
            .iter()
            .flatten()
            .any(|hours| !hours.is_finite() || !(0.0..=100_000.0).contains(hours))
    {
        return Some("implausible_year_or_hours".to_string());
    }
    if policy.require_source_backed_or_verified
        && listing.source_url.as_deref().is_none_or(str::is_empty)
        && !listing.is_verified
    {
        return Some("untrusted_source".to_string());
    }
    let Some(listing_day) = parse_date_days(&listing.added_at) else {
        return Some("invalid_capture_time".to_string());
    };
    if listing_day > capture_day
        || capture_day.saturating_sub(listing_day) > policy.max_listing_age_days
    {
        return Some("stale_or_future_listing".to_string());
    }
    None
}

fn deduplicate(rows: &mut [PreparedRow]) {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate().filter(|(_, row)| row.included) {
        groups
            .entry(row.duplicate_group_key.clone())
            .or_default()
            .push(index);
    }
    for members in groups.into_values().filter(|members| members.len() > 1) {
        let winner = members
            .iter()
            .copied()
            .max_by_key(|index| {
                (
                    rows[*index].training.technical_field_count,
                    rows[*index].added_at.as_str(),
                    rows[*index].listing_id,
                )
            })
            .unwrap_or(members[0]);
        for index in members {
            if index != winner {
                rows[index].included = false;
                rows[index].exclusion_reason = Some("duplicate_aircraft".to_string());
            }
        }
    }
}

fn build_report(
    policy: &SnapshotPolicy,
    input_sha256: &str,
    rows: &[PreparedRow],
    included_count: usize,
    excluded_count: usize,
) -> SnapshotReport {
    let included: Vec<_> = rows.iter().filter(|row| row.included).collect();
    let mut exclusions = BTreeMap::new();
    let mut manufacturers = BTreeMap::new();
    let mut models = BTreeMap::new();
    let mut variants = BTreeMap::new();
    for row in rows {
        if let Some(reason) = &row.exclusion_reason {
            *exclusions.entry(reason.clone()).or_default() += 1;
        }
    }
    for row in &included {
        *manufacturers
            .entry(row.training.manufacturer_id)
            .or_default() += 1;
        *models.entry(row.training.model_id).or_default() += 1;
        *variants.entry(row.training.variant_id).or_default() += 1;
    }
    SnapshotReport {
        applied: false,
        snapshot_id: None,
        capture_time: policy.capture_time.clone(),
        input_sha256: input_sha256.to_string(),
        feature_schema_version: FEATURE_SCHEMA_VERSION,
        included_count,
        excluded_count,
        duplicate_group_count: included
            .iter()
            .map(|row| &row.duplicate_group_key)
            .collect::<BTreeSet<_>>()
            .len(),
        missing_airframe_hours: included
            .iter()
            .filter(|row| row.training.airframe_hours.is_none())
            .count(),
        counts_by_manufacturer: manufacturers,
        counts_by_model: models,
        counts_by_variant: variants,
        exclusions,
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_snapshot(
    db: &AppDb,
    policy: &SnapshotPolicy,
    policy_json: &str,
    input_sha256: &str,
    rows: &[PreparedRow],
    included_count: usize,
    excluded_count: usize,
) -> Result<i64, ValuationError> {
    let existing_sql = db.sql("SELECT id FROM valuation_snapshots WHERE input_sha256 = ?");
    let existing = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&existing_sql)
                .bind(input_sha256)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&existing_sql)
                .bind(input_sha256)
                .fetch_optional(pool)
                .await?
        }
    };
    if let Some(id) = existing {
        return Ok(id);
    }
    let insert_snapshot = db.sql(
        r#"
        INSERT INTO valuation_snapshots (
          capture_time, input_sha256, selection_policy_json,
          feature_schema_version, included_count, excluded_count
        ) VALUES (?, ?, ?, ?, ?, ?)
        RETURNING id
        "#,
    );
    let snapshot_id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&insert_snapshot)
                .bind(&policy.capture_time)
                .bind(input_sha256)
                .bind(policy_json)
                .bind(FEATURE_SCHEMA_VERSION as i64)
                .bind(included_count as i64)
                .bind(excluded_count as i64)
                .fetch_one(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&insert_snapshot)
                .bind(&policy.capture_time)
                .bind(input_sha256)
                .bind(policy_json)
                .bind(FEATURE_SCHEMA_VERSION as i32)
                .bind(included_count as i64)
                .bind(excluded_count as i64)
                .fetch_one(pool)
                .await?
        }
    };
    let insert_row = db.sql(
        r#"
        INSERT INTO valuation_snapshot_rows (
          snapshot_id, source_listing_id, duplicate_group_key, inclusion_flag,
          exclusion_reason, feature_json, target_price_usd, row_sha256
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    );
    for row in rows {
        match db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query(&insert_row)
                    .bind(snapshot_id)
                    .bind(row.listing_id)
                    .bind(&row.duplicate_group_key)
                    .bind(row.included)
                    .bind(&row.exclusion_reason)
                    .bind(&row.feature_json)
                    .bind(row.target_price_usd)
                    .bind(&row.row_sha256)
                    .execute(pool)
                    .await?;
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query(&insert_row)
                    .bind(snapshot_id)
                    .bind(row.listing_id)
                    .bind(&row.duplicate_group_key)
                    .bind(row.included)
                    .bind(&row.exclusion_reason)
                    .bind(&row.feature_json)
                    .bind(row.target_price_usd)
                    .bind(&row.row_sha256)
                    .execute(pool)
                    .await?;
            }
        }
    }
    Ok(snapshot_id)
}

fn duplicate_key(listing: &ListingRow) -> String {
    if let Some(serial) = listing
        .serial_number
        .as_deref()
        .map(normalize_identifier)
        .filter(|value| !value.is_empty())
    {
        return format!("serial:{serial}");
    }
    if let Some(registration) = listing
        .registration_number
        .as_deref()
        .map(normalize_identifier)
        .filter(|value| !value.is_empty())
    {
        return format!("registration:{registration}");
    }
    format!(
        "fingerprint:{}:{}:{}:{}:{:.0}:{:.0}:{:.0}",
        listing.manufacturer_id,
        listing.model_id,
        listing.variant_id,
        listing.model_year,
        listing.airframe_hours / 25.0,
        listing.engine_hours.unwrap_or(-1.0) / 25.0,
        listing.propeller_hours.unwrap_or(-1.0) / 25.0
    )
}

fn normalize_identifier(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}

fn finite_nonnegative(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn parse_year(value: &str) -> Option<i64> {
    value.get(0..4)?.parse().ok()
}

fn parse_date_days(value: &str) -> Option<i64> {
    let year = value.get(0..4)?.parse::<i64>().ok()?;
    let month = value.get(5..7)?.parse::<i64>().ok()?;
    let day = value.get(8..10)?.parse::<i64>().ok()?;
    if value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
    {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

pub fn current_capture_time() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}T00:00:00Z")
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::aircraft::faa::{parse_release, store_release, ReleaseMetadata, ReleaseReaders};

    use super::*;

    const FAA_AIRCRAFT_REFERENCE: &str = "CODE,MFR,MODEL,TYPE-ACFT,TYPE-ENG,AC-CAT,BUILD-CERT-IND,NO-ENG,NO-SEATS,AC-WEIGHT,SPEED,TC-DATA-SHEET,TC-DATA-HOLDER\n2072738,CESSNA AIRCRAFT CO,182T,4,1,1,0,01,004,CLASS 1,0145,3A13,TEXTRON AVIATION INC\n";
    const FAA_ENGINE_REFERENCE: &str =
        "CODE,MFR,MODEL,TYPE,HORSEPOWER,THRUST\n41528,LYCOMING,IO-540-AB1A5,1,00230,000000\n";

    async fn seed_faa_aircraft(db: &AppDb) {
        let master = "N-NUMBER,SERIAL NUMBER,MFR MDL CODE,ENG MFR MDL,YEAR MFR\n123,S123,2072738,41528,2010\n124,S124,2072738,41528,2011\n";
        let release = parse_release(
            ReleaseMetadata::official("2026-07-20", "a".repeat(64)),
            ReleaseReaders::new(
                Cursor::new(master),
                Cursor::new(FAA_AIRCRAFT_REFERENCE),
                Cursor::new(FAA_ENGINE_REFERENCE),
            ),
            ["N123", "N124"],
        )
        .unwrap();
        store_release(db, &release).await.unwrap();
    }

    async fn insert_listing(db: &AppDb) {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::query(
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES ('Test', 'TEST')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name) VALUES (1, 'Model', 'MODEL')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name) VALUES (1, 'Variant', 'VARIANT')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, is_verified, source_url,
              model_year, asking_price_usd, currency, added_at, status,
              ingestion_state, ingestion_completed_at,
              registration_number, serial_number, airframe_hours, engine_hours,
              propeller_hours
            ) VALUES (1, 1, FALSE, 'https://example.test/listing', 2010, 175000,
              'USD', '2026-07-01T00:00:00Z', 'active', 'ready',
              '2026-07-01T00:00:00Z', 'N123', 'S123', 1000, 500, 300)
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[test]
    fn date_round_trip_uses_unix_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(parse_date_days("2026-07-20T00:00:00Z"), Some(20_654));
    }

    #[test]
    fn high_confidence_fact_tokens_are_deterministic() {
        let fact = SourceBackedValuationFact {
            kind: "engine_conversion".to_string(),
            value: "Air Plains IO-550-D / 300 HP".to_string(),
            evidence_text: "Air Plains IO-550-D conversion".to_string(),
            source_url: Some("https://example.test/listing".to_string()),
            confidence: "high".to_string(),
        };
        assert_eq!(
            fact_token(&fact),
            "fact:engine_conversion:air_plains_io_550_d_300_hp"
        );
        assert_eq!(
            fact_token(&fact),
            equipment_feature_token("fact", &fact.kind, &fact.value)
        );
        assert_eq!(technical_field_count(true, false, true, false, true), 4);
    }

    #[tokio::test]
    async fn sqlite_snapshot_is_listing_only_and_round_trips() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        insert_listing(&db).await;
        seed_faa_aircraft(&db).await;
        let policy = SnapshotPolicy {
            capture_time: "2026-07-20T00:00:00Z".to_string(),
            max_listing_age_days: 60,
            ..SnapshotPolicy::default()
        };
        let report = create_snapshot(&db, &policy, true).await.unwrap();
        assert_eq!(report.included_count, 1);
        let rows = load_snapshot(&db, report.snapshot_id.unwrap())
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].asking_price_usd, 175_000.0);
        assert_eq!(rows[0].airframe_hours, Some(1000.0));
    }

    #[tokio::test]
    async fn snapshot_reports_faa_exclusions_and_rejects_contaminated_legacy_rows() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        insert_listing(&db).await;
        seed_faa_aircraft(&db).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        for (registration, serial) in [
            ("C-GABC", "FOREIGN"),
            ("N123", "CONFLICT"),
            ("N999", "UNCOVERED"),
        ] {
            sqlx::query(
                r#"
                INSERT INTO aircraft_sale_listings (
                  aircraft_model_variant_id, created_by_user_id, source_url,
                  model_year, asking_price_usd, currency, added_at, status,
                  ingestion_state, ingestion_completed_at, registration_number,
                  serial_number, airframe_hours
                ) VALUES (
                  1, 1, 'https://example.test/other', 2010, 175000, 'USD',
                  '2026-07-01T00:00:00Z', 'active', 'ready',
                  '2026-07-01T00:00:00Z', ?, ?, 1000
                )
                "#,
            )
            .bind(registration)
            .bind(serial)
            .execute(pool)
            .await
            .unwrap();
        }

        let policy = SnapshotPolicy {
            capture_time: "2026-07-20T00:00:00Z".to_string(),
            max_listing_age_days: 60,
            ..SnapshotPolicy::default()
        };
        let report = create_snapshot(&db, &policy, true).await.unwrap();
        assert_eq!(report.included_count, 1);
        assert_eq!(report.excluded_count, 3);
        assert_eq!(report.exclusions.get("faa_non_n_registration"), Some(&1));
        assert_eq!(report.exclusions.get("faa_serial_conflict"), Some(&1));
        assert_eq!(
            report.exclusions.get("faa_registration_not_covered"),
            Some(&1)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM aircraft_sale_listings")
                .fetch_one(pool)
                .await
                .unwrap(),
            4
        );

        let snapshot_id = report.snapshot_id.unwrap();
        sqlx::query(
            r#"
            UPDATE valuation_snapshot_rows
            SET inclusion_flag = TRUE, exclusion_reason = NULL
            WHERE snapshot_id = ? AND source_listing_id = 3
            "#,
        )
        .bind(snapshot_id)
        .execute(pool)
        .await
        .unwrap();
        let error = load_snapshot(&db, snapshot_id).await.unwrap_err();
        assert!(error.to_string().contains("FAA-ineligible"));
        assert!(error.to_string().contains("serial_conflict=1"));
        assert!(error
            .to_string()
            .contains("create a fresh valuation snapshot"));
    }

    #[tokio::test]
    async fn frozen_snapshot_rejects_an_eligible_to_eligible_identity_swap() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        insert_listing(&db).await;
        seed_faa_aircraft(&db).await;
        let policy = SnapshotPolicy {
            capture_time: "2026-07-20T00:00:00Z".to_string(),
            max_listing_age_days: 60,
            ..SnapshotPolicy::default()
        };
        let report = create_snapshot(&db, &policy, true).await.unwrap();
        let snapshot_id = report.snapshot_id.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::query(
            "UPDATE aircraft_sale_listings SET registration_number = 'N124', serial_number = 'S124' WHERE id = 1",
        )
        .execute(pool)
        .await
        .unwrap();

        let error = load_snapshot(&db, snapshot_id).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("FAA admission identity or provenance changed"));
    }

    #[tokio::test]
    async fn legacy_unreviewed_avionics_do_not_become_training_features() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        insert_listing(&db).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let manufacturer_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Garmin', 'garmin') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let type_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Transponder', 'transponder') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let approved_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text, identity_evidence_kind,
              identity_confidence, catalog_reviewed_at
            ) VALUES (
              ?, 'GTX 345R', 'gtx345r',
              'manufacturer_part_number', '011-03378-40', '0110337840',
              'https://static.garmin.com/manuals/gtx345r.pdf',
              'GTX 345R installation manual',
              'The manufacturer manual identifies the GTX 345R and its part number.',
              'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
            ) RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let unreviewed_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name
            ) VALUES (?, 'Legacy Guess', 'legacy guess')
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        for model_id in [approved_id, unreviewed_id] {
            sqlx::query(
                "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
            )
            .bind(model_id)
            .bind(type_id)
            .execute(pool)
            .await
            .unwrap();
        }
        let gps_type_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('GPS', 'gps') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(approved_id)
        .bind(gps_type_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?")
            .bind(approved_id)
            .execute(pool)
            .await
            .unwrap();

        // A migrated database can legitimately contain associations created
        // before the approved-only trigger existed.
        sqlx::query("DROP TRIGGER aircraft_sale_listing_avionics_approved_insert")
            .execute(pool)
            .await
            .unwrap();
        for model_id in [approved_id, unreviewed_id] {
            sqlx::query(
                r#"
                INSERT INTO aircraft_sale_listing_avionics (
                  aircraft_sale_listing_id, avionics_model_id, source_confidence
                ) VALUES (1, ?, 'high')
                "#,
            )
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        }

        let equipment = load_equipment(&db).await.unwrap();
        assert_eq!(
            equipment.get(&1),
            Some(&vec!["avionics:garmin:gtx_345r".to_string()])
        );
    }
}
