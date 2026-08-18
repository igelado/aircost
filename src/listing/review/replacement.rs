//! Atomic approval of one staged avionics replacement relationship.
//!
//! A replacement is one listing association with two catalog identities, not
//! two independent review decisions. This module deliberately exposes only a
//! paired mutation; ordinary single-aspect approval remains replacement-free.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use sqlx::FromRow;

use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplacementProductSelection {
    pub aspect_id: ReviewAspectId,
    pub product_id: i64,
    pub quantity: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApproveReplacementProductsRequest {
    pub review_payload_sha256: String,
    pub catalog_revision_sha256: String,
    pub parent: ReplacementProductSelection,
    pub child: ReplacementProductSelection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RelationshipPlan {
    parent_index: usize,
    child_index: usize,
    covered_link_id: Option<i64>,
}

#[derive(Clone, Debug, FromRow)]
struct GraphIdentityRow {
    avionics_model_id: i64,
    avionics_manufacturer_identity_id: i64,
    canonical_product_key: String,
}

fn relationship_plan(
    aspects: &[PendingReviewAspect],
    request: &ApproveReplacementProductsRequest,
) -> ReviewResult<RelationshipPlan> {
    if !valid_sha256(&request.review_payload_sha256)
        || !valid_sha256(&request.catalog_revision_sha256)
    {
        return Err(ReviewError::Validation(
            "review and catalog revisions must be lowercase SHA-256 hex values".to_string(),
        ));
    }
    if request.parent.aspect_id == request.child.aspect_id {
        return Err(ReviewError::Validation(
            "replacement parent and child must be different review aspects".to_string(),
        ));
    }
    if request.parent.product_id <= 0
        || request.child.product_id <= 0
        || request.parent.product_id == request.child.product_id
    {
        return Err(ReviewError::Validation(
            "replacement parent and child must select different positive approved product IDs"
                .to_string(),
        ));
    }

    let parent_index = aspects
        .iter()
        .position(|aspect| aspect.id == request.parent.aspect_id)
        .ok_or_else(|| {
            ReviewError::Stale(format!(
                "replacement parent aspect {} changed; reload the review",
                request.parent.aspect_id
            ))
        })?;
    let child_index = aspects
        .iter()
        .position(|aspect| aspect.id == request.child.aspect_id)
        .ok_or_else(|| {
            ReviewError::Stale(format!(
                "replacement child aspect {} changed; reload the review",
                request.child.aspect_id
            ))
        })?;
    let parent = &aspects[parent_index];
    let child = &aspects[child_index];

    for (role, aspect, selection) in [
        ("parent", parent, &request.parent),
        ("child", child, &request.child),
    ] {
        if !aspect.kind.starts_with("avionics")
            || !aspect
                .allowed_actions
                .contains(&ReviewAction::UseVerifiedProduct)
        {
            return Err(ReviewError::Validation(format!(
                "replacement {role} aspect {} does not allow approved-product selection",
                aspect.id
            )));
        }
        if selection.quantity <= 0 || selection.quantity != aspect.quantity {
            return Err(ReviewError::Validation(format!(
                "replacement {role} quantity must exactly match staged quantity {}",
                aspect.quantity
            )));
        }
        if aspect
            .reuse_attestation_target_id
            .is_some_and(|target_id| target_id != selection.product_id)
        {
            return Err(ReviewError::Conflict(format!(
                "replacement {role} aspect {} is bound to a different catalog product",
                aspect.id
            )));
        }
    }
    if parent.configuration_action != "replaces"
        || parent.replaces_product_id.is_some()
        || parent.replacement_aspect_id.as_ref() != Some(&child.id)
    {
        return Err(ReviewError::Validation(format!(
            "review aspect {} is not an explicit replacement parent for {}",
            parent.id, child.id
        )));
    }
    if child.quantity != 1
        || request.child.quantity != 1
        || child.configuration_action != "installed"
        || child.replaces_product_id.is_some()
        || child.replacement_aspect_id.is_some()
    {
        return Err(ReviewError::Validation(format!(
            "replacement child aspect {} must be an installed quantity-one target",
            child.id
        )));
    }

    if aspects.iter().enumerate().any(|(index, aspect)| {
        index != parent_index
            && index != child_index
            && (aspect.replacement_aspect_id.as_ref() == Some(&parent.id)
                || aspect.replacement_aspect_id.as_ref() == Some(&child.id))
    }) {
        return Err(ReviewError::Validation(
            "replacement relationship is part of another staged relationship".to_string(),
        ));
    }

    let covered_link_id = match (
        parent.covered_associations.as_slice(),
        child.covered_associations.as_slice(),
    ) {
        ([], []) => None,
        ([parent_association], [child_association])
            if parent_association.role == ListingAssociationRole::Installed
                && child_association.role == ListingAssociationRole::Replacement
                && parent_association.listing_link_id == child_association.listing_link_id =>
        {
            Some(parent_association.listing_link_id)
        }
        _ => {
            return Err(ReviewError::Validation(
                "replacement approval requires both aspects to cover the same link, or neither aspect to cover a link"
                    .to_string(),
            ))
        }
    };

    Ok(RelationshipPlan {
        parent_index,
        child_index,
        covered_link_id,
    })
}

fn reject_implicit_merge(
    assignments: &[ExistingAssignmentRow],
    covered_link_id: Option<i64>,
    parent_model_id: i64,
    child_model_id: i64,
) -> ReviewResult<()> {
    if let Some(collision) = assignments.iter().find(|assignment| {
        Some(assignment.listing_link_id) != covered_link_id
            && (assignment.avionics_model_id == parent_model_id
                || assignment.avionics_model_id == child_model_id
                || assignment.replaces_avionics_model_id == Some(parent_model_id)
                || assignment.replaces_avionics_model_id == Some(child_model_id))
    }) {
        return Err(ReviewError::Conflict(format!(
            "listing link {} already uses one selected replacement product; atomic approval refuses to merge independent associations",
            collision.listing_link_id
        )));
    }
    Ok(())
}

fn validate_resulting_action_graph(
    assignments: &[ExistingAssignmentRow],
    identities: &[GraphIdentityRow],
) -> ReviewResult<()> {
    let identity_keys = identities
        .iter()
        .map(|identity| {
            approved_avionics_product_key(
                identity.avionics_manufacturer_identity_id,
                &identity.canonical_product_key,
            )
            .map(|key| (identity.avionics_model_id, key))
            .map_err(ReviewError::Stale)
        })
        .collect::<ReviewResult<HashMap<_, _>>>()?;
    let actions = assignments
        .iter()
        .map(|assignment| {
            let subject_key = identity_keys
                .get(&assignment.avionics_model_id)
                .cloned()
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "approved catalog id {} has no canonical product identity",
                        assignment.avionics_model_id
                    ))
                })?;
            let displaced_key = assignment
                .replaces_avionics_model_id
                .map(|model_id| {
                    identity_keys.get(&model_id).cloned().ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "approved replacement catalog id {model_id} has no canonical product identity"
                        ))
                    })
                })
                .transpose()?;
            Ok(CanonicalAvionicsAction::new(
                subject_key,
                assignment.configuration_action.clone(),
                displaced_key,
            ))
        })
        .collect::<ReviewResult<Vec<_>>>()?;
    validate_canonical_avionics_actions(&actions).map_err(ReviewError::Validation)
}

