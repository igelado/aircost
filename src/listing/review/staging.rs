//! Pure projection of current listing avionics facts into review aspects.
//!
//! The projector consumes only strict current-schema extraction occurrences,
//! current listing links, current catalog/authorization state, and the small
//! reviewer-owned subset of the prior payload. It never copies machine-owned
//! reason strings and never interprets historical aspect IDs.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::avionics::model::avionics_identities_are_typography_exact;
use crate::extract::CURATED_AVIONICS_TYPES;
use crate::models::{ParsedAvionics, ParsedAvionicsReference};

use super::{
    preserved_product_aspect, reviewer_correction_binding_matches_assignment,
    validate_current_covered_associations, CoveredListingAssociation, ExistingAssignmentRow,
    ListingAssociationRole, PendingReviewAspect, ReviewAction, ReviewAspectId, ReviewError,
    ReviewProduct, ReviewResult, REVIEWER_CORRECTED_AVIONICS_KIND,
};

const ORDINARY_UNRESOLVED_REASON: &str = "catalog_product_unresolved";
const ORDINARY_UNVERIFIED_REASON: &str = "catalog_product_unverified";
const ASSOCIATION_UNAUTHORIZED_REASON: &str = "catalog_product_or_listing_corroboration_missing";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CatalogProjectionProduct {
    pub id: i64,
    pub manufacturer: String,
    pub model: String,
    pub capabilities: Vec<String>,
    pub catalog_status: String,
}

pub(super) struct PendingReviewProjection<'a> {
    pub occurrences: &'a [ParsedAvionics],
    pub assignments: &'a [ExistingAssignmentRow],
    pub catalog: &'a [CatalogProjectionProduct],
    pub authorized_associations: &'a HashSet<CoveredListingAssociation>,
    pub prior_aspects: &'a [PendingReviewAspect],
}

/// Prove that reset/re-evaluation cannot resurrect an extraction occurrence
/// that has no durable representation in the current residual review or
/// listing links.
///
/// `Ok(Some(reason))` is a typed requires-reextraction outcome for the public
/// reset boundary. Ambiguous many-to-one claims are conflicts because a fresh
/// extraction alone cannot adjudicate them.
pub(super) fn reset_requires_reextraction(
    occurrences: &[ParsedAvionics],
    assignments: &[ExistingAssignmentRow],
    prior_aspects: &[PendingReviewAspect],
) -> ReviewResult<Option<String>> {
    let reviewer_components = reviewer_components(prior_aspects)?;
    let reviewer_component_ids = reviewer_components
        .iter()
        .flat_map(|component| component.iter().cloned())
        .collect::<HashSet<_>>();
    let claimed_reviewer_occurrences =
        reviewer_occurrence_claims(occurrences, prior_aspects, &reviewer_components)?;
    let mut claimed_links = HashSet::new();
    let mut claimed_aspects = HashSet::new();
    for (index, occurrence) in occurrences.iter().enumerate() {
        if claimed_reviewer_occurrences.contains(&index) {
            continue;
        }
        let link_matches = assignments
            .iter()
            .enumerate()
            .filter(|(assignment_index, assignment)| {
                !claimed_links.contains(assignment_index)
                    && occurrence_matches_assignment_shape(occurrence, assignment)
                    && occurrence.source_evidence_text.as_deref()
                        == assignment.source_notes.as_deref()
                    && occurrence.source_confidence.is_some()
                    && assignment.source_confidence.is_some()
            })
            .map(|(assignment_index, _)| assignment_index)
            .collect::<Vec<_>>();
        if link_matches.len() > 1 {
            return Err(ambiguous_alignment(index, "current retained link evidence"));
        }
        if let Some(assignment_index) = link_matches.first() {
            claimed_links.insert(*assignment_index);
            continue;
        }

        let literal_identity_matches = assignments
            .iter()
            .enumerate()
            .filter(|(assignment_index, assignment)| {
                !claimed_links.contains(assignment_index)
                    && occurrence_matches_assignment_shape(occurrence, assignment)
                    && occurrence_identity_is_exact(occurrence, assignment)
            })
            .map(|(assignment_index, _)| assignment_index)
            .collect::<Vec<_>>();
        if literal_identity_matches.len() > 1 {
            return Err(ambiguous_alignment(
                index,
                "literal catalog identity and action graph",
            ));
        }
        if let Some(assignment_index) = literal_identity_matches.first() {
            claimed_links.insert(*assignment_index);
            continue;
        }

        let aspect_matches = prior_aspects
            .iter()
            .enumerate()
            .filter(|(aspect_index, aspect)| {
                !claimed_aspects.contains(aspect_index)
                    && aspect.kind.starts_with("avionics")
                    && !is_machine_synthetic(aspect)
                    && !reviewer_component_ids.contains(&aspect.id)
                    && aspect.source_evidence_text.as_deref()
                        == occurrence.source_evidence_text.as_deref()
                    && aspect.source_confidence.is_some()
                    && aspect_claims_literal_observation(aspect, occurrence, prior_aspects)
            })
            .map(|(aspect_index, _)| aspect_index)
            .collect::<Vec<_>>();
        if aspect_matches.len() > 1 {
            return Err(ReviewError::Conflict(format!(
                "current extraction avionics[{index}] matches more than one residual review aspect by exact evidence and relationship semantics; no review state was changed"
            )));
        }
        if let Some(aspect_index) = aspect_matches.first() {
            claimed_aspects.insert(*aspect_index);
            continue;
        }
        return Ok(Some(format!(
            "current extraction avionics[{index}] {} has no one-to-one current link or residual-review claim; validated re-extraction or a durable disposition receipt is required before reset",
            identity_label(occurrence.manufacturer.as_deref(), &occurrence.model),
        )));
    }
    Ok(None)
}

