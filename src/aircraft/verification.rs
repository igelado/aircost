//! Idempotent per-listing aircraft verification.
//!
//! This workflow owns no listing lifecycle state. It establishes or
//! revalidates only the FAA-backed canonical aircraft assignment. Callers may
//! combine its terminal result with avionics verification before publishing a
//! listing.

use std::fmt;

use serde::Serialize;

use crate::aircraft::curation::application::apply_aircraft_hierarchy_curation_report;
use crate::aircraft::curation::workflow::{
    curate_aircraft_hierarchy_observations_with_config, AircraftHierarchyCurationReport,
};
use crate::aircraft::faa::{
    block_reason_code, drs::DrsClient, require_listing_admission, require_listing_faa_admission,
    AircraftAdmissionError, AircraftGrounding,
};
use crate::aircraft::identity::{
    ensure_listing_identity_assignment_from_approved_catalog, require_listing_identity_assignment,
    resolve_faa_backed_compatibility_identity, CanonicalAircraftCompatibilityIdentity,
    CanonicalAircraftIdentityAssignment, EnsureIdentityAssignmentOutcome, IdentityAssignmentError,
    ResolveCompatibilityIdentityOutcome,
};
use crate::aircraft::observations::load_aircraft_identity_observations;
use crate::db::AppDb;
use crate::gemini::config::GeminiRuntimeConfig;
use crate::gemini::interactions::GeminiInteractionsClient;

#[derive(Clone, Copy)]
pub struct AircraftVerificationServices<'a> {
    pub gemini: &'a GeminiInteractionsClient,
    pub drs: &'a DrsClient,
    pub config: &'a GeminiRuntimeConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AircraftVerificationMethod {
    CurrentAssignment,
    ApprovedCatalog,
    GroundedCuration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AircraftVerificationPendingReason {
    SourceEvidenceMissing,
    GroundingRequired,
    GroundingServicesUnavailable,
    CurationNotReviewable,
    ConcurrentChange,
}

impl AircraftVerificationPendingReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::SourceEvidenceMissing => "source_evidence_missing",
            Self::GroundingRequired => "grounding_required",
            Self::GroundingServicesUnavailable => "grounding_services_unavailable",
            Self::CurationNotReviewable => "curation_not_reviewable",
            Self::ConcurrentChange => "concurrent_change",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AircraftVerificationOutcome {
    Verified {
        method: AircraftVerificationMethod,
        assignment: CanonicalAircraftIdentityAssignment,
        catalog_writes: usize,
        idempotent_replay: bool,
    },
    LocallyAssignable {
        identity: CanonicalAircraftCompatibilityIdentity,
        faa_n_number: String,
        faa_snapshot_id: i64,
    },
    GroundingPreview {
        ready_to_apply: bool,
        catalog_revision: Option<String>,
        provider_request_count: usize,
        validation_errors: Vec<String>,
        faa_n_number: String,
        faa_snapshot_id: i64,
    },
    Pending {
        reason: AircraftVerificationPendingReason,
        reason_code: &'static str,
        detail: String,
        candidate_count: usize,
        faa_n_number: Option<String>,
        faa_snapshot_id: Option<i64>,
    },
    Rejected {
        reason_code: String,
        faa_n_number: Option<String>,
        faa_snapshot_id: Option<i64>,
    },
}

impl AircraftVerificationOutcome {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    fn pending(
        reason: AircraftVerificationPendingReason,
        detail: impl Into<String>,
        candidate_count: usize,
        grounding: Option<&AircraftGrounding>,
    ) -> Self {
        Self::Pending {
            reason,
            reason_code: reason.code(),
            detail: detail.into(),
            candidate_count,
            faa_n_number: grounding.map(|grounding| grounding.n_number.clone()),
            faa_snapshot_id: grounding.map(|grounding| grounding.snapshot.id),
        }
    }
}

#[derive(Debug)]
pub enum AircraftVerificationError {
    NotFound(i64),
    Database(String),
    Grounding(String),
}

impl fmt::Display for AircraftVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(listing_id) => write!(formatter, "listing {listing_id} was not found"),
            Self::Database(message) => write!(formatter, "aircraft verification failed: {message}"),
            Self::Grounding(message) => {
                write!(
                    formatter,
                    "grounded aircraft verification failed: {message}"
                )
            }
        }
    }
}

impl std::error::Error for AircraftVerificationError {}

