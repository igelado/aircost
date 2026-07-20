use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};

use super::types::{ComponentObservation, ComponentTimeBasis, TrainingListing, ValuationError};
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
    registration_number: Option<String>,
    serial_number: Option<String>,
    airframe_hours: f64,
    engine_hours: f64,
    propeller_hours: f64,
}

#[derive(Debug, FromRow)]
struct EquipmentRow {
    listing_id: i64,
    manufacturer: String,
    model: String,
    quantity: i64,
}

#[derive(Debug, FromRow)]
struct SnapshotDbRow {
    id: i64,
    capture_time: String,
}

#[derive(Debug, FromRow)]
struct FrozenDbRow {
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
    let equipment = load_equipment(db).await?;
    let mut prepared = Vec::with_capacity(listings.len());
    for listing in listings {
        let tokens = equipment.get(&listing.id).cloned().unwrap_or_default();
        let duplicate_group_key = duplicate_key(&listing);
        let reason = exclusion_reason(&listing, policy, capture_day, snapshot_year);
        let technical_field_count = 3
            + u32::from(listing.registration_number.is_some())
            + u32::from(listing.serial_number.is_some())
            + u32::from(!tokens.is_empty());
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
            engine_times: vec![ComponentObservation {
                time_hours: finite_nonnegative(listing.engine_hours),
                basis: ComponentTimeBasis::Unknown,
                count: 1,
            }],
            propeller_times: vec![ComponentObservation {
                time_hours: finite_nonnegative(listing.propeller_hours),
                basis: ComponentTimeBasis::Unknown,
                count: 1,
            }],
            equipment_tokens: tokens,
            technical_field_count,
        };
        let feature_json = serde_json::to_string(&training)?;
        let target = finite_positive(listing.asking_price_usd);
        let row_sha256 = sha256_hex(
            format!(
                "{}\n{}\n{}\n{}",
                listing.id,
                duplicate_group_key,
                feature_json,
                target.map(|value| value.to_string()).unwrap_or_default()
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
    let policy_json = serde_json::to_string(policy)?;
    let mut snapshot_material = format!("{policy_json}\n{FEATURE_SCHEMA_VERSION}\n");
    for row in &prepared {
        snapshot_material.push_str(&format!(
            "{}|{}|{}|{}\n",
            row.listing_id, row.duplicate_group_key, row.included, row.row_sha256
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
    let sql = db.sql(
        r#"
        SELECT feature_json
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
        .map(|row| serde_json::from_str(&row.feature_json).map_err(ValuationError::from))
        .collect()
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
          listing.registration_number,
          listing.serial_number,
          listing.airframe_hours,
          listing.engine_hours,
          listing.propeller_hours
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

async fn load_equipment(db: &AppDb) -> Result<BTreeMap<i64, Vec<String>>, ValuationError> {
    let sql = r#"
        SELECT
          link.aircraft_sale_listing_id AS listing_id,
          manufacturer.name AS manufacturer,
          model.name AS model,
          link.quantity
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model ON model.id = link.avionics_model_id
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        WHERE link.source = 'listing'
        ORDER BY link.aircraft_sale_listing_id, manufacturer.name, model.name
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
        let token = format!(
            "{}:{}",
            normalize_identifier(&row.manufacturer),
            normalize_identifier(&row.model)
        );
        for _ in 0..row.quantity.max(1) {
            equipment
                .entry(row.listing_id)
                .or_insert_with(Vec::new)
                .push(token.clone());
        }
    }
    Ok(equipment)
}

fn exclusion_reason(
    listing: &ListingRow,
    policy: &SnapshotPolicy,
    capture_day: i64,
    snapshot_year: i64,
) -> Option<String> {
    if listing.status != "active" {
        return Some("inactive".to_string());
    }
    if listing.currency != "USD"
        || !listing.asking_price_usd.is_finite()
        || listing.asking_price_usd <= 0.0
    {
        return Some("non_usd_or_nonpositive_price".to_string());
    }
    if listing.model_year < policy.minimum_model_year
        || listing.model_year > snapshot_year
        || [
            listing.airframe_hours,
            listing.engine_hours,
            listing.propeller_hours,
        ]
        .iter()
        .any(|hours| !hours.is_finite() || *hours < 0.0)
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
        listing.engine_hours / 25.0,
        listing.propeller_hours / 25.0
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

fn finite_positive(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
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
    use super::*;

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
              registration_number, serial_number, airframe_hours, engine_hours,
              propeller_hours
            ) VALUES (1, 1, FALSE, 'https://example.test/listing', 2010, 175000,
              'USD', '2026-07-01T00:00:00Z', 'active', 'N123', 'S123', 1000, 500, 300)
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

    #[tokio::test]
    async fn sqlite_snapshot_is_listing_only_and_round_trips() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        insert_listing(&db).await;
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
}