/// Rebuild the complete machine-owned avionics review from current facts.
///
/// Alignment is deliberately exact and one-to-one. Reviewer corrections claim
/// their immutable extraction slot first. Remaining occurrences may claim a
/// link by exact retained evidence and relationship semantics, then by one
/// unique canonical catalog identity. Any ambiguity rejects the whole
/// projection before a caller enters its write phase.
pub(super) fn project_pending_review(
    input: PendingReviewProjection<'_>,
) -> ReviewResult<Vec<PendingReviewAspect>> {
    let mut claimed_occurrence_components = HashSet::<(usize, ListingAssociationRole)>::new();
    let mut claimed_assignment_components = HashSet::<(usize, ListingAssociationRole)>::new();
    let reviewer_owned = preserve_reviewer_components(
        &input,
        &mut claimed_occurrence_components,
        &mut claimed_assignment_components,
    )?;
    let mut projected = Vec::new();
    projected.extend(reviewer_owned);

    for (index, occurrence) in input.occurrences.iter().enumerate() {
        if claimed_occurrence_components.contains(&(index, ListingAssociationRole::Installed)) {
            continue;
        }
        let assignment_index = align_occurrence(
            index,
            occurrence,
            input.assignments,
            &claimed_assignment_components,
            input.prior_aspects,
        )?;
        if let Some(assignment_index) = assignment_index {
            claimed_assignment_components
                .insert((assignment_index, ListingAssociationRole::Installed));
            if input.assignments[assignment_index]
                .replaces_avionics_model_id
                .is_some()
            {
                claimed_assignment_components
                    .insert((assignment_index, ListingAssociationRole::Replacement));
            }
        }
        let assignment = assignment_index.map(|index| &input.assignments[index]);
        let assignment_identity_is_exact = assignment
            .is_some_and(|assignment| occurrence_identity_is_exact(occurrence, assignment));
        // Evidence/shape alignment is sufficient to account for a retained
        // occurrence and its exact current link, but only canonical identity
        // equality may bind that occurrence to the link's catalog product. A
        // changed identity gets a normal correction card that owns the old
        // link but derives no product suggestion or reuse target from it.
        let exact_assignment = assignment.filter(|_| assignment_identity_is_exact);
        let installed_association = assignment.map(|assignment| CoveredListingAssociation {
            listing_link_id: assignment.listing_link_id,
            role: ListingAssociationRole::Installed,
            avionics_model_id: assignment.avionics_model_id,
        });
        let installed_authorized = installed_association
            .as_ref()
            .is_some_and(|association| input.authorized_associations.contains(association))
            && assignment_identity_is_exact;
        let replacement_association = assignment.and_then(|assignment| {
            assignment
                .replaces_avionics_model_id
                .map(|avionics_model_id| CoveredListingAssociation {
                    listing_link_id: assignment.listing_link_id,
                    role: ListingAssociationRole::Replacement,
                    avionics_model_id,
                })
        });
        let replacement_authorized = occurrence.replaces.is_none()
            || replacement_association
                .as_ref()
                .is_some_and(|association| input.authorized_associations.contains(association))
                && assignment_identity_is_exact;
        if installed_authorized && replacement_authorized {
            continue;
        }

        let primary_id = format!("avionics:{index}:primary");
        let replacement_id = format!("avionics:{index}:replacement");
        let mut primary = ordinary_aspect(
            primary_id,
            occurrence,
            occurrence_identity(occurrence),
            exact_assignment,
            input.catalog,
            installed_authorized,
        );
        if let Some(association) = installed_association {
            primary.covered_associations.push(association);
        }

        if let Some(replacement) = occurrence.replaces.as_ref() {
            if replacement_authorized {
                primary.replaces_product_id = replacement_association
                    .as_ref()
                    .map(|association| association.avionics_model_id);
            } else {
                primary.replacement_aspect_id = Some(replacement_id.clone().into());
                let mut child = replacement_aspect(
                    replacement_id,
                    occurrence,
                    replacement,
                    exact_assignment,
                    input.catalog,
                );
                if let Some(association) = replacement_association {
                    child.covered_associations.push(association);
                }
                projected.push(child);
            }
        }
        projected.push(primary);
    }

    add_unmatched_preserved_components(
        &mut projected,
        input.assignments,
        input.catalog,
        input.authorized_associations,
        &claimed_assignment_components,
    )?;
    Ok(projected)
}

fn preserve_reviewer_components(
    input: &PendingReviewProjection<'_>,
    claimed_occurrences: &mut HashSet<(usize, ListingAssociationRole)>,
    claimed_assignments: &mut HashSet<(usize, ListingAssociationRole)>,
) -> ReviewResult<Vec<PendingReviewAspect>> {
    let components = reviewer_components(input.prior_aspects)?;
    let component_ids = components
        .iter()
        .flat_map(|component| component.iter().cloned())
        .collect::<HashSet<_>>();

    let preserved_component = input
        .prior_aspects
        .iter()
        .filter(|aspect| component_ids.contains(&aspect.id))
        .cloned()
        .collect::<Vec<_>>();

    let occurrence_claims =
        reviewer_occurrence_claims(input.occurrences, input.prior_aspects, &components)?;
    for component in &components {
        let corrected = input
            .prior_aspects
            .iter()
            .filter(|aspect| {
                component.contains(&aspect.id) && aspect.kind == REVIEWER_CORRECTED_AVIONICS_KIND
            })
            .collect::<Vec<_>>();
        for aspect in corrected {
            if let Some(binding) = &aspect.reviewer_correction_association_binding {
                let assignment_matches = input
                    .assignments
                    .iter()
                    .filter(|assignment| {
                        assignment.listing_link_id == binding.listing_link_id
                            && aspect
                                .covered_associations
                                .first()
                                .is_some_and(|association| {
                                    reviewer_correction_binding_matches_assignment(
                                        aspect,
                                        association,
                                        assignment,
                                    )
                                })
                    })
                    .count();
                if assignment_matches != 1 {
                    return Err(stale_correction(
                        aspect,
                        "association binding is no longer exact and current",
                    ));
                }
            } else if !aspect.covered_associations.is_empty() {
                return Err(stale_correction(
                    aspect,
                    "has covered listing state without its exact association binding",
                ));
            }
        }
    }
    for occurrence_index in occurrence_claims {
        claimed_occurrences.insert((occurrence_index, ListingAssociationRole::Installed));
        if input.occurrences[occurrence_index].replaces.is_some() {
            claimed_occurrences.insert((occurrence_index, ListingAssociationRole::Replacement));
        }
    }

    validate_current_covered_associations(&preserved_component, input.assignments)?;
    for aspect in &preserved_component {
        for association in &aspect.covered_associations {
            let assignment_index = input
                .assignments
                .iter()
                .position(|assignment| assignment.listing_link_id == association.listing_link_id)
                .expect("covered-component validation requires a current listing link");
            if !claimed_assignments.insert((assignment_index, association.role)) {
                return Err(ReviewError::Conflict(format!(
                    "reviewer correction component claims listing link {} role {:?} more than once",
                    association.listing_link_id, association.role
                )));
            }
        }
    }

    Ok(preserved_component)
}

fn reviewer_components(
    aspects: &[PendingReviewAspect],
) -> ReviewResult<Vec<HashSet<ReviewAspectId>>> {
    let aspects_by_id = aspects
        .iter()
        .map(|aspect| (aspect.id.clone(), aspect))
        .collect::<HashMap<_, _>>();
    let mut visited = HashSet::new();
    let mut components = Vec::new();
    for seed in aspects
        .iter()
        .filter(|aspect| aspect.kind == REVIEWER_CORRECTED_AVIONICS_KIND)
    {
        if visited.contains(&seed.id) {
            continue;
        }
        let mut component = HashSet::new();
        let mut queue = VecDeque::from([seed.id.clone()]);
        while let Some(id) = queue.pop_front() {
            if !component.insert(id.clone()) {
                continue;
            }
            visited.insert(id.clone());
            let aspect = aspects_by_id.get(&id).ok_or_else(|| {
                ReviewError::Stale(format!(
                    "reviewer correction component {id} no longer exists"
                ))
            })?;
            if let Some(child) = &aspect.replacement_aspect_id {
                queue.push_back(child.clone());
            }
            for parent in aspects
                .iter()
                .filter(|parent| parent.replacement_aspect_id.as_ref() == Some(&id))
            {
                queue.push_back(parent.id.clone());
            }
        }
        components.push(component);
    }
    Ok(components)
}