/// Inspect the cheapest safe aircraft verification path without writing or
/// making network requests.
pub async fn preflight_listing_aircraft_verification(
    db: &AppDb,
    listing_id: i64,
) -> Result<AircraftVerificationOutcome, AircraftVerificationError> {
    if listing_id <= 0 {
        return Err(AircraftVerificationError::NotFound(listing_id));
    }
    let grounding = match require_listing_faa_admission(db, listing_id).await {
        Ok(grounding) => grounding,
        Err(error) => return admission_outcome_or_error(listing_id, error),
    };

    match require_listing_identity_assignment(db, listing_id, &grounding).await {
        Ok(assignment) => {
            return Ok(verified(
                AircraftVerificationMethod::CurrentAssignment,
                assignment,
                0,
                true,
            ));
        }
        Err(IdentityAssignmentError::Missing(_))
        | Err(IdentityAssignmentError::Mismatch { .. }) => {}
        Err(error) => return Err(identity_error(listing_id, error)),
    }

    let observations = load_aircraft_identity_observations(db, 1, Some(listing_id))
        .await
        .map_err(|error| AircraftVerificationError::Database(error.to_string()))?;
    let observation = observations
        .observations
        .first()
        .ok_or(AircraftVerificationError::NotFound(listing_id))?;
    let resolution = resolve_faa_backed_compatibility_identity(
        db,
        Some(listing_id),
        observation.model_year,
        &grounding,
    )
    .await
    .map_err(|error| identity_error(listing_id, error))?;
    match resolution {
        ResolveCompatibilityIdentityOutcome::Resolved { identity } => {
            Ok(AircraftVerificationOutcome::LocallyAssignable {
                identity,
                faa_n_number: grounding.n_number,
                faa_snapshot_id: grounding.snapshot.id,
            })
        }
        ResolveCompatibilityIdentityOutcome::PendingCuration {
            reason,
            candidate_count,
        } => {
            let has_exact_source = observation.source_excerpt_is_exact
                && observation
                    .source_excerpt
                    .as_deref()
                    .is_some_and(|excerpt| !excerpt.trim().is_empty());
            if !has_exact_source {
                return Ok(AircraftVerificationOutcome::pending(
                    AircraftVerificationPendingReason::SourceEvidenceMissing,
                    "The listing has no exact retained publisher span for its aircraft hierarchy.",
                    candidate_count,
                    Some(&grounding),
                ));
            }
            Ok(AircraftVerificationOutcome::pending(
                AircraftVerificationPendingReason::GroundingRequired,
                reason,
                candidate_count,
                Some(&grounding),
            ))
        }
    }
}

/// Run the paid grounded aircraft pass when preflight requires it, without
/// applying catalog or assignment writes.
pub async fn preview_listing_aircraft_verification(
    db: &AppDb,
    listing_id: i64,
    services: Option<AircraftVerificationServices<'_>>,
) -> Result<AircraftVerificationOutcome, AircraftVerificationError> {
    let preflight = preflight_listing_aircraft_verification(db, listing_id).await?;
    let AircraftVerificationOutcome::Pending {
        reason: AircraftVerificationPendingReason::GroundingRequired,
        candidate_count,
        faa_n_number,
        faa_snapshot_id,
        ..
    } = preflight
    else {
        return Ok(preflight);
    };
    let Some(services) = services else {
        return Ok(AircraftVerificationOutcome::Pending {
            reason: AircraftVerificationPendingReason::GroundingServicesUnavailable,
            reason_code: AircraftVerificationPendingReason::GroundingServicesUnavailable.code(),
            detail: "Aircraft hierarchy grounding requires configured Gemini and FAA DRS services."
                .to_string(),
            candidate_count,
            faa_n_number,
            faa_snapshot_id,
        });
    };
    let report = run_grounded_curation(db, listing_id, services).await?;
    let case = report.cases.first();
    let ready_to_apply = case
        .is_some_and(|case| case.reviewable.is_some() || case.approved_catalog_identity.is_some());
    let validation_errors = case
        .map(|case| case.validation_errors.clone())
        .unwrap_or_else(|| vec!["grounded curation produced no listing case".to_string()]);
    if !ready_to_apply {
        return Ok(AircraftVerificationOutcome::pending(
            AircraftVerificationPendingReason::CurationNotReviewable,
            if validation_errors.is_empty() {
                "Grounded curation did not produce an independently reviewable aircraft identity."
                    .to_string()
            } else {
                validation_errors.join("; ")
            },
            0,
            None,
        ));
    }
    Ok(AircraftVerificationOutcome::GroundingPreview {
        ready_to_apply,
        catalog_revision: case.and_then(|case| case.catalog_revision.clone()),
        provider_request_count: report
            .cases
            .iter()
            .map(|case| case.interactions.len())
            .sum(),
        validation_errors,
        faa_n_number: faa_n_number.expect("FAA-grounded preflight retains its N-number"),
        faa_snapshot_id: faa_snapshot_id.expect("FAA-grounded preflight retains its snapshot"),
    })
}

