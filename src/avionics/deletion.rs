//! Explicit removal of one catalog product and every listing occurrence that
//! resolves to it.
//!
//! Deletion is deliberately stricter than consolidation. It never remaps a
//! source occurrence to a different product and never guesses through a
//! replacement graph. Exact source coordinates are instead recorded as
//! discarded terminal dispositions, so replay cannot recreate the deleted
//! product or its listing associations.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;

use crate::avionics::fingerprint::{
    catalog_products, fingerprint_catalog_products, CatalogFingerprintRow,
    APPROVED_CATALOG_ROWS_SQL,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::listing::avionics::disposition::{
    coordinates_from_aspect_id, extraction_sha256, occurrence_fingerprint, OccurrenceRole,
    DISPOSITION_POLICY_VERSION, INSERT_DISPOSITION_SQL,
};
use crate::listing::review::{
    parse_current_pending_review_aspects, serialize_review_payload, ListingAssociationRole,
    PendingReviewAspect,
};
use crate::normalize::{normalize_avionics_identifier, normalize_avionics_manufacturer_name};

const DELETION_REASON_CODE: &str = "catalog_product_deleted";
const DELETION_DECISION_REASON: &str =
    "A reviewer deleted the catalog product and discarded this exact source occurrence.";

#[derive(Debug)]
pub enum AvionicsProductDeletionError {
    Validation(String),
    NotFound(String),
    Conflict(String),
    Database(String),
}

impl fmt::Display for AvionicsProductDeletionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message)
            | Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Database(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AvionicsProductDeletionError {}

impl From<sqlx::Error> for AvionicsProductDeletionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AvionicsProductDeletion {
    pub deleted_product_id: i64,
    pub deleted_product_name: String,
    pub affected_listing_count: usize,
    pub affected_listing_ids: Vec<i64>,
    pub deleted_listing_association_count: u64,
    pub discarded_occurrence_count: usize,
    pub removed_pending_aspect_count: usize,
    pub deleted_suite_membership_count: u64,
}

#[derive(Debug, FromRow)]
struct ProductRow {
    id: i64,
    manufacturer: String,
    model: String,
    avionics_manufacturer_id: i64,
}

#[derive(Debug, FromRow)]
struct ManufacturerNameRow {
    name: String,
}

#[derive(Clone, Debug, FromRow)]
struct SubmissionRow {
    id: i64,
    canonical_listing_id: i64,
    extracted_listing_json: String,
}

#[derive(Clone, Debug, FromRow)]
struct ReviewRow {
    listing_id: i64,
    plugin_submission_id: Option<i64>,
    review_payload_json: String,
    review_payload_sha256: String,
    pending_aspect_count: i64,
}

#[derive(Debug, FromRow)]
struct ListingLinkRow {
    id: i64,
    aircraft_sale_listing_id: i64,
    avionics_model_id: i64,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, FromRow, PartialEq)]