/// Claim every extraction occurrence represented by a reviewer-owned
/// relationship component. A reviewer may connect two formerly independent
/// observations, so the relationship graph is atomic for preservation while
/// occurrence ownership remains one-to-one per distinct immutable evidence
/// span.
fn reviewer_occurrence_claims(
    occurrences: &[ParsedAvionics],
    aspects: &[PendingReviewAspect],
    components: &[HashSet<ReviewAspectId>],
) -> ReviewResult<HashSet<usize>> {
    let mut claimed_occurrences = HashSet::new();
    for component in components {
        let mut evidence_groups = Vec::<&str>::new();
        for aspect in aspects
            .iter()
            .filter(|aspect| component.contains(&aspect.id))
        {
            let evidence = aspect.source_evidence_text.as_deref().ok_or_else(|| {
                ReviewError::Stale(
                    "reviewer correction component lost immutable extraction evidence".to_string(),
                )
            })?;
            if aspect.source_confidence.is_none() {
                return Err(ReviewError::Stale(
                    "reviewer correction component lost immutable extraction confidence"
                        .to_string(),
                ));
            }
            if !evidence_groups.contains(&evidence) {
                evidence_groups.push(evidence);
            }
        }
        if evidence_groups.is_empty() {
            return Err(ReviewError::Stale(
                "reviewer correction component has no extraction evidence groups".to_string(),
            ));
        }
        for evidence in evidence_groups {
            let occurrence_matches = occurrences
                .iter()
                .enumerate()
                .filter(|(index, occurrence)| {
                    !claimed_occurrences.contains(index)
                        && occurrence.source_evidence_text.as_deref() == Some(evidence)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let [occurrence_index] = occurrence_matches.as_slice() else {
                return Err(ReviewError::Stale(
                    "reviewer correction evidence group does not claim exactly one current extraction occurrence"
                        .to_string(),
                ));
            };
            claimed_occurrences.insert(*occurrence_index);
        }
    }
    Ok(claimed_occurrences)
}

fn align_occurrence(
    occurrence_index: usize,
    occurrence: &ParsedAvionics,
    assignments: &[ExistingAssignmentRow],
    claimed: &HashSet<(usize, ListingAssociationRole)>,
    prior_aspects: &[PendingReviewAspect],
) -> ReviewResult<Option<usize>> {
    let available = |index: usize| !claimed.contains(&(index, ListingAssociationRole::Installed));
    let evidence_matches = assignments
        .iter()
        .enumerate()
        .filter(|(index, assignment)| {
            available(*index) && occurrence_matches_assignment_shape(occurrence, assignment)
        })
        .filter(|(_, assignment)| {
            occurrence.source_evidence_text.as_deref() == assignment.source_notes.as_deref()
                && occurrence.source_confidence.is_some()
                && assignment.source_confidence.is_some()
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if evidence_matches.len() > 1 {
        return Err(ambiguous_alignment(occurrence_index, "retained evidence"));
    }
    if let Some(index) = evidence_matches.first() {
        return Ok(Some(*index));
    }

    let prior_coverage_matches = prior_aspects
        .iter()
        .filter(|aspect| {
            aspect.kind.starts_with("avionics")
                && aspect.kind != REVIEWER_CORRECTED_AVIONICS_KIND
                && !is_machine_synthetic(aspect)
                && aspect.source_evidence_text.as_deref()
                    == occurrence.source_evidence_text.as_deref()
                && aspect.source_confidence.is_some()
                && aspect_claims_literal_observation(aspect, occurrence, prior_aspects)
        })
        .filter_map(|aspect| {
            let [association] = aspect.covered_associations.as_slice() else {
                return None;
            };
            (association.role == ListingAssociationRole::Installed).then_some(association)
        })
        .filter_map(|association| {
            assignments
                .iter()
                .enumerate()
                .find_map(|(index, assignment)| {
                    (available(index)
                        && assignment.listing_link_id == association.listing_link_id
                        && assignment.avionics_model_id == association.avionics_model_id
                        && occurrence_matches_assignment_shape(occurrence, assignment))
                    .then_some(index)
                })
        })
        .collect::<Vec<_>>();
    match prior_coverage_matches.as_slice() {
        [index] => return Ok(Some(*index)),
        [] => {}
        _ => {
            return Err(ambiguous_alignment(
                occurrence_index,
                "prior exact occurrence coverage",
            ));
        }
    }

    let identity_matches = assignments
        .iter()
        .enumerate()
        .filter(|(index, assignment)| {
            available(*index)
                && occurrence_matches_assignment_shape(occurrence, assignment)
                && occurrence_identity_is_exact(occurrence, assignment)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    match identity_matches.as_slice() {
        [] => Ok(None),
        [index] => Ok(Some(*index)),
        _ => Err(ambiguous_alignment(
            occurrence_index,
            "canonical catalog identity and action graph",
        )),
    }
}

fn occurrence_matches_assignment_shape(
    occurrence: &ParsedAvionics,
    assignment: &ExistingAssignmentRow,
) -> bool {
    occurrence.quantity == assignment.quantity
        && occurrence.configuration_action == assignment.configuration_action
        && occurrence.replaces.is_some() == assignment.replaces_avionics_model_id.is_some()
}

fn replacement_identity_is_exact(
    replacement: Option<&ParsedAvionicsReference>,
    assignment: &ExistingAssignmentRow,
) -> bool {
    match replacement {
        None => assignment.replaces_avionics_model_id.is_none(),
        Some(replacement) => {
            replacement
                .manufacturer
                .as_deref()
                .is_some_and(|observed_manufacturer| {
                    assignment
                        .replacement_manufacturer
                        .as_deref()
                        .is_some_and(|manufacturer| {
                            assignment
                                .replacement_model
                                .as_deref()
                                .is_some_and(|model| {
                                    avionics_identities_are_typography_exact(
                                        observed_manufacturer,
                                        &replacement.model,
                                        manufacturer,
                                        model,
                                    )
                                })
                        })
                })
        }
    }
}

fn occurrence_identity_is_exact(
    occurrence: &ParsedAvionics,
    assignment: &ExistingAssignmentRow,
) -> bool {
    occurrence
        .manufacturer
        .as_deref()
        .is_some_and(|observed_manufacturer| {
            assignment
                .installed_manufacturer
                .as_deref()
                .is_some_and(|manufacturer| {
                    assignment.installed_model.as_deref().is_some_and(|model| {
                        avionics_identities_are_typography_exact(
                            observed_manufacturer,
                            &occurrence.model,
                            manufacturer,
                            model,
                        )
                    })
                })
        })
        && replacement_identity_is_exact(occurrence.replaces.as_ref(), assignment)
}

fn aspect_matches_occurrence_semantics(
    aspect: &PendingReviewAspect,
    occurrence: &ParsedAvionics,
    aspects: &[PendingReviewAspect],
) -> bool {
    if aspect.quantity != occurrence.quantity
        || aspect.configuration_action != occurrence.configuration_action
    {
        return false;
    }
    match occurrence.replaces.as_ref() {
        None => aspect.replaces_product_id.is_none() && aspect.replacement_aspect_id.is_none(),
        Some(_) => {
            aspect.replaces_product_id.is_some()
                || aspect
                    .replacement_aspect_id
                    .as_ref()
                    .is_some_and(|child_id| {
                        aspects.iter().any(|child| {
                            &child.id == child_id
                                && child.quantity == 1
                                && child.configuration_action == "installed"
                                && child.replaces_product_id.is_none()
                                && child.replacement_aspect_id.is_none()
                        })
                    })
        }
    }
}

/// Bind an ordinary machine-owned review card to the exact current extraction
/// observation it represents.
///
/// Manufacturer-qualified observations retain a catalog-shaped proposal as
/// their literal identity snapshot. A model-only observation deliberately has
/// no proposal, because manufacturing a maker would turn a retrieval hint into
/// listing evidence. Its exact current label and observation text therefore
/// form the immutable identity/capability snapshot used by reset and restage.
fn aspect_claims_literal_observation(
    aspect: &PendingReviewAspect,
    occurrence: &ParsedAvionics,
    aspects: &[PendingReviewAspect],
) -> bool {
    if !aspect_matches_occurrence_semantics(aspect, occurrence, aspects) {
        return false;
    }
    match occurrence.manufacturer.as_deref() {
        Some(manufacturer) => aspect.proposed_product.as_ref().is_some_and(|proposed| {
            avionics_identities_are_typography_exact(
                &proposed.manufacturer,
                &proposed.model,
                manufacturer,
                &occurrence.model,
            )
        }),
        None => {
            aspect.proposed_product.is_none()
                && aspect.label == identity_label(None, &occurrence.model)
                && aspect.observed_text
                    == observation_text(
                        None,
                        &occurrence.model,
                        &occurrence.avionics_types,
                        occurrence.quantity,
                        &occurrence.configuration_action,
                    )
                && aspect.allowed_actions
                    == [ReviewAction::UseVerifiedProduct, ReviewAction::Discard]
        }
    }
}

fn is_machine_synthetic(aspect: &PendingReviewAspect) -> bool {
    aspect.kind == "avionics_reuse_attestation"
}

fn ordinary_aspect(
    id: String,
    occurrence: &ParsedAvionics,
    identity: (Option<&str>, &str, &[String]),
    assignment: Option<&ExistingAssignmentRow>,
    catalog: &[CatalogProjectionProduct],
    installed_authorized: bool,
) -> PendingReviewAspect {
    let catalog_matches = exact_catalog_matches(identity.0, identity.1, catalog);
    let matched_product = assignment.and_then(|assignment| {
        catalog
            .iter()
            .find(|product| product.id == assignment.avionics_model_id)
    });
    let reason = if matched_product.is_some_and(|product| product.catalog_status != "approved") {
        ORDINARY_UNVERIFIED_REASON
    } else if assignment.is_some() && !installed_authorized {
        ASSOCIATION_UNAUTHORIZED_REASON
    } else if catalog_matches.len() == 1 {
        ORDINARY_UNVERIFIED_REASON
    } else {
        ORDINARY_UNRESOLVED_REASON
    };
    let mut aspect = PendingReviewAspect::avionics(
        id,
        "avionics",
        identity_label(identity.0, identity.1),
        observation_text(
            identity.0,
            identity.1,
            identity.2,
            occurrence.quantity,
            &occurrence.configuration_action,
        ),
        reason,
        occurrence.quantity,
        occurrence.configuration_action.clone(),
        occurrence.source_evidence_text.clone(),
        occurrence.source_confidence.clone(),
    );
    aspect.allowed_actions = review_actions(identity.0.is_some());
    aspect.proposed_product = proposed_or_unreviewed(identity, &catalog_matches);
    if let Some(product) = matched_product.filter(|product| product.catalog_status == "approved") {
        aspect.suggested_product = Some(review_product(product));
        if !installed_authorized {
            aspect.reuse_attestation_target_id = Some(product.id);
        }
    }
    aspect
}

fn replacement_aspect(
    id: String,
    occurrence: &ParsedAvionics,
    replacement: &ParsedAvionicsReference,
    assignment: Option<&ExistingAssignmentRow>,
    catalog: &[CatalogProjectionProduct],
) -> PendingReviewAspect {
    let manufacturer = replacement.manufacturer.as_deref();
    let matches = exact_catalog_matches(manufacturer, &replacement.model, catalog);
    let matched_product = assignment
        .and_then(|assignment| assignment.replaces_avionics_model_id)
        .and_then(|id| catalog.iter().find(|product| product.id == id));
    let mut aspect = PendingReviewAspect::avionics(
        id,
        "avionics",
        identity_label(manufacturer, &replacement.model),
        observation_text(
            manufacturer,
            &replacement.model,
            &replacement.avionics_types,
            1,
            "installed",
        ),
        if matched_product.is_some_and(|product| product.catalog_status != "approved") {
            ORDINARY_UNVERIFIED_REASON
        } else if assignment.is_some() {
            ASSOCIATION_UNAUTHORIZED_REASON
        } else if matches.len() == 1 {
            ORDINARY_UNVERIFIED_REASON
        } else {
            ORDINARY_UNRESOLVED_REASON
        },
        1,
        "installed",
        occurrence.source_evidence_text.clone(),
        occurrence.source_confidence.clone(),
    );
    aspect.allowed_actions = review_actions(manufacturer.is_some());
    aspect.proposed_product = proposed_or_unreviewed(
        (
            manufacturer,
            &replacement.model,
            &replacement.avionics_types,
        ),
        &matches,
    );
    if let Some(product) = matched_product.filter(|product| product.catalog_status == "approved") {
        aspect.suggested_product = Some(review_product(product));
        aspect.reuse_attestation_target_id = Some(product.id);
    }
    aspect
}

fn add_unmatched_preserved_components(
    aspects: &mut Vec<PendingReviewAspect>,
    assignments: &[ExistingAssignmentRow],
    catalog: &[CatalogProjectionProduct],
    authorized: &HashSet<CoveredListingAssociation>,
    claimed: &HashSet<(usize, ListingAssociationRole)>,
) -> ReviewResult<()> {
    for (index, assignment) in assignments.iter().enumerate() {
        let installed = CoveredListingAssociation {
            listing_link_id: assignment.listing_link_id,
            role: ListingAssociationRole::Installed,
            avionics_model_id: assignment.avionics_model_id,
        };
        let installed_pending = !claimed.contains(&(index, ListingAssociationRole::Installed))
            && !authorized.contains(&installed);
        let replacement = assignment
            .replaces_avionics_model_id
            .map(|avionics_model_id| CoveredListingAssociation {
                listing_link_id: assignment.listing_link_id,
                role: ListingAssociationRole::Replacement,
                avionics_model_id,
            });
        let replacement_pending = replacement.as_ref().is_some_and(|association| {
            !claimed.contains(&(index, ListingAssociationRole::Replacement))
                && !authorized.contains(association)
        });
        if !installed_pending && !replacement_pending {
            continue;
        }
        let installed_product = catalog
            .iter()
            .find(|product| product.id == assignment.avionics_model_id)
            .ok_or_else(|| {
                ReviewError::Conflict(format!(
                    "unmatched listing link {} installed catalog id {} no longer exists",
                    assignment.listing_link_id, assignment.avionics_model_id
                ))
            })?;
        if installed_product.catalog_status != "approved" {
            return Err(ReviewError::Conflict(format!(
                "unmatched listing link {} installed catalog id {} is not approved and cannot be represented as a verified preserved product",
                assignment.listing_link_id, assignment.avionics_model_id
            )));
        }
        let installed_review_product = review_product(installed_product);
        let mut parent = preserved_product_aspect(
            assignment,
            ListingAssociationRole::Installed,
            &installed_review_product,
            None,
        );
        if replacement_pending {
            let association = replacement.expect("pending replacement exists");
            let product = catalog
                .iter()
                .find(|product| product.id == association.avionics_model_id)
                .ok_or_else(|| {
                    ReviewError::Conflict(format!(
                        "unmatched listing link {} replacement catalog id {} no longer exists",
                        assignment.listing_link_id, association.avionics_model_id
                    ))
                })?;
            if product.catalog_status != "approved" {
                return Err(ReviewError::Conflict(format!(
                    "unmatched listing link {} replacement catalog id {} is not approved and cannot be represented as a verified preserved product",
                    assignment.listing_link_id, association.avionics_model_id
                )));
            }
            let child = preserved_product_aspect(
                assignment,
                ListingAssociationRole::Replacement,
                &review_product(product),
                None,
            );
            parent.replaces_product_id = None;
            parent.replacement_aspect_id = Some(child.id.clone());
            aspects.push(child);
        } else if let Some(association) = replacement {
            parent.replaces_product_id = Some(association.avionics_model_id);
        }
        aspects.push(parent);
    }
    Ok(())
}

fn exact_catalog_matches<'a>(
    manufacturer: Option<&str>,
    model: &str,
    catalog: &'a [CatalogProjectionProduct],
) -> Vec<&'a CatalogProjectionProduct> {
    let Some(manufacturer) = manufacturer else {
        return Vec::new();
    };
    catalog
        .iter()
        .filter(|product| {
            product.catalog_status != "rejected"
                && product.manufacturer == manufacturer.trim()
                && product.model == model.trim()
        })
        .collect()
}

fn proposed_or_unreviewed(
    identity: (Option<&str>, &str, &[String]),
    matches: &[&CatalogProjectionProduct],
) -> Option<ReviewProduct> {
    let manufacturer = identity.0?;
    match matches {
        [product] if product.catalog_status == "unreviewed" => {
            Some(ReviewProduct::unreviewed_catalog_candidate(
                product.id,
                product.manufacturer.clone(),
                product.model.clone(),
                union_capabilities(identity.2, &product.capabilities),
            ))
        }
        _ => Some(ReviewProduct::proposed(
            manufacturer.trim(),
            identity.1.trim(),
            identity.2.to_vec(),
        )),
    }
}

fn union_capabilities(observed: &[String], catalog: &[String]) -> Vec<String> {
    let available = observed
        .iter()
        .chain(catalog)
        .map(|capability| capability.as_str())
        .collect::<HashSet<_>>();
    let mut union = CURATED_AVIONICS_TYPES
        .iter()
        .filter(|capability| available.contains(**capability))
        .map(|capability| (*capability).to_string())
        .collect::<Vec<_>>();
    if available.contains("Unknown") {
        union.push("Unknown".to_string());
    }
    union
}

fn review_product(product: &CatalogProjectionProduct) -> ReviewProduct {
    ReviewProduct::verified(
        product.id,
        product.manufacturer.clone(),
        product.model.clone(),
        product.capabilities.clone(),
    )
}

fn review_actions(can_create: bool) -> Vec<ReviewAction> {
    let mut actions = vec![ReviewAction::UseVerifiedProduct];
    if can_create {
        actions.push(ReviewAction::CreateVerifiedProduct);
    }
    actions.push(ReviewAction::Discard);
    actions
}

fn occurrence_identity(occurrence: &ParsedAvionics) -> (Option<&str>, &str, &[String]) {
    (
        occurrence.manufacturer.as_deref(),
        &occurrence.model,
        &occurrence.avionics_types,
    )
}

fn observation_text(
    manufacturer: Option<&str>,
    model: &str,
    capabilities: &[String],
    quantity: i64,
    action: &str,
) -> String {
    format!(
        "{} · {} · quantity {} · {}",
        identity_label(manufacturer, model),
        capabilities.join(", "),
        quantity,
        action
    )
}

fn identity_label(manufacturer: Option<&str>, model: &str) -> String {
    manufacturer
        .map(str::trim)
        .filter(|manufacturer| !manufacturer.is_empty())
        .map_or_else(
            || model.trim().to_string(),
            |manufacturer| format!("{manufacturer} {}", model.trim()),
        )
}

fn stale_correction(aspect: &PendingReviewAspect, reason: &str) -> ReviewError {
    ReviewError::Stale(format!("reviewer correction {} {reason}", aspect.id))
}

fn ambiguous_alignment(index: usize, stage: &str) -> ReviewError {
    ReviewError::Conflict(format!(
        "current extraction avionics[{index}] matches more than one listing link by {stage}; no review state was changed"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occurrence(manufacturer: &str, model: &str, evidence: &str) -> ParsedAvionics {
        ParsedAvionics {
            manufacturer: Some(manufacturer.to_string()),
            model: model.to_string(),
            avionics_types: vec!["Flight Display".to_string()],
            quantity: 1,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some(evidence.to_string()),
            source_confidence: Some("high".to_string()),
        }
    }

    fn product(id: i64, model: &str, status: &str) -> CatalogProjectionProduct {
        CatalogProjectionProduct {
            id,
            manufacturer: "Garmin".to_string(),
            model: model.to_string(),
            capabilities: vec!["Flight Display".to_string()],
            catalog_status: status.to_string(),
        }
    }

    fn assignment(id: i64, product_id: i64, model: &str, evidence: &str) -> ExistingAssignmentRow {
        ExistingAssignmentRow {
            listing_link_id: id,
            avionics_model_id: product_id,
            installed_manufacturer: Some("Garmin".to_string()),
            installed_model: Some(model.to_string()),
            replacement_manufacturer: None,
            replacement_model: None,
            quantity: 1,
            source: "listing".to_string(),
            source_notes: Some(evidence.to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            installed_catalog_status: Some("approved".to_string()),
            replacement_catalog_status: None,
        }
    }

    #[test]
    fn model_only_observation_has_no_fake_manufacturer_or_create_action() {
        let mut observation = occurrence("L3", "WX500", "WX500 Stormscope");
        observation.manufacturer = None;
        observation.avionics_types = vec!["Lightning Detection".to_string()];

        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &[observation],
            assignments: &[],
            catalog: &[],
            authorized_associations: &HashSet::new(),
            prior_aspects: &[],
        })
        .unwrap();

        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].label, "WX500");
        assert!(projected[0].observed_text.starts_with("WX500 ·"));
        assert_eq!(projected[0].proposed_product, None);
        assert_eq!(
            projected[0].allowed_actions,
            [ReviewAction::UseVerifiedProduct, ReviewAction::Discard]
        );
    }

    #[test]
    fn unchanged_model_only_card_is_a_durable_reset_and_restage_claim() {
        let mut observation = occurrence("L3", "WX500", "WX500 Stormscope");
        observation.manufacturer = None;
        observation.avionics_types = vec!["Lightning Detection".to_string()];
        let first = project_pending_review(PendingReviewProjection {
            occurrences: std::slice::from_ref(&observation),
            assignments: &[],
            catalog: &[],
            authorized_associations: &HashSet::new(),
            prior_aspects: &[],
        })
        .unwrap();

        assert_eq!(
            reset_requires_reextraction(std::slice::from_ref(&observation), &[], first.as_slice(),)
                .unwrap(),
            None
        );
        let restaged = project_pending_review(PendingReviewProjection {
            occurrences: std::slice::from_ref(&observation),
            assignments: &[],
            catalog: &[],
            authorized_associations: &HashSet::new(),
            prior_aspects: first.as_slice(),
        })
        .unwrap();
        assert_eq!(restaged, first);

        let mut invented = first;
        invented[0].proposed_product = Some(ReviewProduct::proposed(
            "L3",
            "WX500",
            vec!["Lightning Detection".to_string()],
        ));
        assert!(reset_requires_reextraction(&[observation], &[], &invented)
            .unwrap()
            .is_some());
    }

    #[test]
    fn listing_style_projection_omits_only_authorized_exact_occurrence() {
        let mut observations = vec![
            occurrence("Garmin", "GTN 750Xi", "GTN 750Xi"),
            occurrence("Garmin", "GTN 650Xi", "GTN 650Xi"),
            occurrence("Garmin", "G5", "Dual Garmin G5"),
            occurrence("Garmin", "GFC 500", "Garmin GFC 500"),
            occurrence("Garmin", "GTX 345", "Garmin GTX 345"),
            occurrence("Garmin", "FlightStream 210", "Garmin FlightStream 210"),
        ];
        observations[0].avionics_types = vec![
            "GPS".to_string(),
            "NAV".to_string(),
            "COM".to_string(),
            "Flight Display".to_string(),
        ];
        observations[1].avionics_types =
            vec!["GPS".to_string(), "NAV".to_string(), "COM".to_string()];
        let mut catalog = vec![
            product(1, "GTN 750Xi", "unreviewed"),
            product(2, "GTN 650Xi", "unreviewed"),
            product(3, "G5", "approved"),
            product(4, "GFC 500", "approved"),
            product(5, "GTX 345", "unreviewed"),
            product(6, "Flight Stream 210", "approved"),
            product(7, "GTN 650Xi", "unreviewed"),
        ];
        catalog[0].capabilities = vec!["GPS".to_string(), "Weather Radar".to_string()];
        let assignments = vec![
            assignment(11, 1, "GTN 750Xi", "GTN 750Xi"),
            assignment(12, 2, "GTN 650Xi", "GTN 650Xi"),
            assignment(13, 3, "G5", "Dual Garmin G5"),
            assignment(14, 4, "GFC 500", "Garmin GFC 500"),
            assignment(15, 5, "GTX 345", "Garmin GTX 345"),
            assignment(16, 6, "Flight Stream 210", "Garmin FlightStream 210"),
        ];
        let authorized = HashSet::from([CoveredListingAssociation {
            listing_link_id: 14,
            role: ListingAssociationRole::Installed,
            avionics_model_id: 4,
        }]);
        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &observations,
            assignments: &assignments,
            catalog: &catalog,
            authorized_associations: &authorized,
            prior_aspects: &[],
        })
        .unwrap();
        assert_eq!(projected.len(), 5);
        assert!(projected
            .iter()
            .all(|aspect| !aspect.id.to_string().contains("legacy")));
        assert!(projected
            .iter()
            .all(|aspect| !aspect.reason.contains("listing_action_graph_invalid")));
        assert!(projected
            .iter()
            .all(|aspect| aspect.label != "Garmin GFC 500"));
        let flight_stream = projected
            .iter()
            .filter(|aspect| aspect.label == "Garmin FlightStream 210")
            .collect::<Vec<_>>();
        assert_eq!(flight_stream.len(), 1);
        assert_eq!(flight_stream[0].covered_associations[0].listing_link_id, 16);
        for label in ["Garmin GTN 750Xi", "Garmin GTN 650Xi", "Garmin GTX 345"] {
            assert_eq!(
                projected
                    .iter()
                    .find(|aspect| aspect.label == label)
                    .unwrap()
                    .reason,
                ORDINARY_UNVERIFIED_REASON,
            );
        }
        for label in ["Garmin G5", "Garmin FlightStream 210"] {
            assert_eq!(
                projected
                    .iter()
                    .find(|aspect| aspect.label == label)
                    .unwrap()
                    .reason,
                ASSOCIATION_UNAUTHORIZED_REASON,
            );
        }
        let gtn_750 = projected
            .iter()
            .find(|aspect| aspect.label == "Garmin GTN 750Xi")
            .unwrap()
            .proposed_product
            .as_ref()
            .unwrap();
        assert_eq!(gtn_750.id, Some(1));
        assert_eq!(
            gtn_750.capabilities,
            ["GPS", "NAV", "COM", "Flight Display", "Weather Radar"]
        );
        let gtn_650 = projected
            .iter()
            .find(|aspect| aspect.label == "Garmin GTN 650Xi")
            .unwrap()
            .proposed_product
            .as_ref()
            .unwrap();
        assert_eq!(gtn_650.id, None);
        assert_eq!(gtn_650.capabilities, observations[1].avionics_types);
    }

    #[test]
    fn ambiguous_exact_evidence_fails_before_projection() {
        let observations = [occurrence("Garmin", "G5", "Garmin G5")];
        let assignments = [
            assignment(1, 1, "G5", "Garmin G5"),
            assignment(2, 1, "G5", "Garmin G5"),
        ];
        let error = project_pending_review(PendingReviewProjection {
            occurrences: &observations,
            assignments: &assignments,
            catalog: &[product(1, "G5", "approved")],
            authorized_associations: &HashSet::new(),
            prior_aspects: &[],
        })
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("matches more than one listing link"));
    }

    #[test]
    fn authorization_never_suppresses_a_changed_literal_identity() {
        let observations = [occurrence("Garmin", "G5", "Garmin G5 installed")];
        let assignments = [assignment(42, 9, "G500", "Garmin G5 installed")];
        let authorized = HashSet::from([CoveredListingAssociation {
            listing_link_id: 42,
            role: ListingAssociationRole::Installed,
            avionics_model_id: 9,
        }]);
        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &observations,
            assignments: &assignments,
            catalog: &[product(9, "G500", "approved")],
            authorized_associations: &authorized,
            prior_aspects: &[],
        })
        .unwrap();

        assert_eq!(projected.len(), 1);
        let correction = projected
            .iter()
            .find(|aspect| aspect.id.to_string() == "avionics:0:primary")
            .unwrap();
        assert_eq!(correction.label, "Garmin G5");
        assert_eq!(correction.reason, ORDINARY_UNRESOLVED_REASON);
        assert_eq!(correction.allowed_actions, review_actions(true));
        assert_eq!(
            correction.covered_associations,
            [CoveredListingAssociation {
                listing_link_id: 42,
                role: ListingAssociationRole::Installed,
                avionics_model_id: 9,
            }]
        );
        assert_eq!(correction.suggested_product, None);
        assert_eq!(correction.reuse_attestation_target_id, None);
        assert_eq!(
            correction
                .proposed_product
                .as_ref()
                .map(|product| product.model.as_str()),
            Some("G5")
        );

        let validated = super::super::validated_aspects(&projected).unwrap();
        assert_eq!(validated.len(), 1);
        assert_eq!(validated[0].allowed_actions, review_actions(true));
        assert_eq!(validated[0].covered_associations.len(), 1);

        let mut maintained = projected.clone();
        assert!(
            !super::super::remove_authorized_preserved_aspects(&mut maintained, &authorized,)
                .unwrap()
        );
        let approved = HashMap::from([(
            9,
            ReviewProduct::verified(9, "Garmin", "G500", vec!["Flight Display".to_string()]),
        )]);
        assert!(!super::super::add_unauthorized_preserved_aspects(
            &mut maintained,
            &assignments,
            &approved,
            &HashSet::new(),
        )
        .unwrap());
        assert_eq!(maintained[0].allowed_actions, review_actions(true));
        assert_eq!(maintained[0].reuse_attestation_target_id, None);

        let mut cleared_assignment = assignments[0].clone();
        cleared_assignment.source_notes = None;
        cleared_assignment.source_confidence = None;
        let rebuilt_again = project_pending_review(PendingReviewProjection {
            occurrences: &observations,
            assignments: &[cleared_assignment],
            catalog: &[product(9, "G500", "approved")],
            authorized_associations: &HashSet::new(),
            prior_aspects: &maintained,
        })
        .unwrap();
        assert_eq!(rebuilt_again.len(), 1);
        assert_eq!(rebuilt_again[0].label, "Garmin G5");
        assert_eq!(rebuilt_again[0].covered_associations.len(), 1);
        assert_eq!(rebuilt_again[0].reuse_attestation_target_id, None);
    }

    #[test]
    fn catalog_binding_accepts_only_typography_not_meaningful_variants() {
        assert!(avionics_identities_are_typography_exact(
            "Garmin",
            "FlightStream 210",
            "Garmin",
            "Flight Stream 210",
        ));
        assert!(!avionics_identities_are_typography_exact(
            "Garmin", "GNS 430", "Garmin", "GNS 430W",
        ));
        assert!(!avionics_identities_are_typography_exact(
            "Garmin",
            "G1000",
            "Garmin",
            "G1000 NXi",
        ));
        assert!(!avionics_identities_are_typography_exact(
            "", "G5", "", "G5",
        ));
    }

    #[test]
    fn reset_guard_refuses_an_unrepresented_extraction_occurrence() {
        let observations = [occurrence("Garmin", "G3X", "Garmin G3X")];
        let coincident_aircraft_card = PendingReviewAspect::avionics(
            "aircraft-card",
            "aircraft_identity",
            "Garmin G3X",
            "Garmin G3X",
            "aircraft review",
            1,
            "installed",
            Some("Garmin G3X".to_string()),
            Some("high".to_string()),
        );
        let reason = reset_requires_reextraction(&observations, &[], &[coincident_aircraft_card])
            .unwrap()
            .expect("an unrepresented extraction row must block reset");
        assert!(reason.contains("no one-to-one current link or residual-review claim"));
    }

    #[test]
    fn reset_guard_accepts_one_unique_literal_identity_with_placeholder_link_notes() {
        let observations = [occurrence(
            "Garmin",
            "GTN 750Xi",
            "Garmin GTN 750Xi GPS/NAV/COM/MFD",
        )];
        let mut placeholder = assignment(
            411,
            103,
            "GTN 750Xi",
            "reused previously grounded avionics metadata",
        );
        placeholder.source_confidence = None;

        assert_eq!(
            reset_requires_reextraction(&observations, &[placeholder], &[]).unwrap(),
            None
        );
    }

    #[test]
    fn exact_evidence_does_not_merge_g355_with_gnc355() {
        let observations = [occurrence("Garmin", "G355", "Garmin G355 installed")];
        let assignments = [assignment(20, 9, "GNC-355", "Garmin GNC-355 installed")];
        let prior = [PendingReviewAspect::avionics(
            "old-g355",
            "avionics",
            "Garmin G355",
            "Garmin G355",
            "stale machine reason",
            1,
            "installed",
            Some("Garmin G355 installed".to_string()),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "G355",
            vec!["GPS".to_string()],
        ))];
        assert!(
            reset_requires_reextraction(&observations, &assignments, &[])
                .unwrap()
                .is_some()
        );
        assert_eq!(
            reset_requires_reextraction(&observations, &assignments, &prior).unwrap(),
            None
        );
        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &observations,
            assignments: &assignments,
            catalog: &[product(9, "GNC-355", "approved")],
            authorized_associations: &HashSet::new(),
            prior_aspects: &prior,
        })
        .unwrap();
        assert_eq!(projected.len(), 2);
        assert!(projected.iter().any(|aspect| {
            aspect.id.to_string() == "avionics:0:primary" && aspect.covered_associations.is_empty()
        }));
        assert!(projected
            .iter()
            .any(|aspect| aspect.id.to_string() == "avionics:preserved:20:installed"));
    }

    #[test]
    fn distinct_gi275_occurrences_do_not_collapse_into_one_quantity_three_link() {
        let observations = [
            occurrence("Garmin", "GI 275 HSI", "GI 275 HSI installed"),
            occurrence("Garmin", "GI 275 ADAHRS", "GI 275 ADAHRS installed"),
            occurrence("Garmin", "GI 275 EIS", "GI 275 EIS installed"),
        ];
        let mut broad_link = assignment(35, 12, "GI 275", "Three Garmin GI 275 displays");
        broad_link.quantity = 3;
        let prior = observations
            .iter()
            .enumerate()
            .map(|(index, occurrence)| {
                PendingReviewAspect::avionics(
                    format!("old-{index}"),
                    "avionics",
                    occurrence.model.clone(),
                    occurrence.model.clone(),
                    "stale",
                    1,
                    "installed",
                    occurrence.source_evidence_text.clone(),
                    occurrence.source_confidence.clone(),
                )
                .with_proposed_product(ReviewProduct::proposed(
                    occurrence
                        .manufacturer
                        .clone()
                        .expect("fixture observations name Garmin"),
                    occurrence.model.clone(),
                    occurrence.avionics_types.clone(),
                ))
            })
            .collect::<Vec<_>>();
        assert!(
            reset_requires_reextraction(&observations, &[broad_link.clone()], &[])
                .unwrap()
                .is_some()
        );
        assert_eq!(
            reset_requires_reextraction(&observations, &[broad_link.clone()], &prior).unwrap(),
            None
        );
        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &observations,
            assignments: &[broad_link],
            catalog: &[product(12, "GI 275", "approved")],
            authorized_associations: &HashSet::new(),
            prior_aspects: &prior,
        })
        .unwrap();
        assert_eq!(projected.len(), 4);
        assert_eq!(
            projected
                .iter()
                .filter(|aspect| aspect.covered_associations.is_empty())
                .count(),
            3
        );
    }

    #[test]
    fn stale_reviewer_correction_binding_aborts_projection() {
        let observations = [occurrence("Garmin", "G5", "Garmin G5")];
        let mut correction = PendingReviewAspect::avionics(
            "corrected-g5",
            REVIEWER_CORRECTED_AVIONICS_KIND,
            "Garmin G5",
            "Garmin G5",
            "reviewer correction",
            1,
            "installed",
            Some("Garmin G5".to_string()),
            Some("high".to_string()),
        )
        .with_covered_association(99, ListingAssociationRole::Installed, 1);
        correction.reviewer_correction_association_binding =
            Some(super::super::ReviewerCorrectionAssociationBinding {
                listing_link_id: 99,
                avionics_model_id: 1,
                quantity: 1,
                configuration_action: "installed".to_string(),
                replaces_avionics_model_id: None,
            });
        correction.proposed_product = Some(ReviewProduct::proposed(
            "Garmin",
            "G5",
            vec!["Flight Display".to_string()],
        ));
        let error = project_pending_review(PendingReviewProjection {
            occurrences: &observations,
            assignments: &[],
            catalog: &[],
            authorized_associations: &HashSet::new(),
            prior_aspects: &[correction],
        })
        .unwrap_err();
        assert!(error.to_string().contains("binding is no longer exact"));
    }

    #[test]
    fn reset_preserves_exact_reviewer_corrections_verbatim() {
        let observations = [occurrence("Garmin", "G5", "Garmin G5")];
        let mut correction = PendingReviewAspect::avionics(
            "corrected-g5",
            REVIEWER_CORRECTED_AVIONICS_KIND,
            "Garmin G5",
            "reviewer-selected Garmin G5",
            "reviewer correction",
            1,
            "installed",
            Some("Garmin G5".to_string()),
            Some("high".to_string()),
        );
        correction.proposed_product = Some(ReviewProduct::proposed(
            "Garmin",
            "G5",
            vec!["Flight Display".to_string()],
        ));
        let prior = [correction.clone()];

        assert_eq!(
            reset_requires_reextraction(&observations, &[], &prior).unwrap(),
            None
        );
        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &observations,
            assignments: &[],
            catalog: &[],
            authorized_associations: &HashSet::new(),
            prior_aspects: &prior,
        })
        .unwrap();

        assert_eq!(projected, vec![correction]);
    }

    #[test]
    fn reviewer_corrected_replacement_component_claims_both_link_roles_once() {
        let evidence = "Garmin GTN 750Xi replaces Garmin GNS 530W";
        let mut observation = occurrence("Garmin", "GTN 750Xi", evidence);
        observation.configuration_action = "replaces".to_string();
        observation.replaces = Some(ParsedAvionicsReference {
            manufacturer: Some("Garmin".to_string()),
            model: "GNS 530W".to_string(),
            avionics_types: vec!["GPS".to_string()],
        });
        let mut link = assignment(414, 10, "GTN 750Xi", evidence);
        link.configuration_action = "replaces".to_string();
        link.replaces_avionics_model_id = Some(11);
        link.replacement_manufacturer = Some("Garmin".to_string());
        link.replacement_model = Some("GNS 530W".to_string());
        link.replacement_catalog_status = Some("approved".to_string());

        let child_id = super::super::ReviewAspectId::from("corrected-replacement");
        let mut parent = PendingReviewAspect::avionics(
            "corrected-primary",
            REVIEWER_CORRECTED_AVIONICS_KIND,
            "Garmin GTN 750Xi",
            "reviewer-selected GTN 750Xi",
            "reviewer correction",
            1,
            "replaces",
            Some(evidence.to_string()),
            Some("high".to_string()),
        )
        .with_replacement_aspect(child_id.clone())
        .with_covered_association(414, ListingAssociationRole::Installed, 10);
        parent.reviewer_correction_association_binding =
            Some(super::super::ReviewerCorrectionAssociationBinding {
                listing_link_id: 414,
                avionics_model_id: 10,
                quantity: 1,
                configuration_action: "replaces".to_string(),
                replaces_avionics_model_id: Some(11),
            });
        parent.proposed_product = Some(ReviewProduct::proposed(
            "Garmin",
            "GTN 750Xi",
            vec!["GPS".to_string()],
        ));
        let child = PendingReviewAspect::avionics(
            child_id,
            "avionics",
            "Garmin GNS 530W",
            "reviewer-retained replacement",
            "replacement correction",
            1,
            "installed",
            Some(evidence.to_string()),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GNS 530W",
            vec!["GPS".to_string()],
        ))
        .with_covered_association(414, ListingAssociationRole::Replacement, 11);
        let prior = vec![parent, child];

        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &[observation],
            assignments: &[link],
            catalog: &[
                product(10, "GTN 750Xi", "approved"),
                product(11, "GNS 530W", "approved"),
            ],
            authorized_associations: &HashSet::new(),
            prior_aspects: &prior,
        })
        .unwrap();

        assert_eq!(projected, prior);
        assert_eq!(
            projected
                .iter()
                .flat_map(|aspect| aspect.covered_associations.iter())
                .count(),
            2
        );
    }

    #[test]
    fn corrected_parent_and_child_share_one_relationship_occurrence_claim() {
        let (observation, link, mut prior) = corrected_relationship_fixture();
        let child = &mut prior[1];
        child.kind = REVIEWER_CORRECTED_AVIONICS_KIND.to_string();
        child.reviewer_correction_association_binding =
            Some(super::super::ReviewerCorrectionAssociationBinding {
                listing_link_id: 414,
                avionics_model_id: 10,
                quantity: 1,
                configuration_action: "replaces".to_string(),
                replaces_avionics_model_id: Some(11),
            });

        assert_eq!(
            reset_requires_reextraction(
                std::slice::from_ref(&observation),
                std::slice::from_ref(&link),
                &prior,
            )
            .unwrap(),
            None
        );
        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &[observation],
            assignments: &[link],
            catalog: &[
                product(10, "GTN 750Xi", "approved"),
                product(11, "GNS 530W", "approved"),
            ],
            authorized_associations: &HashSet::new(),
            prior_aspects: &prior,
        })
        .unwrap();
        assert_eq!(projected, prior);
    }

    #[test]
    fn corrected_child_claims_its_ordinary_parent_relationship_once() {
        let (observation, link, mut prior) = corrected_relationship_fixture();
        prior[0].kind = "avionics".to_string();
        prior[0].reviewer_correction_association_binding = None;
        prior[1].kind = REVIEWER_CORRECTED_AVIONICS_KIND.to_string();
        prior[1].reviewer_correction_association_binding =
            Some(super::super::ReviewerCorrectionAssociationBinding {
                listing_link_id: 414,
                avionics_model_id: 10,
                quantity: 1,
                configuration_action: "replaces".to_string(),
                replaces_avionics_model_id: Some(11),
            });

        assert_eq!(
            reset_requires_reextraction(
                std::slice::from_ref(&observation),
                std::slice::from_ref(&link),
                &prior,
            )
            .unwrap(),
            None
        );
        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &[observation],
            assignments: &[link],
            catalog: &[
                product(10, "GTN 750Xi", "approved"),
                product(11, "GNS 530W", "approved"),
            ],
            authorized_associations: &HashSet::new(),
            prior_aspects: &prior,
        })
        .unwrap();
        assert_eq!(projected, prior);
    }

    #[test]
    fn reviewer_relationship_spanning_independent_evidence_claims_each_occurrence_once() {
        let first_evidence = "Garmin GTN 750Xi installed";
        let second_evidence = "Garmin GNS 530W installed";
        let observations = [
            occurrence("Garmin", "GTN 750Xi", first_evidence),
            occurrence("Garmin", "GNS 530W", second_evidence),
        ];
        let child_id = ReviewAspectId::from("independent-gns-530w");
        let mut parent = PendingReviewAspect::avionics(
            "corrected-gtn-750xi",
            REVIEWER_CORRECTED_AVIONICS_KIND,
            "Garmin GTN 750Xi",
            "reviewer-selected GTN 750Xi",
            "reviewer correction",
            1,
            "replaces",
            Some(first_evidence.to_string()),
            Some("high".to_string()),
        )
        .with_replacement_aspect(child_id.clone())
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GTN 750Xi",
            vec!["Flight Display".to_string()],
        ));
        parent.allowed_actions = review_actions(true);
        let mut child = PendingReviewAspect::avionics(
            child_id,
            "avionics",
            "Garmin GNS 530W",
            "Garmin GNS 530W",
            "catalog_product_unresolved",
            1,
            "installed",
            Some(second_evidence.to_string()),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GNS 530W",
            vec!["Flight Display".to_string()],
        ));
        child.allowed_actions = review_actions(true);
        let prior = [parent, child];

        assert_eq!(
            reset_requires_reextraction(&observations, &[], &prior).unwrap(),
            None
        );
        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &observations,
            assignments: &[],
            catalog: &[],
            authorized_associations: &HashSet::new(),
            prior_aspects: &prior,
        })
        .unwrap();

        assert_eq!(projected, prior);
    }

    #[test]
    fn unmatched_preserved_links_require_approved_products_and_never_invent_evidence() {
        let link = assignment(77, 12, "GDL 69A", "generated resolver prose");
        let projected = project_pending_review(PendingReviewProjection {
            occurrences: &[],
            assignments: std::slice::from_ref(&link),
            catalog: &[product(12, "GDL 69A", "approved")],
            authorized_associations: &HashSet::new(),
            prior_aspects: &[],
        })
        .unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].source_evidence_text, None);
        assert_eq!(projected[0].source_confidence, None);

        let error = project_pending_review(PendingReviewProjection {
            occurrences: &[],
            assignments: &[link],
            catalog: &[product(12, "GDL 69A", "unreviewed")],
            authorized_associations: &HashSet::new(),
            prior_aspects: &[],
        })
        .unwrap_err();
        assert!(error.to_string().contains("is not approved"));
    }

    fn corrected_relationship_fixture() -> (
        ParsedAvionics,
        ExistingAssignmentRow,
        Vec<PendingReviewAspect>,
    ) {
        let evidence = "Garmin GTN 750Xi replaces Garmin GNS 530W";
        let mut observation = occurrence("Garmin", "GTN 750Xi", evidence);
        observation.configuration_action = "replaces".to_string();
        observation.replaces = Some(ParsedAvionicsReference {
            manufacturer: Some("Garmin".to_string()),
            model: "GNS 530W".to_string(),
            avionics_types: vec!["GPS".to_string()],
        });
        let mut link = assignment(414, 10, "GTN 750Xi", evidence);
        link.configuration_action = "replaces".to_string();
        link.replaces_avionics_model_id = Some(11);
        link.replacement_manufacturer = Some("Garmin".to_string());
        link.replacement_model = Some("GNS 530W".to_string());
        link.replacement_catalog_status = Some("approved".to_string());
        let child_id = super::super::ReviewAspectId::from("corrected-replacement");
        let mut parent = PendingReviewAspect::avionics(
            "corrected-primary",
            REVIEWER_CORRECTED_AVIONICS_KIND,
            "Garmin GTN 750Xi",
            "reviewer-selected GTN 750Xi",
            "reviewer correction",
            1,
            "replaces",
            Some(evidence.to_string()),
            Some("high".to_string()),
        )
        .with_replacement_aspect(child_id.clone())
        .with_covered_association(414, ListingAssociationRole::Installed, 10);
        parent.reviewer_correction_association_binding =
            Some(super::super::ReviewerCorrectionAssociationBinding {
                listing_link_id: 414,
                avionics_model_id: 10,
                quantity: 1,
                configuration_action: "replaces".to_string(),
                replaces_avionics_model_id: Some(11),
            });
        parent.proposed_product = Some(ReviewProduct::proposed(
            "Garmin",
            "GTN 750Xi",
            vec!["GPS".to_string()],
        ));
        let child = PendingReviewAspect::avionics(
            child_id,
            "avionics",
            "Garmin GNS 530W",
            "reviewer-retained replacement",
            "replacement correction",
            1,
            "installed",
            Some(evidence.to_string()),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GNS 530W",
            vec!["GPS".to_string()],
        ))
        .with_covered_association(414, ListingAssociationRole::Replacement, 11);
        (observation, link, vec![parent, child])
    }
}