/// Establish a canonical aircraft assignment. Deterministic FAA and approved
/// catalog checks always run before the injected network services are used.
pub async fn apply_listing_aircraft_verification(
    db: &AppDb,
    listing_id: i64,
    services: Option<AircraftVerificationServices<'_>>,
) -> Result<AircraftVerificationOutcome, AircraftVerificationError> {
    let preflight = preflight_listing_aircraft_verification(db, listing_id).await?;
    match preflight {
        verified @ AircraftVerificationOutcome::Verified { .. }
        | verified @ AircraftVerificationOutcome::Rejected { .. }
        | verified @ AircraftVerificationOutcome::GroundingPreview { .. } => Ok(verified),
        pending @ AircraftVerificationOutcome::Pending {
            reason: AircraftVerificationPendingReason::SourceEvidenceMissing,
            ..
        } => Ok(pending),
        AircraftVerificationOutcome::LocallyAssignable { .. } => {
            let grounding = require_listing_faa_admission(db, listing_id)
                .await
                .map_err(|error| admission_error_after_preflight(listing_id, error))?;
            match ensure_listing_identity_assignment_from_approved_catalog(
                db, listing_id, &grounding,
            )
            .await
            .map_err(|error| identity_error(listing_id, error))?
            {
                EnsureIdentityAssignmentOutcome::Current { assignment } => Ok(verified(
                    AircraftVerificationMethod::CurrentAssignment,
                    assignment,
                    0,
                    true,
                )),
                EnsureIdentityAssignmentOutcome::Assigned { assignment } => {
                    require_listing_admission(db, listing_id)
                        .await
                        .map_err(|error| admission_error_after_preflight(listing_id, error))?;
                    Ok(verified(
                        AircraftVerificationMethod::ApprovedCatalog,
                        assignment,
                        0,
                        false,
                    ))
                }
                EnsureIdentityAssignmentOutcome::PendingCuration {
                    reason,
                    candidate_count,
                } => {
                    if services.is_none() {
                        return Ok(AircraftVerificationOutcome::pending(
                            AircraftVerificationPendingReason::ConcurrentChange,
                            format!(
                                "The approved aircraft catalog changed during verification: {reason}"
                            ),
                            candidate_count,
                            Some(&grounding),
                        ));
                    }
                    apply_grounded_curation(db, listing_id, services).await
                }
            }
        }
        AircraftVerificationOutcome::Pending {
            reason: AircraftVerificationPendingReason::GroundingRequired,
            candidate_count,
            faa_n_number,
            faa_snapshot_id,
            ..
        } => {
            let Some(services) = services else {
                return Ok(AircraftVerificationOutcome::Pending {
                    reason: AircraftVerificationPendingReason::GroundingServicesUnavailable,
                    reason_code: AircraftVerificationPendingReason::GroundingServicesUnavailable
                        .code(),
                    detail:
                        "Aircraft hierarchy grounding requires configured Gemini and FAA DRS services."
                            .to_string(),
                    candidate_count,
                    faa_n_number,
                    faa_snapshot_id,
                });
            };
            apply_grounded_curation(db, listing_id, Some(services)).await
        }
        pending @ AircraftVerificationOutcome::Pending { .. } => Ok(pending),
    }
}

async fn apply_grounded_curation(
    db: &AppDb,
    listing_id: i64,
    services: Option<AircraftVerificationServices<'_>>,
) -> Result<AircraftVerificationOutcome, AircraftVerificationError> {
    let services = services.expect("grounded curation checks injected services first");
    let report = run_grounded_curation(db, listing_id, services).await?;
    let application = apply_aircraft_hierarchy_curation_report(db, &report, 1, Some(listing_id))
        .await
        .map_err(|error| AircraftVerificationError::Database(format!("{error:#}")))?;
    let outcome = application
        .outcomes
        .iter()
        .find(|outcome| outcome.listing_id == Some(listing_id) && outcome.succeeded());
    let Some(outcome) = outcome else {
        let detail = application
            .outcomes
            .iter()
            .filter_map(|outcome| outcome.reason.as_deref())
            .chain(
                report
                    .cases
                    .iter()
                    .flat_map(|case| case.validation_errors.iter().map(String::as_str)),
            )
            .collect::<Vec<_>>()
            .join("; ");
        return Ok(AircraftVerificationOutcome::pending(
            AircraftVerificationPendingReason::CurationNotReviewable,
            if detail.is_empty() {
                "Grounded curation did not produce an independently reviewable aircraft identity."
                    .to_string()
            } else {
                detail
            },
            0,
            None,
        ));
    };
    require_listing_admission(db, listing_id)
        .await
        .map_err(|error| admission_error_after_preflight(listing_id, error))?;
    let grounding = require_listing_faa_admission(db, listing_id)
        .await
        .map_err(|error| admission_error_after_preflight(listing_id, error))?;
    let assignment = require_listing_identity_assignment(db, listing_id, &grounding)
        .await
        .map_err(|error| identity_error(listing_id, error))?;
    let method = if outcome.status.starts_with("catalog_reused") {
        AircraftVerificationMethod::ApprovedCatalog
    } else {
        AircraftVerificationMethod::GroundedCuration
    };
    Ok(verified(
        method,
        assignment,
        outcome.catalog_writes,
        outcome.status == "idempotent" || outcome.status == "catalog_reused_current",
    ))
}

