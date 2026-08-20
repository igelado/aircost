//! Revalidate and apply read-only aircraft hierarchy curation results.
//!
//! The admin command, web workflow, and automatic listing verifier all use
//! this boundary. Paid model output retained in memory is never trusted
//! directly: every observation, FAA projection, and catalog revision is
//! reloaded before an assignment or catalog write.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;

use super::persistence::{
    persist_reviewable_aircraft_hierarchy, PersistReviewableAircraftHierarchy,
};
use super::workflow::{AircraftHierarchyCurationCaseReport, AircraftHierarchyCurationReport};
use crate::aircraft::catalog::AircraftHierarchy;
use crate::aircraft::faa::{AircraftGrounding, Eligibility};
use crate::aircraft::identity::{
    ensure_listing_identity_assignment_from_approved_catalog,
    CanonicalAircraftCompatibilityIdentity, CanonicalAircraftIdentityAssignment,
    EnsureIdentityAssignmentOutcome,
};
use crate::aircraft::observations::{
    load_aircraft_identity_observations, AircraftIdentityObservation,
};
use crate::db::AppDb;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AircraftHierarchyApplicationOutcome {
    pub cluster_key: String,
    pub listing_id: Option<i64>,
    pub observation_sha256: Option<String>,
    pub status: &'static str,
    pub catalog_writes: usize,
    pub assignment_id: Option<i64>,
    pub assignment_status: Option<&'static str>,
    pub approval_fingerprint: Option<String>,
    pub reason: Option<String>,
}