struct ExistingDispositionRow {
    outcome: String,
    avionics_model_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct OccurrenceCoordinate {
    listing_id: i64,
    submission_id: i64,
    extraction_sha256: String,
    occurrence_index: usize,
    occurrence_role: OccurrenceRole,
}

#[derive(Debug)]
enum ReviewMutation {
    Delete {
        listing_id: i64,
    },
    Update {
        listing_id: i64,
        review_payload_json: String,
        review_payload_sha256: String,
        extraction_sha256: String,
        pending_aspect_count: i64,
    },
}

#[derive(Debug)]
struct DeletionPlan {
    coordinates: BTreeSet<OccurrenceCoordinate>,
    review_mutations: Vec<ReviewMutation>,
    affected_listing_ids: BTreeSet<i64>,
    removed_pending_aspect_count: usize,
}

pub async fn delete_avionics_product(
    db: &AppDb,
    reviewer_user_id: i64,
    avionics_model_id: i64,
) -> Result<AvionicsProductDeletion, AvionicsProductDeletionError> {
    if reviewer_user_id <= 0 {
        return Err(AvionicsProductDeletionError::Validation(
            "avionics product deletion requires a valid reviewer".to_string(),
        ));
    }
    if avionics_model_id <= 0 {
        return Err(AvionicsProductDeletionError::Validation(
            "avionics product id must be positive".to_string(),
        ));
    }

    let sqlite_lock_sql = db.sql("UPDATE avionics_models SET updated_at = updated_at WHERE id = ?");
    let postgres_lock_sql = db.sql(
        "LOCK TABLE avionics_models, avionics_model_types, avionics_approved_product_identities, avionics_catalog_product_deletion_guards, aircraft_reference_avionics, aircraft_sale_listings, aircraft_sale_listing_avionics, aircraft_sale_listing_avionics_dispositions, aircraft_sale_listing_pending_reviews, plugin_submissions, avionics_suite_components, avionics_catalog_consolidation_guard, avionics_catalog_grounded_consolidation_authorizations, avionics_catalog_grounded_consolidation_guard, avionics_catalog_grounded_consolidation_claim, avionics_catalog_human_consolidation_guard, avionics_catalog_human_consolidation_claim IN SHARE ROW EXCLUSIVE MODE",
    );
    let product_sql = db.sql(
        r#"
        SELECT model.id, manufacturer.name AS manufacturer, model.name AS model,
               model.avionics_manufacturer_id
        FROM avionics_models model
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        WHERE model.id = ?
        "#,
    );
    let manufacturer_names_sql = db.sql(
        r#"
        SELECT DISTINCT member_manufacturer.name
        FROM avionics_manufacturer_effective_memberships target
        JOIN avionics_manufacturer_effective_memberships member
          ON member.avionics_manufacturer_identity_id =
             target.avionics_manufacturer_identity_id
        JOIN avionics_manufacturers member_manufacturer
          ON member_manufacturer.id = member.avionics_manufacturer_id
        WHERE target.avionics_manufacturer_id = ?
        UNION
        SELECT identity.canonical_name
        FROM avionics_manufacturer_effective_memberships target
        JOIN avionics_manufacturer_identities identity
          ON identity.id = target.avionics_manufacturer_identity_id
        WHERE target.avionics_manufacturer_id = ?
        "#,
    );
    let reference_count_sql =
        db.sql("SELECT COUNT(*) FROM aircraft_reference_avionics WHERE avionics_model_id = ?");
    let consolidation_survivor_count_sql = db.sql(
        r#"
        SELECT
          (SELECT COUNT(*) FROM avionics_catalog_consolidation_guard
           WHERE survivor_model_id = ?) +
          (SELECT COUNT(*) FROM avionics_catalog_grounded_consolidation_guard
           WHERE survivor_model_id = ?) +
          (SELECT COUNT(*) FROM avionics_catalog_human_consolidation_guard
           WHERE survivor_model_id = ?) +
          (SELECT COUNT(*) FROM avionics_catalog_grounded_consolidation_authorizations
           WHERE survivor_model_id = ?) +
          (SELECT COUNT(*) FROM avionics_catalog_grounded_consolidation_claim
           WHERE survivor_model_id = ?) +
          (SELECT COUNT(*) FROM avionics_catalog_human_consolidation_claim
           WHERE survivor_model_id = ?)
        "#,
    );
    let submissions_sql = db.sql(
        r#"
        SELECT id, canonical_listing_id, extracted_listing_json
        FROM plugin_submissions
        WHERE canonical_listing_id IS NOT NULL
          AND extracted_listing_json IS NOT NULL
        ORDER BY id
        "#,
    );
    let reviews_sql = db.sql(
        r#"
        SELECT listing_id, plugin_submission_id, review_payload_json,
               review_payload_sha256, pending_aspect_count
        FROM aircraft_sale_listing_pending_reviews
        ORDER BY listing_id
        "#,
    );
    let links_sql = db.sql(
        r#"
        SELECT id, aircraft_sale_listing_id, avionics_model_id,
               configuration_action, replaces_avionics_model_id
        FROM aircraft_sale_listing_avionics
        WHERE avionics_model_id = ? OR replaces_avionics_model_id = ?
        ORDER BY aircraft_sale_listing_id, id
        "#,
    );
    let target_disposition_coordinates_sql = db.sql(
        r#"
        SELECT aircraft_sale_listing_id, plugin_submission_id,
               extraction_sha256, occurrence_index, occurrence_role
        FROM aircraft_sale_listing_avionics_dispositions
        WHERE avionics_model_id = ?
        ORDER BY plugin_submission_id, occurrence_index, occurrence_role
        "#,
    );
    let existing_disposition_sql = db.sql(
        r#"
        SELECT outcome, avionics_model_id
        FROM aircraft_sale_listing_avionics_dispositions
        WHERE plugin_submission_id = ? AND extraction_sha256 = ?
          AND occurrence_index = ? AND occurrence_role = ?
        "#,
    );
    let delete_target_dispositions_sql = db
        .sql("DELETE FROM aircraft_sale_listing_avionics_dispositions WHERE avionics_model_id = ?");
    let delete_target_links_sql =
        db.sql("DELETE FROM aircraft_sale_listing_avionics WHERE avionics_model_id = ?");
    let delete_suite_memberships_sql = db.sql(
        "DELETE FROM avionics_suite_components WHERE suite_model_id = ? OR component_model_id = ?",
    );
    let delete_review_sql =
        db.sql("DELETE FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?");
    let update_review_sql = db.sql(
        r#"
        UPDATE aircraft_sale_listing_pending_reviews
        SET extraction_sha256 = ?, review_payload_json = ?,
            review_payload_sha256 = ?, pending_aspect_count = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE listing_id = ?
        "#,
    );
    let insert_guard_sql = db.sql(
        r#"
        INSERT INTO avionics_catalog_product_deletion_guards (
          avionics_model_id, requested_by_user_id
        ) VALUES (?, ?)
        "#,
    );
    let delete_product_sql = db.sql("DELETE FROM avionics_models WHERE id = ?");
    let delete_guard_sql =
        db.sql("DELETE FROM avionics_catalog_product_deletion_guards WHERE avionics_model_id = ?");
    let guard_count_sql = db.sql(
        "SELECT COUNT(*) FROM avionics_catalog_product_deletion_guards WHERE avionics_model_id = ?",
    );
    let update_catalog_revision_sql = db.sql(
        r#"
        UPDATE aircraft_sale_listing_pending_reviews
        SET catalog_revision_sha256 = ?, updated_at = CURRENT_TIMESTAMP
        WHERE catalog_revision_sha256 <> ?
        "#,
    );
    let invalidate_listing_sql = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'incomplete',
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
    );
    let finalize_listing_state_sql = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = CASE WHEN EXISTS (
              SELECT 1 FROM aircraft_sale_listing_pending_reviews review
              WHERE review.listing_id = aircraft_sale_listings.id
            ) THEN 'pending_review' ELSE 'incomplete' END,
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
    );
    let catalog_rows_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let insert_disposition_sql = db.sql(INSERT_DISPOSITION_SQL);

    macro_rules! delete_in_transaction {
        ($pool:expr, $lock_sql:expr, $bind_lock:expr) => {{
            let mut transaction = $pool.begin().await?;
            let mut lock = sqlx::query(&$lock_sql);
            if $bind_lock {
                lock = lock.bind(avionics_model_id);
            }
            lock.execute(&mut *transaction).await?;

            let product = sqlx::query_as::<_, ProductRow>(&product_sql)
                .bind(avionics_model_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    AvionicsProductDeletionError::NotFound(format!(
                        "avionics catalog product {avionics_model_id} was not found"
                    ))
                })?;
            let mut manufacturer_names = sqlx::query_as::<_, ManufacturerNameRow>(
                &manufacturer_names_sql,
            )
            .bind(product.avionics_manufacturer_id)
            .bind(product.avionics_manufacturer_id)
            .fetch_all(&mut *transaction)
            .await?
            .into_iter()
            .map(|row| normalize_avionics_manufacturer_name(&row.name))
            .filter(|name| !name.is_empty())
            .collect::<HashSet<_>>();
            manufacturer_names.insert(normalize_avionics_manufacturer_name(
                &product.manufacturer,
            ));

            let reference_count: i64 = sqlx::query_scalar(&reference_count_sql)
                .bind(avionics_model_id)
                .fetch_one(&mut *transaction)
                .await?;
            if reference_count != 0 {
                return Err(AvionicsProductDeletionError::Conflict(format!(
                    "avionics catalog product {avionics_model_id} is used by {reference_count} aircraft reference configuration facts; remove or replace those curated facts explicitly first"
                )));
            }
            let consolidation_survivor_count: i64 =
                sqlx::query_scalar(&consolidation_survivor_count_sql)
                    .bind(avionics_model_id)
                    .bind(avionics_model_id)
                    .bind(avionics_model_id)
                    .bind(avionics_model_id)
                    .bind(avionics_model_id)
                    .bind(avionics_model_id)
                    .fetch_one(&mut *transaction)
                    .await?;
            if consolidation_survivor_count != 0 {
                return Err(AvionicsProductDeletionError::Conflict(format!(
                    "avionics catalog product {avionics_model_id} is a consolidation survivor; resolve its consolidation dependencies explicitly first"
                )));
            }

            let submissions = sqlx::query_as::<_, SubmissionRow>(&submissions_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let submission_by_id = submissions
                .iter()
                .map(|submission| (submission.id, submission.clone()))
                .collect::<HashMap<_, _>>();
            let mut exact_coordinates = BTreeSet::new();
            for submission in &submissions {
                exact_coordinates.extend(exact_product_coordinates(
                    submission,
                    &product.model,
                    &manufacturer_names,
                ));
            }

            let reviews = sqlx::query_as::<_, ReviewRow>(&reviews_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let mut plan = plan_review_mutations(
                reviews,
                &submission_by_id,
                exact_coordinates,
                avionics_model_id,
                &product,
                &manufacturer_names,
            )?;

            let links = sqlx::query_as::<_, ListingLinkRow>(&links_sql)
                .bind(avionics_model_id)
                .bind(avionics_model_id)
                .fetch_all(&mut *transaction)
                .await?;
            validate_independent_listing_links(&links, avionics_model_id)?;
            plan.affected_listing_ids.extend(
                links.iter().map(|link| link.aircraft_sale_listing_id),
            );

            let target_disposition_rows = sqlx::query(&target_disposition_coordinates_sql)
                .bind(avionics_model_id)
                .fetch_all(&mut *transaction)
                .await?;
            for row in target_disposition_rows {
                use sqlx::Row;
                let role: String = row.try_get("occurrence_role")?;
                let occurrence_role = parse_occurrence_role(&role)?;
                let occurrence_index: i64 = row.try_get("occurrence_index")?;
                let coordinate = OccurrenceCoordinate {
                    listing_id: row.try_get("aircraft_sale_listing_id")?,
                    submission_id: row.try_get("plugin_submission_id")?,
                    extraction_sha256: row.try_get("extraction_sha256")?,
                    occurrence_index: usize::try_from(occurrence_index).map_err(|_| {
                        AvionicsProductDeletionError::Conflict(
                            "stored avionics disposition has an invalid occurrence index"
                                .to_string(),
                        )
                    })?,
                    occurrence_role,
                };
                plan.affected_listing_ids.insert(coordinate.listing_id);
                plan.coordinates.insert(coordinate);
            }

            for coordinate in &plan.coordinates {
                let existing = sqlx::query_as::<_, ExistingDispositionRow>(
                    &existing_disposition_sql,
                )
                .bind(coordinate.submission_id)
                .bind(&coordinate.extraction_sha256)
                .bind(coordinate.occurrence_index as i64)
                .bind(coordinate.occurrence_role.as_str())
                .fetch_optional(&mut *transaction)
                .await?;
                if let Some(existing) = existing {
                    let target_link = existing.outcome == "linked"
                        && existing.avionics_model_id == Some(avionics_model_id);
                    let already_discarded = existing.outcome == "discarded"
                        && existing.avionics_model_id.is_none();
                    if !target_link && !already_discarded {
                        return Err(AvionicsProductDeletionError::Conflict(format!(
                            "source occurrence {}:{}:{} is already linked to a different avionics product",
                            coordinate.submission_id,
                            coordinate.occurrence_index,
                            coordinate.occurrence_role.as_str(),
                        )));
                    }
                }
            }

            // Listing avionics are immutable while their listing is ready or
            // verified. Invalidate every affected listing before removing any
            // link; the final state is derived from its surviving review below.
            for listing_id in &plan.affected_listing_ids {
                sqlx::query(&invalidate_listing_sql)
                    .bind(listing_id)
                    .execute(&mut *transaction)
                    .await?;
            }

            sqlx::query(&delete_target_dispositions_sql)
                .bind(avionics_model_id)
                .execute(&mut *transaction)
                .await?;
            let deleted_listing_association_count = sqlx::query(&delete_target_links_sql)
                .bind(avionics_model_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            let deleted_suite_membership_count =
                sqlx::query(&delete_suite_memberships_sql)
                    .bind(avionics_model_id)
                    .bind(avionics_model_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();

            for mutation in &plan.review_mutations {
                match mutation {
                    ReviewMutation::Delete { listing_id } => {
                        sqlx::query(&delete_review_sql)
                            .bind(listing_id)
                            .execute(&mut *transaction)
                            .await?;
                    }
                    ReviewMutation::Update {
                        listing_id,
                        review_payload_json,
                        review_payload_sha256,
                        extraction_sha256,
                        pending_aspect_count,
                    } => {
                        sqlx::query(&update_review_sql)
                            .bind(extraction_sha256)
                            .bind(review_payload_json)
                            .bind(review_payload_sha256)
                            .bind(pending_aspect_count)
                            .bind(listing_id)
                            .execute(&mut *transaction)
                            .await?;
                    }
                }
            }

            for coordinate in &plan.coordinates {
                let fingerprint = occurrence_fingerprint(
                    &coordinate.extraction_sha256,
                    coordinate.occurrence_index,
                    coordinate.occurrence_role,
                )
                .map_err(AvionicsProductDeletionError::Conflict)?;
                sqlx::query(&insert_disposition_sql)
                    .bind(coordinate.listing_id)
                    .bind(coordinate.submission_id)
                    .bind(&coordinate.extraction_sha256)
                    .bind(coordinate.occurrence_index as i64)
                    .bind(coordinate.occurrence_role.as_str())
                    .bind(fingerprint)
                    .bind("discarded")
                    .bind(Option::<i64>::None)
                    .bind(DELETION_REASON_CODE)
                    .bind(DELETION_DECISION_REASON)
                    .bind("manual")
                    .bind(reviewer_user_id)
                    .bind(DISPOSITION_POLICY_VERSION)
                    .execute(&mut *transaction)
                    .await?;
            }

            sqlx::query(&insert_guard_sql)
                .bind(avionics_model_id)
                .bind(reviewer_user_id)
                .execute(&mut *transaction)
                .await?;
            let deleted = sqlx::query(&delete_product_sql)
                .bind(avionics_model_id)
                .execute(&mut *transaction)
                .await?;
            if deleted.rows_affected() != 1 {
                return Err(AvionicsProductDeletionError::Conflict(format!(
                    "avionics catalog product {avionics_model_id} changed before deletion"
                )));
            }
            sqlx::query(&delete_guard_sql)
                .bind(avionics_model_id)
                .execute(&mut *transaction)
                .await?;
            let remaining_guard_count: i64 = sqlx::query_scalar(&guard_count_sql)
                .bind(avionics_model_id)
                .fetch_one(&mut *transaction)
                .await?;
            if remaining_guard_count != 0 {
                return Err(AvionicsProductDeletionError::Conflict(format!(
                    "avionics catalog product {avionics_model_id} deletion authorization was not cleaned up"
                )));
            }

            let catalog_rows = sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_rows_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let catalog_revision = fingerprint_catalog_products(&catalog_products(catalog_rows));
            sqlx::query(&update_catalog_revision_sql)
                .bind(&catalog_revision)
                .bind(&catalog_revision)
                .execute(&mut *transaction)
                .await?;

            for listing_id in &plan.affected_listing_ids {
                sqlx::query(&finalize_listing_state_sql)
                    .bind(listing_id)
                    .execute(&mut *transaction)
                    .await?;
            }

            let affected_listing_ids = plan
                .affected_listing_ids
                .iter()
                .copied()
                .collect::<Vec<_>>();
            let result = AvionicsProductDeletion {
                deleted_product_id: product.id,
                deleted_product_name: format!("{} {}", product.manufacturer, product.model),
                affected_listing_count: affected_listing_ids.len(),
                affected_listing_ids,
                deleted_listing_association_count,
                discarded_occurrence_count: plan.coordinates.len(),
                removed_pending_aspect_count: plan.removed_pending_aspect_count,
                deleted_suite_membership_count,
            };
            transaction.commit().await?;
            Ok(result)
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            delete_in_transaction!(pool, sqlite_lock_sql, true)
        }
        DatabaseBackend::Postgres(pool) => {
            delete_in_transaction!(pool, postgres_lock_sql, false)
        }
    }
}

fn exact_product_coordinates(
    submission: &SubmissionRow,
    product_model: &str,
    manufacturer_names: &HashSet<String>,
) -> Vec<OccurrenceCoordinate> {
    let Ok(value) = serde_json::from_str::<Value>(&submission.extracted_listing_json) else {
        return Vec::new();
    };
    let Some(avionics) = value.get("avionics").and_then(Value::as_array) else {
        return Vec::new();
    };
    let extraction_hash = extraction_sha256(&submission.extracted_listing_json);
    let mut coordinates = Vec::new();
    for (index, occurrence) in avionics.iter().enumerate() {
        if identity_is_exact(
            occurrence.get("manufacturer").and_then(Value::as_str),
            occurrence.get("model").and_then(Value::as_str),
            product_model,
            manufacturer_names,
        ) {
            coordinates.push(OccurrenceCoordinate {
                listing_id: submission.canonical_listing_id,
                submission_id: submission.id,
                extraction_sha256: extraction_hash.clone(),
                occurrence_index: index,
                occurrence_role: OccurrenceRole::Primary,
            });
        }
        if let Some(replacement) = occurrence.get("replaces") {
            if identity_is_exact(
                replacement.get("manufacturer").and_then(Value::as_str),
                replacement.get("model").and_then(Value::as_str),
                product_model,
                manufacturer_names,
            ) {
                coordinates.push(OccurrenceCoordinate {
                    listing_id: submission.canonical_listing_id,
                    submission_id: submission.id,
                    extraction_sha256: extraction_hash.clone(),
                    occurrence_index: index,
                    occurrence_role: OccurrenceRole::Replacement,
                });
            }
        }
    }
    coordinates
}

fn identity_is_exact(
    observed_manufacturer: Option<&str>,
    observed_model: Option<&str>,
    product_model: &str,
    manufacturer_names: &HashSet<String>,
) -> bool {
    let Some(observed_model) = observed_model else {
        return false;
    };
    if normalize_avionics_identifier(observed_model) != normalize_avionics_identifier(product_model)
    {
        return false;
    }
    observed_manufacturer
        .map(normalize_avionics_manufacturer_name)
        .filter(|manufacturer| !manufacturer.is_empty())
        .is_none_or(|manufacturer| manufacturer_names.contains(&manufacturer))
}

fn plan_review_mutations(
    reviews: Vec<ReviewRow>,
    submission_by_id: &HashMap<i64, SubmissionRow>,
    exact_coordinates: BTreeSet<OccurrenceCoordinate>,
    avionics_model_id: i64,
    product: &ProductRow,
    manufacturer_names: &HashSet<String>,
) -> Result<DeletionPlan, AvionicsProductDeletionError> {
    let mut plan = DeletionPlan {
        coordinates: exact_coordinates,
        review_mutations: Vec::new(),
        affected_listing_ids: BTreeSet::new(),
        removed_pending_aspect_count: 0,
    };

    for review in reviews {
        let aspects = parse_current_pending_review_aspects(
            &review.review_payload_json,
            &review.review_payload_sha256,
            review.pending_aspect_count,
        )
        .map_err(|error| {
            AvionicsProductDeletionError::Conflict(format!(
                "listing {} has an invalid pending avionics review: {error}",
                review.listing_id
            ))
        })?;
        let current_submission = review
            .plugin_submission_id
            .and_then(|id| submission_by_id.get(&id));
        let exact_for_review = current_submission
            .map(|submission| {
                exact_product_coordinates(submission, &product.model, manufacturer_names)
                    .into_iter()
                    .map(|coordinate| {
                        (
                            (coordinate.occurrence_index, coordinate.occurrence_role),
                            coordinate,
                        )
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();

        let mut remove_ids = HashSet::new();
        let mut removed_coordinates = Vec::new();
        for aspect in &aspects {
            let aspect_coordinate = coordinates_from_aspect_id(&aspect.id);
            let exact_coordinate =
                aspect_coordinate.and_then(|coordinate| exact_for_review.get(&coordinate));
            let primary_target = aspect_primary_references_product(aspect, avionics_model_id)
                || exact_coordinate.is_some_and(|coordinate| {
                    coordinate.occurrence_role == OccurrenceRole::Primary
                });
            let replacement_target =
                aspect_replacement_references_product(aspect, avionics_model_id)
                    || exact_coordinate.is_some_and(|coordinate| {
                        coordinate.occurrence_role == OccurrenceRole::Replacement
                    });
            if replacement_target && !primary_target {
                return Err(complex_review_conflict(
                    review.listing_id,
                    aspect,
                    avionics_model_id,
                    "the deleted product is the replacement side of another product action",
                ));
            }
            if primary_target {
                validate_independent_review_aspect(review.listing_id, aspect, avionics_model_id)?;
                let Some((index, role)) = aspect_coordinate else {
                    return Err(complex_review_conflict(
                        review.listing_id,
                        aspect,
                        avionics_model_id,
                        "the aspect is not bound to a current source occurrence",
                    ));
                };
                let Some(submission) = current_submission else {
                    return Err(complex_review_conflict(
                        review.listing_id,
                        aspect,
                        avionics_model_id,
                        "the review has no retained source submission",
                    ));
                };
                let coordinate = OccurrenceCoordinate {
                    listing_id: review.listing_id,
                    submission_id: submission.id,
                    extraction_sha256: extraction_sha256(&submission.extracted_listing_json),
                    occurrence_index: index,
                    occurrence_role: role,
                };
                remove_ids.insert(aspect.id.clone());
                removed_coordinates.push(coordinate);
            }
        }

        if remove_ids.is_empty() {
            continue;
        }
        for aspect in &aspects {
            if aspect
                .replacement_aspect_id
                .as_ref()
                .is_some_and(|id| remove_ids.contains(id))
                && !remove_ids.contains(&aspect.id)
            {
                return Err(complex_review_conflict(
                    review.listing_id,
                    aspect,
                    avionics_model_id,
                    "another pending aspect depends on the product aspect",
                ));
            }
        }

        let retained = aspects
            .into_iter()
            .filter(|aspect| !remove_ids.contains(&aspect.id))
            .collect::<Vec<_>>();
        plan.removed_pending_aspect_count += remove_ids.len();
        plan.affected_listing_ids.insert(review.listing_id);
        plan.coordinates.extend(removed_coordinates);
        if retained.is_empty() {
            plan.review_mutations.push(ReviewMutation::Delete {
                listing_id: review.listing_id,
            });
        } else {
            let serialized = serialize_review_payload(&retained).map_err(|error| {
                AvionicsProductDeletionError::Conflict(format!(
                    "listing {} review could not be rebuilt after product deletion: {error}",
                    review.listing_id
                ))
            })?;
            plan.review_mutations.push(ReviewMutation::Update {
                listing_id: review.listing_id,
                review_payload_json: serialized.review_payload_json,
                review_payload_sha256: serialized.review_payload_sha256,
                extraction_sha256: serialized.extraction_sha256,
                pending_aspect_count: serialized.pending_aspect_count,
            });
        }
    }

    for coordinate in &plan.coordinates {
        plan.affected_listing_ids.insert(coordinate.listing_id);
    }
    Ok(plan)
}

fn aspect_primary_references_product(aspect: &PendingReviewAspect, product_id: i64) -> bool {
    aspect
        .suggested_product
        .as_ref()
        .and_then(|product| product.id)
        == Some(product_id)
        || aspect
            .proposed_product
            .as_ref()
            .and_then(|product| product.id)
            == Some(product_id)
        || aspect.reuse_attestation_target_id == Some(product_id)
        || aspect.covered_associations.iter().any(|association| {
            association.avionics_model_id == product_id
                && association.role == ListingAssociationRole::Installed
        })
        || aspect
            .reviewer_correction_association_binding
            .as_ref()
            .is_some_and(|binding| binding.avionics_model_id == product_id)
}

fn aspect_replacement_references_product(aspect: &PendingReviewAspect, product_id: i64) -> bool {
    aspect.replaces_product_id == Some(product_id)
        || aspect.covered_associations.iter().any(|association| {
            association.avionics_model_id == product_id
                && association.role == ListingAssociationRole::Replacement
        })
        || aspect
            .reviewer_correction_association_binding
            .as_ref()
            .is_some_and(|binding| binding.replaces_avionics_model_id == Some(product_id))
}

fn validate_independent_review_aspect(
    listing_id: i64,
    aspect: &PendingReviewAspect,
    product_id: i64,
) -> Result<(), AvionicsProductDeletionError> {
    let has_other_association = aspect
        .covered_associations
        .iter()
        .any(|association| association.avionics_model_id != product_id);
    let correction_has_partner = aspect
        .reviewer_correction_association_binding
        .as_ref()
        .is_some_and(|binding| binding.replaces_avionics_model_id.is_some());
    if aspect.configuration_action != "installed"
        || aspect.replaces_product_id.is_some()
        || aspect.replacement_aspect_id.is_some()
        || has_other_association
        || correction_has_partner
    {
        return Err(complex_review_conflict(
            listing_id,
            aspect,
            product_id,
            "the product participates in a replacement or multi-product action",
        ));
    }
    Ok(())
}

fn complex_review_conflict(
    listing_id: i64,
    aspect: &PendingReviewAspect,
    product_id: i64,
    reason: &str,
) -> AvionicsProductDeletionError {
    AvionicsProductDeletionError::Conflict(format!(
        "cannot delete avionics catalog product {product_id}: listing {listing_id} review aspect {} is not an independent occurrence ({reason}); resolve that aspect explicitly first",
        aspect.id
    ))
}

fn validate_independent_listing_links(
    links: &[ListingLinkRow],
    product_id: i64,
) -> Result<(), AvionicsProductDeletionError> {
    for link in links {
        if link.avionics_model_id != product_id
            || link.configuration_action != "installed"
            || link.replaces_avionics_model_id.is_some()
        {
            return Err(AvionicsProductDeletionError::Conflict(format!(
                "cannot delete avionics catalog product {product_id}: listing {} association {} participates in a replacement action; resolve it explicitly first",
                link.aircraft_sale_listing_id, link.id
            )));
        }
    }
    Ok(())
}

fn parse_occurrence_role(role: &str) -> Result<OccurrenceRole, AvionicsProductDeletionError> {
    match role {
        "primary" => Ok(OccurrenceRole::Primary),
        "replacement" => Ok(OccurrenceRole::Replacement),
        _ => Err(AvionicsProductDeletionError::Conflict(format!(
            "stored avionics disposition has invalid occurrence role {role}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avionics::manufacturer::ensure_test_manufacturer_identity_for_model;
    use sqlx::SqlitePool;

    fn pool(db: &AppDb) -> &SqlitePool {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("deletion tests require SQLite");
        };
        pool
    }

    async fn insert_test_product(
        db: &AppDb,
        model: &str,
        normalized_model: &str,
        identifier: &str,
        normalized_identifier: &str,
    ) -> i64 {
        let pool = pool(db);
        let product_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_evidence_kind, identity_confidence
            ) VALUES (
              (SELECT id FROM avionics_manufacturers
               WHERE normalized_name = 'deletion maker'),
              ?, ?, 'manufacturer_model_number', ?, ?,
              'https://manufacturer.example/deletion-product',
              'Deletion product manufacturer data sheet',
              'The manufacturer data sheet identifies this exact deletion test product.',
              'authoritative_reference', 'very_high'
            )
            RETURNING id
            "#,
        )
        .bind(model)
        .bind(normalized_model)
        .bind(identifier)
        .bind(normalized_identifier)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
            SELECT ?, id FROM avionics_types
            WHERE normalized_name = 'deletion capability'
            "#,
        )
        .bind(product_id)
        .execute(pool)
        .await
        .unwrap();
        ensure_test_manufacturer_identity_for_model(db, product_id)
            .await
            .unwrap();
        sqlx::query(
            r#"
            UPDATE avionics_models
            SET catalog_status = 'approved',
                catalog_reviewed_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
        )
        .bind(product_id)
        .execute(pool)
        .await
        .unwrap();
        product_id
    }

    async fn test_catalog() -> (AppDb, i64, i64, i64) {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let reviewer_id = db.current_user(None).await.unwrap().id;
        let pool = pool(&db);
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Deletion Maker', 'deletion maker')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Deletion Capability', 'deletion capability')",
        )
        .execute(pool)
        .await
        .unwrap();
        let target_id =
            insert_test_product(&db, "DX 100", "dx 100", "DX-100-PN", "dx 100 pn").await;
        let preserved_id =
            insert_test_product(&db, "PX 200", "px 200", "PX-200-PN", "px 200 pn").await;
        (db, reviewer_id, target_id, preserved_id)
    }

    async fn insert_listing_with_capture(
        db: &AppDb,
        reviewer_id: i64,
        target_id: i64,
        preserved_id: i64,
    ) -> (i64, i64, String) {
        let pool = pool(db);
        let listing_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            ) VALUES (
              (SELECT aircraft_model_variant_id
               FROM aircraft_sale_listing_pending_compatibility_placeholder
               WHERE singleton_id = 1),
              ?, 'https://listing.example/deletion-test', 2020, 100000, 1000
            )
            RETURNING id
            "#,
        )
        .bind(reviewer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        for product_id in [target_id, preserved_id] {
            sqlx::query(
                r#"
                INSERT INTO aircraft_sale_listing_avionics (
                  aircraft_sale_listing_id, avionics_model_id,
                  source, source_confidence
                ) VALUES (?, ?, 'listing_review', 'high')
                "#,
            )
            .bind(listing_id)
            .bind(product_id)
            .execute(pool)
            .await
            .unwrap();
        }
        let install_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO plugin_installs (user_id, public_key_base64)
            VALUES (?, 'deletion-test-key')
            RETURNING id
            "#,
        )
        .bind(reviewer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let extraction = serde_json::json!({
            "avionics": [
                {"manufacturer": "Deletion Maker", "model": "DX-100"},
                {"manufacturer": "Deletion Maker", "model": "PX 200"}
            ]
        })
        .to_string();
        let submission_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64,
              extracted_listing_json, canonical_listing_id
            ) VALUES (
              ?, ?, 'https://listing.example/deletion-test',
              '<html>deletion test</html>', ?, 'deletion-test-signature', ?, ?
            )
            RETURNING id
            "#,
        )
        .bind(reviewer_id)
        .bind(install_id)
        .bind("1".repeat(64))
        .bind(&extraction)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        (listing_id, submission_id, extraction)
    }

    #[test]
    fn exact_identity_accepts_only_typography_and_same_or_blank_manufacturer() {
        let manufacturers = HashSet::from(["garmin".to_string()]);
        assert!(identity_is_exact(
            Some("Garmin"),
            Some("GMA-1347"),
            "GMA 1347",
            &manufacturers,
        ));
        assert!(identity_is_exact(
            None,
            Some("GMA1347"),
            "GMA 1347",
            &manufacturers,
        ));
        assert!(!identity_is_exact(
            Some("Garmin"),
            Some("GMA 1347A"),
            "GMA 1347",
            &manufacturers,
        ));
        assert!(!identity_is_exact(
            Some("Honeywell"),
            Some("GMA 1347"),
            "GMA 1347",
            &manufacturers,
        ));
    }

    #[tokio::test]
    async fn deletion_invalidates_ready_listing_and_preserves_unrelated_product() {
        let (db, reviewer_id, target_id, preserved_id) = test_catalog().await;
        let (listing_id, submission_id, extraction) =
            insert_listing_with_capture(&db, reviewer_id, target_id, preserved_id).await;
        let pool = pool(&db);

        // This fixture bypasses only the aircraft/authorization prerequisites
        // for entering ready. The production immutability trigger on listing
        // avionics remains installed, so the deletion must invalidate first.
        for trigger in [
            "listing_ready_requires_canonical_aircraft_update",
            "listing_ready_requires_aircraft_projection",
            "listing_ready_rejects_pending_aircraft_placeholder",
            "aircraft_sale_listings_ready_semantic_avionics",
        ] {
            sqlx::query(&format!("DROP TRIGGER {trigger}"))
                .execute(pool)
                .await
                .unwrap();
        }
        sqlx::query(
            r#"
            UPDATE aircraft_sale_listings
            SET ingestion_state = 'ready', is_verified = TRUE,
                ingestion_completed_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();

        let outcome = delete_avionics_product(&db, reviewer_id, target_id)
            .await
            .unwrap();
        assert_eq!(outcome.deleted_product_id, target_id);
        assert_eq!(outcome.deleted_product_name, "Deletion Maker DX 100");
        assert_eq!(outcome.affected_listing_ids, vec![listing_id]);
        assert_eq!(outcome.affected_listing_count, 1);
        assert_eq!(outcome.deleted_listing_association_count, 1);
        assert_eq!(outcome.discarded_occurrence_count, 1);

        let target_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models WHERE id = ?")
                .bind(target_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(target_count, 0);
        let preserved_link_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?
            "#,
        )
        .bind(listing_id)
        .bind(preserved_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(preserved_link_count, 1);
        let listing_state: (String, bool, Option<String>) = sqlx::query_as(
            r#"
            SELECT ingestion_state, is_verified, ingestion_completed_at
            FROM aircraft_sale_listings WHERE id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(listing_state, ("incomplete".to_string(), false, None));
        let disposition: (String, Option<i64>, String) = sqlx::query_as(
            r#"
            SELECT outcome, avionics_model_id, reason_code
            FROM aircraft_sale_listing_avionics_dispositions
            WHERE plugin_submission_id = ? AND extraction_sha256 = ?
              AND occurrence_index = 0 AND occurrence_role = 'primary'
            "#,
        )
        .bind(submission_id)
        .bind(extraction_sha256(&extraction))
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            disposition,
            (
                "discarded".to_string(),
                None,
                DELETION_REASON_CODE.to_string()
            )
        );
        let guard_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_catalog_product_deletion_guards")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(guard_count, 0);
    }

    #[tokio::test]
    async fn deletion_rejects_a_consolidation_survivor_without_mutation() {
        let (db, reviewer_id, target_id, preserved_id) = test_catalog().await;
        let pool = pool(&db);
        let duplicate_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name
            )
            SELECT avionics_manufacturer_id, name, normalized_name
            FROM avionics_models WHERE id = ?
            RETURNING id
            "#,
        )
        .bind(target_id)
        .fetch_one(pool)
        .await
        .unwrap();
        // The deletion boundary must reject a survivor FK before attempting
        // any mutation. This fixture needs only that dependency; bypass the
        // unrelated exact-identity admission rule used when creating a real
        // consolidation authorization.
        sqlx::query("DROP TRIGGER avionics_catalog_consolidation_guard_validate_insert")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            r#"
            INSERT INTO avionics_catalog_consolidation_guard (
              duplicate_model_id, survivor_model_id
            ) VALUES (?, ?)
            "#,
        )
        .bind(duplicate_id)
        .bind(target_id)
        .execute(pool)
        .await
        .unwrap();

        let error = delete_avionics_product(&db, reviewer_id, target_id)
            .await
            .unwrap_err();
        assert!(matches!(error, AvionicsProductDeletionError::Conflict(_)));
        assert!(error.to_string().contains("consolidation survivor"));
        let remaining_products: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models WHERE id IN (?, ?)")
                .bind(target_id)
                .bind(preserved_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(remaining_products, 2);
        let guard_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_catalog_product_deletion_guards")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(guard_count, 0);
    }
}