async fn run_grounded_curation(
    db: &AppDb,
    listing_id: i64,
    services: AircraftVerificationServices<'_>,
) -> Result<AircraftHierarchyCurationReport, AircraftVerificationError> {
    curate_aircraft_hierarchy_observations_with_config(
        db,
        services.gemini,
        services.drs,
        1,
        Some(listing_id),
        1,
        services.config,
    )
    .await
    .map_err(|error| AircraftVerificationError::Grounding(format!("{error:#}")))
}

fn verified(
    method: AircraftVerificationMethod,
    assignment: CanonicalAircraftIdentityAssignment,
    catalog_writes: usize,
    idempotent_replay: bool,
) -> AircraftVerificationOutcome {
    AircraftVerificationOutcome::Verified {
        method,
        assignment,
        catalog_writes,
        idempotent_replay,
    }
}

fn admission_outcome_or_error(
    listing_id: i64,
    error: AircraftAdmissionError,
) -> Result<AircraftVerificationOutcome, AircraftVerificationError> {
    match error {
        AircraftAdmissionError::Rejected {
            reason,
            n_number,
            snapshot_id,
            ..
        } => Ok(AircraftVerificationOutcome::Rejected {
            reason_code: block_reason_code(&reason).to_string(),
            faa_n_number: n_number,
            faa_snapshot_id: snapshot_id,
        }),
        AircraftAdmissionError::LookupFailed { message, .. } => {
            Err(AircraftVerificationError::Database(message))
        }
        AircraftAdmissionError::ListingNotFound { .. } => {
            Err(AircraftVerificationError::NotFound(listing_id))
        }
    }
}

fn admission_error_after_preflight(
    listing_id: i64,
    error: AircraftAdmissionError,
) -> AircraftVerificationError {
    match error {
        AircraftAdmissionError::ListingNotFound { .. } => {
            AircraftVerificationError::NotFound(listing_id)
        }
        AircraftAdmissionError::LookupFailed { message, .. } => {
            AircraftVerificationError::Database(message)
        }
        AircraftAdmissionError::Rejected { .. } => AircraftVerificationError::Grounding(format!(
            "FAA admission changed after preflight: {error}"
        )),
    }
}