impl AircraftHierarchyApplicationOutcome {
    pub fn succeeded(&self) -> bool {
        matches!(
            self.status,
            "applied"
                | "idempotent"
                | "catalog_reused"
                | "catalog_reused_assigned"
                | "catalog_reused_current"
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AircraftHierarchyApplicationReport {
    pub requested: bool,
    pub attempted_observations: usize,
    pub applied_observations: usize,
    pub idempotent_observations: usize,
    pub catalog_reused_observations: usize,
    pub blocked_outcomes: usize,
    pub canonical_catalog_writes: usize,
    pub outcomes: Vec<AircraftHierarchyApplicationOutcome>,
}

impl AircraftHierarchyApplicationReport {
    pub fn dry_run() -> Self {
        Self {
            requested: false,
            attempted_observations: 0,
            applied_observations: 0,
            idempotent_observations: 0,
            catalog_reused_observations: 0,
            blocked_outcomes: 0,
            canonical_catalog_writes: 0,
            outcomes: Vec::new(),
        }
    }

    fn block_case(
        &mut self,
        case: &AircraftHierarchyCurationCaseReport,
        reason: impl Into<String>,
    ) {
        self.blocked_outcomes += 1;
        self.outcomes.push(AircraftHierarchyApplicationOutcome {
            cluster_key: case.cluster_key.clone(),
            listing_id: None,
            observation_sha256: None,
            status: "blocked",
            catalog_writes: 0,
            assignment_id: None,
            assignment_status: None,
            approval_fingerprint: None,
            reason: Some(reason.into()),
        });
    }
}

#[derive(Clone, Copy, Debug)]
struct AircraftApplyGrounding<'a> {
    listing_id: i64,
    observation_sha256: &'a str,
    grounding: &'a AircraftGrounding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AircraftObservationApplyPolicy {
    /// A reviewable result may create canonical catalog state and therefore
    /// remains bound to literal hierarchy labels in retained source.
    ReviewableCatalogWrite,
    /// This path can only assign an already-approved catalog identity.
    ApprovedCatalogReuse,
}

fn require_reviewable_apply_trace(
    case: &AircraftHierarchyCurationCaseReport,
) -> std::result::Result<(&str, Vec<AircraftApplyGrounding<'_>>), String> {
    if !case.validation_errors.is_empty() {
        return Err(format!(
            "reviewable payload was accompanied by validation errors: {}",
            case.validation_errors.join("; ")
        ));
    }
    let catalog_revision = case
        .catalog_revision
        .as_deref()
        .filter(|revision| !revision.trim().is_empty())
        .ok_or_else(|| "reviewable case has no exact catalog revision".to_string())?;
    if case.catalog_function_results.len() != 1
        || case.catalog_function_results[0].catalog_revision != catalog_revision
    {
        return Err("reviewable case has missing or ambiguous catalog grounding".to_string());
    }
    if case.faa_function_call_count != 1
        || case.faa_function_result_count != 1
        || case.faa_function_results.len() != 1
    {
        return Err("reviewable case has missing or ambiguous FAA function grounding".to_string());
    }
    let groundings = case.faa_function_results[0]
        .observations
        .iter()
        .map(|grounded| AircraftApplyGrounding {
            listing_id: grounded.listing_id,
            observation_sha256: &grounded.observation_sha256,
            grounding: &grounded.grounding,
        })
        .collect();
    Ok((catalog_revision, groundings))
}

fn approved_catalog_apply_groundings(
    case: &AircraftHierarchyCurationCaseReport,
) -> std::result::Result<Vec<AircraftApplyGrounding<'_>>, String> {
    if !case.validation_errors.is_empty() {
        return Err(format!(
            "approved-catalog case was accompanied by validation errors: {}",
            case.validation_errors.join("; ")
        ));
    }
    let mut groundings = Vec::new();
    for audit in &case.faa_observations {
        if !audit.faa_eligible || !audit.included_in_curation {
            continue;
        }
        let Some(Eligibility::Eligible { grounding }) = audit.eligibility.as_ref() else {
            return Err(format!(
                "included FAA audit has no exact eligible grounding for listing {} observation {}",
                audit.listing_id, audit.observation_sha256
            ));
        };
        groundings.push(AircraftApplyGrounding {
            listing_id: audit.listing_id,
            observation_sha256: &audit.observation_sha256,
            grounding,
        });
    }
    if groundings.is_empty() {
        return Err(
            "approved-catalog case has no exact FAA-eligible observation grounding".to_string(),
        );
    }
    Ok(groundings)
}

fn plan_case_observations<'observation, 'grounding>(
    case: &AircraftHierarchyCurationCaseReport,
    fresh_observations: &'observation [AircraftIdentityObservation],
    groundings: &[AircraftApplyGrounding<'grounding>],
    policy: AircraftObservationApplyPolicy,
) -> std::result::Result<
    Vec<(
        &'observation AircraftIdentityObservation,
        &'grounding AircraftGrounding,
    )>,
    String,
> {
    if policy == AircraftObservationApplyPolicy::ApprovedCatalogReuse {
        if case.approved_catalog_identity.is_none() {
            return Err(
                "non-mutating catalog reuse requires an approved exact catalog identity"
                    .to_string(),
            );
        }
        if !case.validation_errors.is_empty() {
            return Err(format!(
                "approved catalog reuse was accompanied by validation errors: {}",
                case.validation_errors.join("; ")
            ));
        }
    }

    let mut observations_by_key = BTreeMap::new();
    let mut ambiguous_observation_keys = BTreeSet::new();
    for (index, observation) in fresh_observations.iter().enumerate() {
        let key = (
            observation.listing_id,
            observation.observation_sha256.clone(),
        );
        if observations_by_key.insert(key.clone(), index).is_some() {
            ambiguous_observation_keys.insert(key);
        }
    }

    let expected_listing_ids = case
        .curation_listing_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if expected_listing_ids.len() != case.curation_listing_ids.len() {
        return Err("curation case repeats an eligible listing id".to_string());
    }
    let mut grounding_keys = BTreeSet::new();
    let mut planned = Vec::new();
    for grounded in groundings {
        let key = (grounded.listing_id, grounded.observation_sha256.to_string());
        if !grounding_keys.insert(key.clone()) {
            return Err(format!(
                "FAA grounding repeats listing {} observation {}",
                key.0, key.1
            ));
        }
        if !expected_listing_ids.contains(&grounded.listing_id)
            || !case.listing_ids.contains(&grounded.listing_id)
            || !case
                .observation_sha256s
                .iter()
                .any(|hash| hash == grounded.observation_sha256)
        {
            return Err(format!(
                "FAA grounding is not bound to the reported curation observation for listing {}",
                grounded.listing_id
            ));
        }
        if ambiguous_observation_keys.contains(&key) {
            return Err(format!(
                "fresh observation lookup is ambiguous for listing {} observation {}",
                key.0, key.1
            ));
        }
        let Some(index) = observations_by_key.get(&key).copied() else {
            return Err(format!(
                "fresh observation is missing for listing {} observation {}",
                key.0, key.1
            ));
        };
        let observation = &fresh_observations[index];
        if observation.cluster_key != case.cluster_key {
            return Err(format!(
                "fresh observation no longer has the cluster binding used for listing {}",
                grounded.listing_id
            ));
        }
        let has_exact_listing_source = observation.source_excerpt_is_exact
            && observation
                .source_excerpt
                .as_deref()
                .is_some_and(|excerpt| !excerpt.trim().is_empty());
        if policy == AircraftObservationApplyPolicy::ReviewableCatalogWrite
            && !has_exact_listing_source
        {
            return Err(format!(
                "fresh observation no longer has the exact listing source used for catalog curation for listing {}",
                grounded.listing_id
            ));
        }
        let audit_matches = case.faa_observations.iter().filter(|audit| {
            audit.listing_id == grounded.listing_id
                && audit.observation_sha256 == grounded.observation_sha256
                && audit.faa_eligible
                && audit.included_in_curation
        });
        if audit_matches.count() != 1 {
            return Err(format!(
                "FAA eligibility audit is missing or ambiguous for listing {} observation {}",
                grounded.listing_id, grounded.observation_sha256
            ));
        }
        planned.push((observation, grounded.grounding));
    }
    let grounded_listing_ids = grounding_keys
        .iter()
        .map(|(listing_id, _)| *listing_id)
        .collect::<BTreeSet<_>>();
    if planned.is_empty()
        || grounded_listing_ids != expected_listing_ids
        || planned.len() != expected_listing_ids.len()
    {
        return Err(
            "FAA grounding does not map one-to-one to every eligible curation listing".to_string(),
        );
    }
    Ok(planned)
}

async fn ensure_exact_catalog_assignment(
    db: &AppDb,
    listing_id: i64,
    grounding: &AircraftGrounding,
    expected: &CanonicalAircraftCompatibilityIdentity,
) -> std::result::Result<(&'static str, CanonicalAircraftIdentityAssignment), String> {
    let ensured =
        ensure_listing_identity_assignment_from_approved_catalog(db, listing_id, grounding)
            .await
            .map_err(|error| error.to_string())?;
    let (assignment_status, assignment) = match ensured {
        EnsureIdentityAssignmentOutcome::Current { assignment } => ("current", assignment),
        EnsureIdentityAssignmentOutcome::Assigned { assignment } => ("assigned", assignment),
        EnsureIdentityAssignmentOutcome::PendingCuration {
            reason,
            candidate_count,
        } => {
            return Err(format!(
                "approved catalog identity could not be assigned: {reason} (exact candidate count: {candidate_count})"
            ));
        }
    };
    let actual = CanonicalAircraftCompatibilityIdentity::from(&assignment);
    if &actual != expected {
        return Err(
            "ensured assignment differs from the exact approved catalog identity returned by curation"
                .to_string(),
        );
    }
    Ok((assignment_status, assignment))
}

fn assignment_matches_hierarchy(
    assignment: &CanonicalAircraftIdentityAssignment,
    hierarchy: &AircraftHierarchy,
) -> bool {
    assignment.aircraft_make_id == hierarchy.manufacturer_id
        && assignment.aircraft_model_family_id == hierarchy.model_family_id
        && assignment.aircraft_designation_id == hierarchy.certified_variant_id
        && assignment.aircraft_generation_id == hierarchy.generation_id
        && assignment.aircraft_factory_package_id == hierarchy.tier_id
}

/// Revalidate and apply every independently reviewable case in one curation
/// report.
pub async fn apply_aircraft_hierarchy_curation_report(
    db: &AppDb,
    report: &AircraftHierarchyCurationReport,
    listing_limit: i64,
    listing_id: Option<i64>,
) -> Result<AircraftHierarchyApplicationReport> {
    let fresh = load_aircraft_identity_observations(db, listing_limit, listing_id)
        .await
        .map_err(|error| anyhow!(error))
        .context("could not reload aircraft observations before apply")?;

    let mut application = AircraftHierarchyApplicationReport {
        requested: true,
        ..AircraftHierarchyApplicationReport::dry_run()
    };
    for case in &report.cases {
        let Some(reviewable) = case.reviewable.as_ref() else {
            let Some(approved_identity) = case.approved_catalog_identity.as_ref() else {
                let reason = if case.validation_errors.is_empty() {
                    "case did not produce a fully reviewable hierarchy".to_string()
                } else {
                    format!(
                        "case did not pass curation gates: {}",
                        case.validation_errors.join("; ")
                    )
                };
                application.block_case(case, reason);
                continue;
            };
            let fast_path_groundings = match approved_catalog_apply_groundings(case) {
                Ok(groundings) => groundings,
                Err(error) => {
                    application.block_case(case, error);
                    continue;
                }
            };
            let planned = match plan_case_observations(
                case,
                &fresh.observations,
                &fast_path_groundings,
                AircraftObservationApplyPolicy::ApprovedCatalogReuse,
            ) {
                Ok(planned) => planned,
                Err(error) => {
                    application.block_case(case, error);
                    continue;
                }
            };
            for (observation, grounding) in planned {
                application.attempted_observations += 1;
                let (assignment_status, assignment) = match ensure_exact_catalog_assignment(
                    db,
                    observation.listing_id,
                    grounding,
                    approved_identity,
                )
                .await
                {
                    Ok(assignment) => assignment,
                    Err(error) => {
                        application.blocked_outcomes += 1;
                        application
                            .outcomes
                            .push(AircraftHierarchyApplicationOutcome {
                                cluster_key: case.cluster_key.clone(),
                                listing_id: Some(observation.listing_id),
                                observation_sha256: Some(observation.observation_sha256.clone()),
                                status: "blocked",
                                catalog_writes: 0,
                                assignment_id: None,
                                assignment_status: None,
                                approval_fingerprint: None,
                                reason: Some(error),
                            });
                        continue;
                    }
                };
                application.catalog_reused_observations += 1;
                application
                    .outcomes
                    .push(AircraftHierarchyApplicationOutcome {
                        cluster_key: case.cluster_key.clone(),
                        listing_id: Some(observation.listing_id),
                        observation_sha256: Some(observation.observation_sha256.clone()),
                        status: match assignment_status {
                            "assigned" => "catalog_reused_assigned",
                            "current" => "catalog_reused_current",
                            _ => "catalog_reused",
                        },
                        catalog_writes: 0,
                        assignment_id: Some(assignment.assignment_id),
                        assignment_status: Some(assignment_status),
                        approval_fingerprint: None,
                        reason: None,
                    });
            }
            continue;
        };
        let (catalog_revision, case_groundings) = match require_reviewable_apply_trace(case) {
            Ok(trace) => trace,
            Err(error) => {
                application.block_case(case, error);
                continue;
            }
        };
        let mut planned = match plan_case_observations(
            case,
            &fresh.observations,
            &case_groundings,
            AircraftObservationApplyPolicy::ReviewableCatalogWrite,
        ) {
            Ok(planned) => planned,
            Err(error) => {
                application.block_case(case, error);
                continue;
            }
        };
        planned.sort_by(|(left, _), (right, _)| {
            (left.listing_id, left.observation_sha256.as_str())
                .cmp(&(right.listing_id, right.observation_sha256.as_str()))
        });
        let ((representative, representative_grounding), remaining) = planned
            .split_first()
            .expect("non-empty plan was validated before apply");
        application.attempted_observations += 1;
        let persisted = match persist_reviewable_aircraft_hierarchy(
            db,
            PersistReviewableAircraftHierarchy {
                listing_id: representative.listing_id,
                observation: representative,
                expected_catalog_revision: catalog_revision,
                reviewable,
                grounding: representative_grounding,
            },
        )
        .await
        {
            Ok(persisted) => persisted,
            Err(error) => {
                let reason = error.to_string();
                for (index, (observation, _)) in planned.iter().enumerate() {
                    application.blocked_outcomes += 1;
                    application
                        .outcomes
                        .push(AircraftHierarchyApplicationOutcome {
                            cluster_key: case.cluster_key.clone(),
                            listing_id: Some(observation.listing_id),
                            observation_sha256: Some(observation.observation_sha256.clone()),
                            status: "blocked",
                            catalog_writes: 0,
                            assignment_id: None,
                            assignment_status: None,
                            approval_fingerprint: None,
                            reason: Some(if index == 0 {
                                reason.clone()
                            } else {
                                format!(
                                    "representative hierarchy persistence was blocked: {reason}"
                                )
                            }),
                        });
                }
                continue;
            }
        };
        let status = if persisted.idempotent_replay {
            application.idempotent_observations += 1;
            "idempotent"
        } else {
            application.applied_observations += 1;
            "applied"
        };
        application.canonical_catalog_writes += persisted.catalog_writes;
        let expected_identity = CanonicalAircraftCompatibilityIdentity::from(&persisted.assignment);
        let approval_fingerprint = persisted.approval_fingerprint.clone();
        application
            .outcomes
            .push(AircraftHierarchyApplicationOutcome {
                cluster_key: case.cluster_key.clone(),
                listing_id: Some(representative.listing_id),
                observation_sha256: Some(representative.observation_sha256.clone()),
                status,
                catalog_writes: persisted.catalog_writes,
                assignment_id: Some(persisted.assignment.assignment_id),
                assignment_status: Some("persisted"),
                approval_fingerprint: Some(approval_fingerprint.clone()),
                reason: None,
            });

        for (observation, grounding) in remaining {
            application.attempted_observations += 1;
            match ensure_exact_catalog_assignment(
                db,
                observation.listing_id,
                grounding,
                &expected_identity,
            )
            .await
            {
                Ok((assignment_status, assignment)) => {
                    if !assignment_matches_hierarchy(&assignment, &persisted.hierarchy) {
                        application.blocked_outcomes += 1;
                        application
                            .outcomes
                            .push(AircraftHierarchyApplicationOutcome {
                                cluster_key: case.cluster_key.clone(),
                                listing_id: Some(observation.listing_id),
                                observation_sha256: Some(
                                    observation.observation_sha256.clone(),
                                ),
                                status: "blocked",
                                catalog_writes: 0,
                                assignment_id: Some(assignment.assignment_id),
                                assignment_status: Some(assignment_status),
                                approval_fingerprint: Some(approval_fingerprint.clone()),
                                reason: Some(
                                    "catalog-reused assignment differs from the representative persisted hierarchy"
                                        .to_string(),
                                ),
                            });
                        continue;
                    }
                    application.catalog_reused_observations += 1;
                    application
                        .outcomes
                        .push(AircraftHierarchyApplicationOutcome {
                            cluster_key: case.cluster_key.clone(),
                            listing_id: Some(observation.listing_id),
                            observation_sha256: Some(observation.observation_sha256.clone()),
                            status: match assignment_status {
                                "assigned" => "catalog_reused_assigned",
                                "current" => "catalog_reused_current",
                                _ => "catalog_reused",
                            },
                            catalog_writes: 0,
                            assignment_id: Some(assignment.assignment_id),
                            assignment_status: Some(assignment_status),
                            approval_fingerprint: Some(approval_fingerprint.clone()),
                            reason: None,
                        });
                }
                Err(error) => {
                    application.blocked_outcomes += 1;
                    application
                        .outcomes
                        .push(AircraftHierarchyApplicationOutcome {
                            cluster_key: case.cluster_key.clone(),
                            listing_id: Some(observation.listing_id),
                            observation_sha256: Some(observation.observation_sha256.clone()),
                            status: "blocked",
                            catalog_writes: 0,
                            assignment_id: None,
                            assignment_status: None,
                            approval_fingerprint: Some(approval_fingerprint.clone()),
                            reason: Some(error),
                        });
                }
            }
        }
    }
    Ok(application)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::curation::workflow::{
        FaaObservationAudit, FaaRegistryFunctionResult, FaaRegistryObservationGrounding,
    };
    use crate::aircraft::curation::{AircraftCatalogSearchRequest, AircraftCatalogSearchResponse};
    use crate::aircraft::faa::{AircraftReference, SerialMatch, Snapshot};

    fn grounding() -> AircraftGrounding {
        AircraftGrounding {
            snapshot: Snapshot {
                id: 2,
                evidence_source_id: 3,
                snapshot_date: "2026-07-23".to_string(),
                source_url: "https://www.faa.gov/registry".to_string(),
                archive_sha256: "a".repeat(64),
                source_manifest_sha256: "b".repeat(64),
                target_set_sha256: "c".repeat(64),
                record_hash_domain: crate::aircraft::faa::AIRCRAFT_RECORD_HASH_DOMAIN.to_string(),
            },
            n_number: "N89225".to_string(),
            manufacturer_serial_raw: Some("SERIAL".to_string()),
            manufacturer_serial_key: Some("SERIAL".to_string()),
            aircraft_code: "2072723".to_string(),
            engine_code: None,
            source_record_sha256: "d".repeat(64),
            year_manufactured: Some(2022),
            aircraft: Some(AircraftReference {
                aircraft_code: "2072723".to_string(),
                manufacturer_name: Some("TEXTRON AVIATION INC".to_string()),
                model_name: Some("182T".to_string()),
                aircraft_type_code: None,
                engine_type_code: None,
                category_code: None,
                certification_indicator_code: None,
                engine_count: Some(1),
                seat_count: Some(4),
                weight_class_code: None,
                cruise_speed_mph: None,
                type_certificate_data_sheet: None,
                type_certificate_holder: None,
            }),
            engine: None,
            serial_match: SerialMatch::RawExact,
        }
    }

    fn observation() -> AircraftIdentityObservation {
        AircraftIdentityObservation {
            listing_id: 23,
            submission_id: Some(7),
            source_url: Some("https://example.test/listing/23".to_string()),
            rendered_html_sha256: Some("e".repeat(64)),
            manufacturer: "Cessna".to_string(),
            model: "182".to_string(),
            variant: "182T".to_string(),
            model_year: 2022,
            serial_number: Some("SERIAL".to_string()),
            registration_number: Some("N89225".to_string()),
            source_excerpt: Some("2022 Cessna 182T".to_string()),
            source_excerpt_is_exact: true,
            source_kind: "retained_submission".to_string(),
            observation_sha256: "f".repeat(64),
            cluster_key: "cluster-182t".to_string(),
            requires_human_review: false,
            review_reasons: Vec::new(),
        }
    }

    fn case() -> AircraftHierarchyCurationCaseReport {
        let grounding = grounding();
        let observation = observation();
        let grounded = FaaRegistryObservationGrounding {
            listing_id: observation.listing_id,
            observation_sha256: observation.observation_sha256.clone(),
            observed_make: observation.manufacturer.clone(),
            observed_model: observation.model.clone(),
            observed_variant: observation.variant.clone(),
            listing_model_year: observation.model_year,
            model_year_differs_from_year_manufactured: false,
            grounding: grounding.clone(),
        };
        AircraftHierarchyCurationCaseReport {
            cluster_key: observation.cluster_key.clone(),
            listing_ids: vec![observation.listing_id],
            curation_listing_ids: vec![observation.listing_id],
            observation_sha256s: vec![observation.observation_sha256.clone()],
            source_observation_count: 1,
            skipped_non_exact_observation_count: 0,
            faa_eligible_observation_count: 1,
            faa_rejected_observation_count: 0,
            faa_snapshot: Some(grounding.snapshot.clone()),
            faa_observations: vec![FaaObservationAudit {
                listing_id: observation.listing_id,
                observation_sha256: observation.observation_sha256,
                supplied_registration: observation.registration_number,
                supplied_serial_number: observation.serial_number,
                listing_model_year: observation.model_year,
                faa_year_manufactured: grounding.year_manufactured,
                model_year_differs_from_year_manufactured: false,
                faa_eligible: true,
                included_in_curation: true,
                lookup_outcome: None,
                eligibility: Some(Eligibility::Eligible {
                    grounding: grounding.clone(),
                }),
                lookup_error: None,
            }],
            faa_function_call_count: 1,
            faa_function_result_count: 1,
            faa_function_results: vec![FaaRegistryFunctionResult {
                case_token: "case-token".to_string(),
                cluster_key: "cluster-182t".to_string(),
                snapshot: grounding.snapshot,
                year_manufactured_is_model_year: false,
                observations: vec![grounded],
            }],
            catalog_revision: Some("catalog-revision".to_string()),
            research: None,
            adjudication: None,
            verification: None,
            reviewable: None,
            approved_catalog_identity: None,
            approved_catalog_fallback_reasons: Vec::new(),
            validation_errors: Vec::new(),
            interactions: Vec::new(),
            evidence_reuse_audits: Vec::new(),
            catalog_function_results: vec![AircraftCatalogSearchResponse {
                catalog_revision: "catalog-revision".to_string(),
                catalog_is_empty: true,
                search_request: AircraftCatalogSearchRequest {
                    observed_make: observation.manufacturer,
                    observed_family: observation.model,
                    observed_designation: observation.variant,
                    observed_generation: None,
                    observed_package: None,
                    model_year: observation.model_year,
                },
                allowed_existing_ids_by_kind: BTreeMap::new(),
                candidates: Vec::new(),
                generation_designations: Vec::new(),
                package_applicability: Vec::new(),
                warning: String::new(),
            }],
        }
    }

    #[test]
    fn trace_rejects_validation_errors_and_catalog_or_faa_ambiguity() {
        let mut invalid = case();
        invalid
            .validation_errors
            .push("not independently verified".to_string());
        assert!(require_reviewable_apply_trace(&invalid)
            .unwrap_err()
            .contains("validation errors"));

        let mut stale = case();
        stale.catalog_function_results[0].catalog_revision = "stale".to_string();
        assert!(require_reviewable_apply_trace(&stale)
            .unwrap_err()
            .contains("catalog"));

        let mut ambiguous = case();
        ambiguous.faa_function_result_count = 2;
        assert!(require_reviewable_apply_trace(&ambiguous)
            .unwrap_err()
            .contains("FAA"));
    }

    #[test]
    fn planning_rejects_missing_duplicate_and_non_exact_observations() {
        let case = case();
        let (_, groundings) = require_reviewable_apply_trace(&case).unwrap();
        let exact_observation = observation();
        let mut stale = exact_observation.clone();
        stale.observation_sha256 = "0".repeat(64);
        assert!(plan_case_observations(
            &case,
            &[stale],
            &groundings,
            AircraftObservationApplyPolicy::ReviewableCatalogWrite,
        )
        .unwrap_err()
        .contains("missing"));
        assert!(plan_case_observations(
            &case,
            &[exact_observation.clone(), exact_observation],
            &groundings,
            AircraftObservationApplyPolicy::ReviewableCatalogWrite,
        )
        .unwrap_err()
        .contains("ambiguous"));

        let mut non_exact = observation();
        non_exact.source_excerpt_is_exact = false;
        assert!(plan_case_observations(
            &case,
            &[non_exact],
            &groundings,
            AircraftObservationApplyPolicy::ReviewableCatalogWrite,
        )
        .unwrap_err()
        .contains("exact listing source"));
    }

    #[test]
    fn approved_catalog_reuse_does_not_depend_on_listing_prose() {
        let mut case = case();
        case.approved_catalog_identity = Some(CanonicalAircraftCompatibilityIdentity {
            aircraft_make_id: 1,
            make_name: "TEXTRON AVIATION INC".to_string(),
            aircraft_model_family_id: 2,
            family_name: "Skylane".to_string(),
            aircraft_designation_id: 3,
            official_designation: "182T".to_string(),
            aircraft_generation_id: None,
            aircraft_factory_package_id: None,
        });
        case.catalog_revision = None;
        case.catalog_function_results.clear();
        case.faa_function_call_count = 0;
        case.faa_function_result_count = 0;
        case.faa_function_results.clear();

        let groundings = approved_catalog_apply_groundings(&case).unwrap();
        let mut observation = observation();
        observation.source_excerpt = None;
        observation.source_excerpt_is_exact = false;
        let observations = [observation];
        let planned = plan_case_observations(
            &case,
            &observations,
            &groundings,
            AircraftObservationApplyPolicy::ApprovedCatalogReuse,
        )
        .unwrap();
        assert_eq!(planned.len(), 1);
    }
}