/// Approve one complete parent/child replacement relationship without
/// grounding, product mutation, or any other review decision.
pub(crate) async fn approve_replacement_products_and_restage(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    request: &ApproveReplacementProductsRequest,
) -> ReviewResult<Option<StagedPendingReview>> {
    let lock_catalog = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models WHERE catalog_status = 'approved' ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_RESTAGE_CATALOG_LOCK_SQL),
    };
    let lock_listing_children = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql("SELECT 1"),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_LISTING_CHILD_LOCK_SQL),
    };
    let postgres_review_select = format!("{REVIEW_SELECT_SQL} FOR UPDATE OF listing, review");
    let select_review = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(REVIEW_SELECT_SQL),
        DatabaseBackend::Postgres(_) => db.sql(&postgres_review_select),
    };
    let catalog_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let active_collision_catalog_sql = db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
    let approved_products_sql = db.sql(APPROVED_PRODUCT_ROWS_SQL);
    let assignments_sql = db.sql(EXISTING_ASSIGNMENT_ROWS_SQL);
    let corroborations_sql = db.sql(association_authorization_rows_sql(db));
    let attested_product_ids_sql = db.sql(
        "SELECT avionics_model_id FROM avionics_product_reuse_attestations ORDER BY avionics_model_id",
    );
    let graph_identities_sql = db.sql(
        r#"
        SELECT avionics_model_id,
               avionics_manufacturer_identity_id,
               canonical_product_key
        FROM avionics_approved_product_graph_identities
        ORDER BY avionics_model_id
        "#,
    );
    let update_link = db.sql(
        r#"
        UPDATE aircraft_sale_listing_avionics
        SET avionics_model_id = ?,
            quantity = ?,
            source = 'listing_review',
            source_notes = ?,
            source_confidence = 'high',
            configuration_action = 'replaces',
            replaces_avionics_model_id = ?
        WHERE id = ?
          AND aircraft_sale_listing_id = ?
          AND avionics_model_id = ?
          AND quantity = ?
          AND configuration_action = ?
          AND COALESCE(replaces_avionics_model_id, CAST(-1 AS BIGINT))
              = COALESCE(?, CAST(-1 AS BIGINT))
        "#,
    );
    let insert_link = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics (
          aircraft_sale_listing_id,
          avionics_model_id,
          quantity,
          source,
          source_notes,
          source_confidence,
          configuration_action,
          replaces_avionics_model_id
        ) VALUES (?, ?, ?, 'listing_review', ?, 'high', 'replaces', ?)
        RETURNING id
        "#,
    );
    let update_review = db.sql(
        r#"
        UPDATE aircraft_sale_listing_pending_reviews
        SET extraction_sha256 = ?,
            catalog_revision_sha256 = ?,
            pending_aspect_count = ?,
            review_payload_json = ?,
            review_payload_sha256 = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE listing_id = ?
          AND review_payload_sha256 = ?
        "#,
    );
    let delete_review = db.sql(
        r#"
        DELETE FROM aircraft_sale_listing_pending_reviews
        WHERE listing_id = ?
          AND review_payload_sha256 = ?
        "#,
    );
    let mark_incomplete = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'incomplete',
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND created_by_user_id = ?
          AND ingestion_state = 'pending_review'
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_reviews review
            WHERE review.listing_id = aircraft_sale_listings.id
          )
        "#,
    );

    macro_rules! apply_in_transaction {
        ($pool:expr, $reuse_attestation_is_current:ident) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&lock_catalog)
                .execute(&mut *transaction)
                .await?;
            sqlx::query(&lock_listing_children)
                .execute(&mut *transaction)
                .await?;

            let row = sqlx::query_as::<_, ReviewRow>(&select_review)
                .bind(listing_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "pending review for listing {listing_id} changed or was resolved"
                    ))
                })?;
            if row.owner_user_id != owner_user_id {
                return Err(ReviewError::Permission(
                    "reviewers may only change reviews for listings they own".to_string(),
                ));
            }
            if row.ingestion_state != "pending_review"
                || row.is_verified
                || row.review_payload_sha256 != request.review_payload_sha256
            {
                return Err(ReviewError::Stale(
                    "pending review changed before replacement approval; reload".to_string(),
                ));
            }

            let mut payload = parse_payload(
                &row.review_payload_json,
                Some(&row.review_payload_sha256),
                row.pending_aspect_count,
            )?;
            let plan = relationship_plan(&payload.aspects, request)?;
            let mut assignments = sqlx::query_as::<_, ExistingAssignmentRow>(&assignments_sql)
                .bind(listing_id)
                .fetch_all(&mut *transaction)
                .await?;
            validate_current_covered_associations(&payload.aspects, &assignments)?;
            reject_implicit_merge(
                &assignments,
                plan.covered_link_id,
                request.parent.product_id,
                request.child.product_id,
            )?;

            let catalog_rows = sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let catalog_products = catalog_products(catalog_rows);
            let catalog_product_fingerprints =
                catalog_product_fingerprints(&catalog_products);
            let catalog_revision_sha256 = fingerprint_catalog_products(&catalog_products);
            if catalog_revision_sha256 != request.catalog_revision_sha256
                || catalog_revision_sha256 != row.catalog_revision_sha256
            {
                return Err(ReviewError::Stale(
                    "approved avionics catalog changed before replacement approval; reload"
                        .to_string(),
                ));
            }
            let approved_rows = sqlx::query_as::<_, ApprovedProductRow>(&approved_products_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let approved = approved_product_map(approved_rows);
            for selected_id in [
                request.parent.product_id,
                request.child.product_id,
            ] {
                if !approved.contains_key(&selected_id)
                    || !$reuse_attestation_is_current(db, &mut transaction, selected_id).await?
                {
                    return Err(ReviewError::Conflict(format!(
                        "avionics catalog id {selected_id} is not an approved current-policy reusable product"
                    )));
                }
            }

            let parent = payload.aspects[plan.parent_index].clone();
            let source_notes = parent.source_evidence_text.as_deref();
            let original_assignment = plan.covered_link_id.map(|listing_link_id| {
                assignments
                    .iter()
                    .find(|assignment| assignment.listing_link_id == listing_link_id)
                    .expect("covered relationship was validated against current assignments")
                    .clone()
            });
            let proposed_assignment_index =
                if let Some(listing_link_id) = plan.covered_link_id {
                    assignments
                        .iter()
                        .position(|assignment| assignment.listing_link_id == listing_link_id)
                        .expect("covered relationship was validated against current assignments")
                } else {
                    let parent_product = approved
                        .get(&request.parent.product_id)
                        .expect("approved parent product was loaded under lock");
                    let child_product = approved
                        .get(&request.child.product_id)
                        .expect("approved child product was loaded under lock");
                    assignments.push(ExistingAssignmentRow {
                        listing_link_id: -1,
                        avionics_model_id: request.parent.product_id,
                        installed_manufacturer: Some(parent_product.manufacturer.clone()),
                        installed_model: Some(parent_product.model.clone()),
                        replacement_manufacturer: Some(child_product.manufacturer.clone()),
                        replacement_model: Some(child_product.model.clone()),
                        quantity: request.parent.quantity,
                        source: "listing_review".to_string(),
                        source_notes: parent.source_evidence_text.clone(),
                        source_confidence: Some("high".to_string()),
                        configuration_action: "replaces".to_string(),
                        replaces_avionics_model_id: Some(request.child.product_id),
                        installed_catalog_status: Some("approved".to_string()),
                        replacement_catalog_status: Some("approved".to_string()),
                    });
                    assignments.len() - 1
                };
            {
                let proposed = &mut assignments[proposed_assignment_index];
                let parent_product = approved
                    .get(&request.parent.product_id)
                    .expect("approved parent product was loaded under lock");
                let child_product = approved
                    .get(&request.child.product_id)
                    .expect("approved child product was loaded under lock");
                proposed.avionics_model_id = request.parent.product_id;
                proposed.installed_manufacturer = Some(parent_product.manufacturer.clone());
                proposed.installed_model = Some(parent_product.model.clone());
                proposed.replacement_manufacturer = Some(child_product.manufacturer.clone());
                proposed.replacement_model = Some(child_product.model.clone());
                proposed.quantity = request.parent.quantity;
                proposed.source = "listing_review".to_string();
                proposed.source_notes = parent.source_evidence_text.clone();
                proposed.source_confidence = Some("high".to_string());
                proposed.configuration_action = "replaces".to_string();
                proposed.replaces_avionics_model_id = Some(request.child.product_id);
                proposed.installed_catalog_status = Some("approved".to_string());
                proposed.replacement_catalog_status = Some("approved".to_string());
            }
            let graph_identities = sqlx::query_as::<_, GraphIdentityRow>(&graph_identities_sql)
                .fetch_all(&mut *transaction)
                .await?;
            validate_resulting_action_graph(&assignments, &graph_identities)?;

            let listing_link_id = if let Some(original) = original_assignment {
                let listing_link_id = original.listing_link_id;
                let assignment = assignments
                    .get_mut(proposed_assignment_index)
                    .expect("proposed assignment index remains valid");
                let changed = sqlx::query(&update_link)
                    .bind(request.parent.product_id)
                    .bind(request.parent.quantity)
                    .bind(source_notes)
                    .bind(request.child.product_id)
                    .bind(listing_link_id)
                    .bind(listing_id)
                    .bind(original.avionics_model_id)
                    .bind(original.quantity)
                    .bind(original.configuration_action.as_str())
                    .bind(original.replaces_avionics_model_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if changed != 1 {
                    return Err(ReviewError::Stale(format!(
                        "listing link {listing_link_id} changed before replacement approval"
                    )));
                }
                assignment.listing_link_id = listing_link_id;
                listing_link_id
            } else {
                let listing_link_id = sqlx::query_scalar::<_, i64>(&insert_link)
                    .bind(listing_id)
                    .bind(request.parent.product_id)
                    .bind(request.parent.quantity)
                    .bind(source_notes)
                    .bind(request.child.product_id)
                    .fetch_one(&mut *transaction)
                    .await?;
                assignments[proposed_assignment_index].listing_link_id = listing_link_id;
                listing_link_id
            };

            let mut indexes = [plan.parent_index, plan.child_index];
            indexes.sort_unstable_by(|left, right| right.cmp(left));
            for index in indexes {
                payload.aspects.remove(index);
            }
            if !payload.aspects.is_empty() {
                payload.aspects = validated_aspects(&payload.aspects)?;
            }

            let attested_product_ids =
                sqlx::query_scalar::<_, i64>(&attested_product_ids_sql)
                    .fetch_all(&mut *transaction)
                    .await?;
            let mut reuse_attested_ids = HashSet::new();
            for avionics_model_id in attested_product_ids {
                if $reuse_attestation_is_current(db, &mut transaction, avionics_model_id).await? {
                    reuse_attested_ids.insert(avionics_model_id);
                }
            }
            let active_collision_catalog_rows =
                sqlx::query_as::<_, ActiveCollisionCatalogFingerprintRow>(
                    &active_collision_catalog_sql,
                )
                .fetch_all(&mut *transaction)
                .await?;
            let corroboration_rows =
                sqlx::query_as::<_, AssociationAuthorizationRow>(&corroborations_sql)
                    .bind(listing_id)
                    .fetch_all(&mut *transaction)
                    .await?;
            let authorized_associations = current_authorized_associations(
                listing_id,
                &assignments,
                &corroboration_rows,
                &reuse_attested_ids,
                &active_collision_catalog_rows,
                &catalog_product_fingerprints,
            );
            let accepted_parent = CoveredListingAssociation {
                listing_link_id,
                role: ListingAssociationRole::Installed,
                avionics_model_id: request.parent.product_id,
            };
            let accepted_child = CoveredListingAssociation {
                listing_link_id,
                role: ListingAssociationRole::Replacement,
                avionics_model_id: request.child.product_id,
            };
            if !authorized_associations.contains(&accepted_parent)
                || !authorized_associations.contains(&accepted_child)
            {
                return Err(ReviewError::Conflict(
                    "atomic replacement approval did not produce exact listing corroboration"
                        .to_string(),
                ));
            }

            remove_authorized_preserved_aspects(
                &mut payload.aspects,
                &authorized_associations,
            )?;
            add_unauthorized_preserved_aspects(
                &mut payload.aspects,
                &assignments,
                &approved,
                &authorized_associations,
            )?;
            validate_current_covered_associations(&payload.aspects, &assignments)?;
            let hidden_blockers = hidden_preserved_blockers(
                &payload.aspects,
                &assignments,
                &authorized_associations,
            );
            if !hidden_blockers.is_empty() {
                return Err(ReviewError::Conflict(format!(
                    "preserved avionics cannot be represented by the current review: {}",
                    hidden_blockers.join("; ")
                )));
            }

            if payload.aspects.is_empty() {
                let deleted = sqlx::query(&delete_review)
                    .bind(listing_id)
                    .bind(request.review_payload_sha256.as_str())
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if deleted != 1 {
                    return Err(ReviewError::Stale(
                        "pending review changed before replacement approval completed".to_string(),
                    ));
                }
                let changed = sqlx::query(&mark_incomplete)
                    .bind(listing_id)
                    .bind(owner_user_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if changed != 1 {
                    return Err(ReviewError::Stale(
                        "listing state changed while its satisfied review was cleared".to_string(),
                    ));
                }
                transaction.commit().await?;
                return Ok::<Option<StagedPendingReview>, ReviewError>(None);
            }

            let serialized = serialize_review_payload(&payload.aspects)?;
            let changed = sqlx::query(&update_review)
                .bind(serialized.extraction_sha256.as_str())
                .bind(catalog_revision_sha256.as_str())
                .bind(serialized.pending_aspect_count)
                .bind(serialized.review_payload_json.as_str())
                .bind(serialized.review_payload_sha256.as_str())
                .bind(listing_id)
                .bind(request.review_payload_sha256.as_str())
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if changed != 1 {
                return Err(ReviewError::Stale(
                    "pending review changed before replacement approval completed".to_string(),
                ));
            }
            transaction.commit().await?;
            Ok::<Option<StagedPendingReview>, ReviewError>(Some(StagedPendingReview {
                listing_id,
                review_payload_sha256: serialized.review_payload_sha256,
                catalog_revision_sha256,
                pending_aspect_count: serialized.pending_aspect_count,
            }))
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            apply_in_transaction!(pool, reuse_attestation_is_current_sqlite)
        }
        DatabaseBackend::Postgres(pool) => {
            apply_in_transaction!(pool, reuse_attestation_is_current_postgres)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ApproveReplacementProductsRequest {
        ApproveReplacementProductsRequest {
            review_payload_sha256: "a".repeat(64),
            catalog_revision_sha256: "b".repeat(64),
            parent: ReplacementProductSelection {
                aspect_id: "parent".into(),
                product_id: 10,
                quantity: 2,
            },
            child: ReplacementProductSelection {
                aspect_id: "child".into(),
                product_id: 20,
                quantity: 1,
            },
        }
    }

    fn aspects() -> Vec<PendingReviewAspect> {
        let mut parent = PendingReviewAspect::avionics(
            "parent",
            "avionics",
            "new",
            "two new units replace old",
            "candidate",
            2,
            "replaces",
            Some("two new units replace old".to_string()),
            Some("high".to_string()),
        )
        .with_replacement_aspect("child");
        parent.allowed_actions = vec![ReviewAction::UseVerifiedProduct];
        let mut child = PendingReviewAspect::avionics(
            "child",
            "avionics",
            "old",
            "old unit",
            "candidate",
            1,
            "installed",
            Some("old unit".to_string()),
            Some("high".to_string()),
        );
        child.allowed_actions = vec![ReviewAction::UseVerifiedProduct];
        vec![parent, child]
    }

    #[test]
    fn request_schema_is_strict_and_current_only() {
        let value = serde_json::json!({
            "review_payload_sha256": "a".repeat(64),
            "catalog_revision_sha256": "b".repeat(64),
            "parent": {
                "aspect_id": "parent",
                "product_id": 10,
                "quantity": 2
            },
            "child": {
                "aspect_id": "child",
                "product_id": 20,
                "quantity": 1
            }
        });
        assert!(serde_json::from_value::<ApproveReplacementProductsRequest>(value).is_ok());
        let legacy = serde_json::json!({
            "expected_review_payload_sha256": "a".repeat(64),
            "catalog_revision_sha256": "b".repeat(64),
            "parent": {
                "aspect_id": "parent",
                "product_id": 10,
                "quantity": 2
            },
            "child": {
                "aspect_id": "child",
                "product_id": 20,
                "quantity": 1
            }
        });
        assert!(serde_json::from_value::<ApproveReplacementProductsRequest>(legacy).is_err());
        let extra = serde_json::json!({
            "review_payload_sha256": "a".repeat(64),
            "catalog_revision_sha256": "b".repeat(64),
            "parent": {
                "aspect_id": "parent",
                "product_id": 10,
                "quantity": 2,
                "legacy_product_id": 10
            },
            "child": {
                "aspect_id": "child",
                "product_id": 20,
                "quantity": 1
            }
        });
        assert!(serde_json::from_value::<ApproveReplacementProductsRequest>(extra).is_err());
    }

    #[test]
    fn relationship_requires_both_exact_staged_aspects() {
        assert_eq!(
            relationship_plan(&aspects(), &request())
                .unwrap()
                .covered_link_id,
            None
        );

        let mut mismatched = request();
        mismatched.parent.quantity = 1;
        assert!(relationship_plan(&aspects(), &mismatched).is_err());

        let mut half_covered = aspects();
        half_covered[0] = half_covered[0].clone().with_covered_association(
            7,
            ListingAssociationRole::Installed,
            30,
        );
        assert!(relationship_plan(&half_covered, &request()).is_err());
    }

    #[test]
    fn relationship_rejects_third_aspect_links_and_child_quantities() {
        let mut linked = aspects();
        let mut third = PendingReviewAspect::avionics(
            "third",
            "avionics",
            "third",
            "third",
            "candidate",
            1,
            "replaces",
            None,
            None,
        )
        .with_replacement_aspect("child");
        third.allowed_actions = vec![ReviewAction::UseVerifiedProduct];
        linked.push(third);
        assert!(relationship_plan(&linked, &request()).is_err());

        let mut wrong_child_quantity = aspects();
        wrong_child_quantity[1].quantity = 2;
        assert!(relationship_plan(&wrong_child_quantity, &request()).is_err());
    }

    fn assignment(
        listing_link_id: i64,
        subject: i64,
        target: Option<i64>,
    ) -> ExistingAssignmentRow {
        ExistingAssignmentRow {
            listing_link_id,
            avionics_model_id: subject,
            installed_manufacturer: Some("Garmin".to_string()),
            installed_model: Some(format!("Product {subject}")),
            replacement_manufacturer: target.map(|_| "Garmin".to_string()),
            replacement_model: target.map(|id| format!("Product {id}")),
            quantity: 1,
            source: "listing_review".to_string(),
            source_notes: Some("listing evidence".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: if target.is_some() {
                "replaces".to_string()
            } else {
                "installed".to_string()
            },
            replaces_avionics_model_id: target,
            installed_catalog_status: Some("approved".to_string()),
            replacement_catalog_status: target.map(|_| "approved".to_string()),
        }
    }

    fn identity(model_id: i64, product_key: &str) -> GraphIdentityRow {
        GraphIdentityRow {
            avionics_model_id: model_id,
            avionics_manufacturer_identity_id: 1,
            canonical_product_key: product_key.to_string(),
        }
    }

    #[test]
    fn resulting_graph_rejects_cycles_and_semantic_collisions() {
        let identities = [
            identity(10, "parent"),
            identity(20, "child"),
            identity(30, "parent"),
        ];
        let cycle = [assignment(1, 10, Some(20)), assignment(2, 20, Some(10))];
        assert!(validate_resulting_action_graph(&cycle, &identities).is_err());

        let semantic_duplicate = [assignment(1, 10, None), assignment(2, 30, None)];
        assert!(validate_resulting_action_graph(&semantic_duplicate, &identities).is_err());
    }

    #[test]
    fn selected_products_never_merge_an_independent_link() {
        let assignments = [assignment(7, 10, Some(20)), assignment(8, 30, Some(40))];
        assert!(reject_implicit_merge(&assignments, Some(7), 30, 50).is_err());
        assert!(reject_implicit_merge(&assignments, Some(7), 50, 40).is_err());
        assert!(reject_implicit_merge(&assignments, Some(7), 50, 60).is_ok());
    }
}