fn identity_error(listing_id: i64, error: IdentityAssignmentError) -> AircraftVerificationError {
    match error {
        IdentityAssignmentError::ListingNotFound(_) => {
            AircraftVerificationError::NotFound(listing_id)
        }
        IdentityAssignmentError::Database(message) => AircraftVerificationError::Database(message),
        IdentityAssignmentError::Missing(_) | IdentityAssignmentError::Mismatch { .. } => {
            AircraftVerificationError::Grounding(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::faa::{
        store_release, AircraftRecord, AircraftReference, MemberProvenance, Release,
        ReleaseMetadata, TargetCoverage,
    };
    use crate::db::DatabaseBackend;

    fn release(n_number: &str, matched: bool) -> Release {
        Release {
            metadata: ReleaseMetadata::official("2026-07-29", "a".repeat(64)),
            source_manifest_sha256: "b".repeat(64),
            target_set_sha256: "c".repeat(64),
            master: MemberProvenance {
                member_name: "MASTER.txt".to_string(),
                sha256: "d".repeat(64),
            },
            aircraft_reference: MemberProvenance {
                member_name: "ACFTREF.txt".to_string(),
                sha256: "e".repeat(64),
            },
            engine_reference: MemberProvenance {
                member_name: "ENGINE.txt".to_string(),
                sha256: "f".repeat(64),
            },
            coverage: vec![TargetCoverage {
                n_number: n_number.to_string(),
                matched,
            }],
            aircraft: matched
                .then(|| AircraftRecord {
                    n_number: n_number.to_string(),
                    manufacturer_serial_raw: Some("18256025".to_string()),
                    manufacturer_serial_key: Some("18256025".to_string()),
                    aircraft_code: "2072718".to_string(),
                    engine_code: None,
                    year_manufactured: Some(1965),
                    source_record_sha256: "1".repeat(64),
                })
                .into_iter()
                .collect(),
            aircraft_references: matched
                .then(|| AircraftReference {
                    aircraft_code: "2072718".to_string(),
                    manufacturer_name: Some("CESSNA".to_string()),
                    model_name: Some("182H".to_string()),
                    aircraft_type_code: None,
                    engine_type_code: None,
                    category_code: None,
                    certification_indicator_code: None,
                    engine_count: Some(1),
                    seat_count: Some(4),
                    weight_class_code: None,
                    cruise_speed_mph: None,
                    type_certificate_data_sheet: Some("3A13".to_string()),
                    type_certificate_holder: Some("Textron Aviation Inc.".to_string()),
                })
                .into_iter()
                .collect(),
            engine_references: Vec::new(),
        }
    }

    async fn insert_listing(db: &AppDb, registration: &str, serial: Option<&str>) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let variant_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let user_id: i64 = sqlx::query_scalar("SELECT id FROM users ORDER BY id LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap();
        sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours,
              registration_number, serial_number, ingestion_state
            ) VALUES (?, ?, 'https://example.test/aircraft-verification',
                      1965, 100000, 2000, ?, ?, 'pending_review')
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .bind(user_id)
        .bind(registration)
        .bind(serial)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn preflight_rejects_before_network_when_faa_snapshot_is_missing() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = insert_listing(&db, "N1925X", Some("18256025")).await;

        let outcome = preflight_listing_aircraft_verification(&db, listing_id)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            AircraftVerificationOutcome::Rejected {
                reason_code: "registry_snapshot_unavailable".to_string(),
                faa_n_number: None,
                faa_snapshot_id: None,
            }
        );
    }

    #[tokio::test]
    async fn preflight_rejects_faa_absence_without_grounding_services() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = insert_listing(&db, "N182PF", None).await;
        store_release(&db, &release("N182PF", false)).await.unwrap();

        let outcome = apply_listing_aircraft_verification(&db, listing_id, None)
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            AircraftVerificationOutcome::Rejected {
                reason_code,
                faa_n_number: Some(ref n_number),
                ..
            } if reason_code == "registration_not_found" && n_number == "N182PF"
        ));
    }

    #[tokio::test]
    async fn exact_faa_match_without_source_or_catalog_remains_pending_and_free() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = insert_listing(&db, "N1925X", Some("18256025")).await;
        store_release(&db, &release("N1925X", true)).await.unwrap();

        let outcome = apply_listing_aircraft_verification(&db, listing_id, None)
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            AircraftVerificationOutcome::Pending {
                reason: AircraftVerificationPendingReason::SourceEvidenceMissing,
                ..
            }
        ));
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let assignments: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM aircraft_sale_listing_identity_assignments")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(assignments, 0);
    }

    #[test]
    fn current_and_reused_assignments_have_distinct_typed_methods() {
        let assignment = CanonicalAircraftIdentityAssignment {
            assignment_id: 9,
            aircraft_sale_listing_id: 3,
            supersedes_assignment_id: None,
            aircraft_make_id: 1,
            make_name: "Cessna".to_string(),
            aircraft_model_family_id: 2,
            family_name: "Skylane".to_string(),
            aircraft_designation_id: 3,
            official_designation: "182H".to_string(),
            aircraft_generation_id: None,
            aircraft_factory_package_id: None,
            identity_decision_id: 4,
            identity_evidence_claim_id: 5,
            faa_registry_snapshot_id: 6,
            faa_n_number: "N1925X".to_string(),
            faa_aircraft_code: "2072718".to_string(),
            faa_source_record_sha256: "1".repeat(64),
            created_at: "2026-07-29 00:00:00".to_string(),
        };
        let current = verified(
            AircraftVerificationMethod::CurrentAssignment,
            assignment.clone(),
            0,
            true,
        );
        let reused = verified(
            AircraftVerificationMethod::ApprovedCatalog,
            assignment,
            0,
            false,
        );
        assert!(matches!(
            current,
            AircraftVerificationOutcome::Verified {
                method: AircraftVerificationMethod::CurrentAssignment,
                idempotent_replay: true,
                ..
            }
        ));
        assert!(matches!(
            reused,
            AircraftVerificationOutcome::Verified {
                method: AircraftVerificationMethod::ApprovedCatalog,
                idempotent_replay: false,
                ..
            }
        ));
    }
}
