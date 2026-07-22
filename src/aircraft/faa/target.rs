use std::collections::BTreeSet;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::db::{AppDb, DatabaseBackend};

use super::normalize_n_number;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListingTargets {
    /// Sorted and deduplicated valid U.S. N-numbers from both source groups.
    pub n_numbers: Vec<String>,
    pub listing_counts: ListingTargetCounts,
    pub pending_submission_counts: PendingSubmissionTargetCounts,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListingTargetCounts {
    pub row_count: usize,
    pub accepted_registration_count: usize,
    pub missing_registration_count: usize,
    pub foreign_registration_count: usize,
    pub invalid_n_number_count: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingSubmissionTargetCounts {
    /// Submissions that still need identity coverage at collection time: either
    /// unlinked, or linked to a canonical listing whose registration is blank.
    pub row_count: usize,
    pub accepted_registration_count: usize,
    pub missing_payload_count: usize,
    pub malformed_json_count: usize,
    pub missing_registration_count: usize,
    pub foreign_registration_count: usize,
    pub invalid_n_number_count: usize,
}

/// Operator-supplied N-numbers used to extend a target-scoped FAA projection.
///
/// `requested` preserves the exact CLI inputs for the dry-run report, while
/// `accepted` is canonical, sorted, and deduplicated. Invalid requests fail the
/// import instead of being silently discarded.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExplicitNNumberTargets {
    pub requested: Vec<String>,
    pub accepted: Vec<String>,
}

impl ExplicitNNumberTargets {
    pub fn parse(requested: Vec<String>) -> Result<Self> {
        let mut accepted = BTreeSet::new();
        for value in &requested {
            let Some(n_number) = normalize_n_number(value) else {
                bail!("invalid --include-n-number value {value:?}: expected a valid U.S. N-number");
            };
            accepted.insert(n_number);
        }

        Ok(Self {
            requested,
            accepted: accepted.into_iter().collect(),
        })
    }
}

/// Complete sorted target set for one FAA projection, with source accounting.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FaaImportTargets {
    pub n_numbers: Vec<String>,
    pub listing_targets: ListingTargets,
    pub explicit_targets: ExplicitNNumberTargets,
}

impl FaaImportTargets {
    pub fn merge(
        listing_targets: ListingTargets,
        explicit_targets: ExplicitNNumberTargets,
    ) -> Self {
        let n_numbers = listing_targets
            .n_numbers
            .iter()
            .chain(&explicit_targets.accepted)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        Self {
            n_numbers,
            listing_targets,
            explicit_targets,
        }
    }
}

/// Collects the deterministic target set for a privacy-minimized FAA import.
///
/// Valid candidates come from canonical listing registrations and extracted
/// JSON on submissions that either have no canonical listing or are linked to a
/// canonical listing whose registration is still blank. It reads both sources
/// but never changes, admits, rejects, or deletes their rows.
pub async fn listing_targets(db: &AppDb) -> Result<ListingTargets> {
    let listing_sql = db.sql(
        r#"
        SELECT registration_number
        FROM aircraft_sale_listings
        ORDER BY id
        "#,
    );
    let submission_sql = db.sql(
        r#"
        SELECT submission.extracted_listing_json
        FROM plugin_submissions submission
        LEFT JOIN aircraft_sale_listings listing
          ON listing.id = submission.canonical_listing_id
        WHERE submission.canonical_listing_id IS NULL
           OR listing.registration_number IS NULL
           OR TRIM(listing.registration_number) = ''
        ORDER BY submission.id
        "#,
    );
    let (registrations, pending_submission_payloads) = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let registrations = sqlx::query_scalar::<_, Option<String>>(&listing_sql)
                .fetch_all(pool)
                .await?;
            let submissions = sqlx::query_scalar::<_, Option<String>>(&submission_sql)
                .fetch_all(pool)
                .await?;
            (registrations, submissions)
        }
        DatabaseBackend::Postgres(pool) => {
            let registrations = sqlx::query_scalar::<_, Option<String>>(&listing_sql)
                .fetch_all(pool)
                .await?;
            let submissions = sqlx::query_scalar::<_, Option<String>>(&submission_sql)
                .fetch_all(pool)
                .await?;
            (registrations, submissions)
        }
    };

    Ok(collect_targets(registrations, pending_submission_payloads))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RegistrationCandidate {
    Accepted(String),
    Missing,
    Foreign,
    InvalidNNumber,
}

fn classify_registration(registration: Option<&str>) -> RegistrationCandidate {
    let Some(registration) = registration
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return RegistrationCandidate::Missing;
    };
    if let Some(n_number) = normalize_n_number(registration) {
        return RegistrationCandidate::Accepted(n_number);
    }
    let starts_like_n_number = registration
        .chars()
        .find(|character| !character.is_ascii_whitespace() && *character != '-')
        .is_some_and(|character| character.eq_ignore_ascii_case(&'N'));
    if starts_like_n_number {
        RegistrationCandidate::InvalidNNumber
    } else {
        RegistrationCandidate::Foreign
    }
}

fn collect_targets(
    registrations: Vec<Option<String>>,
    pending_submission_payloads: Vec<Option<String>>,
) -> ListingTargets {
    let mut n_numbers = BTreeSet::new();
    let mut listing_counts = ListingTargetCounts {
        row_count: registrations.len(),
        ..ListingTargetCounts::default()
    };
    for registration in registrations {
        match classify_registration(registration.as_deref()) {
            RegistrationCandidate::Accepted(n_number) => {
                listing_counts.accepted_registration_count += 1;
                n_numbers.insert(n_number);
            }
            RegistrationCandidate::Missing => listing_counts.missing_registration_count += 1,
            RegistrationCandidate::Foreign => listing_counts.foreign_registration_count += 1,
            RegistrationCandidate::InvalidNNumber => listing_counts.invalid_n_number_count += 1,
        }
    }

    let mut pending_submission_counts = PendingSubmissionTargetCounts {
        row_count: pending_submission_payloads.len(),
        ..PendingSubmissionTargetCounts::default()
    };
    for payload in pending_submission_payloads {
        let Some(payload) = payload
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            pending_submission_counts.missing_payload_count += 1;
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
            pending_submission_counts.malformed_json_count += 1;
            continue;
        };
        let Some(object) = value.as_object() else {
            pending_submission_counts.malformed_json_count += 1;
            continue;
        };
        let candidate = match object.get("registration_number") {
            None | Some(serde_json::Value::Null) => RegistrationCandidate::Missing,
            Some(serde_json::Value::String(registration)) => {
                classify_registration(Some(registration))
            }
            Some(_) => RegistrationCandidate::InvalidNNumber,
        };
        match candidate {
            RegistrationCandidate::Accepted(n_number) => {
                pending_submission_counts.accepted_registration_count += 1;
                n_numbers.insert(n_number);
            }
            RegistrationCandidate::Missing => {
                pending_submission_counts.missing_registration_count += 1
            }
            RegistrationCandidate::Foreign => {
                pending_submission_counts.foreign_registration_count += 1
            }
            RegistrationCandidate::InvalidNNumber => {
                pending_submission_counts.invalid_n_number_count += 1
            }
        }
    }

    ListingTargets {
        n_numbers: n_numbers.into_iter().collect(),
        listing_counts,
        pending_submission_counts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing_targets_fixture() -> ListingTargets {
        ListingTargets {
            n_numbers: vec!["N123AB".to_string(), "N99999".to_string()],
            listing_counts: ListingTargetCounts {
                row_count: 4,
                accepted_registration_count: 2,
                missing_registration_count: 1,
                foreign_registration_count: 1,
                invalid_n_number_count: 0,
            },
            pending_submission_counts: PendingSubmissionTargetCounts::default(),
        }
    }

    #[test]
    fn explicit_targets_are_normalized_sorted_and_deduplicated() {
        let targets = ExplicitNNumberTargets::parse(vec![
            " n-1925 x ".to_string(),
            "N123AB".to_string(),
            "n1925x".to_string(),
        ])
        .unwrap();

        assert_eq!(targets.requested, [" n-1925 x ", "N123AB", "n1925x"]);
        assert_eq!(targets.accepted, ["N123AB", "N1925X"]);
    }

    #[test]
    fn explicit_targets_reject_invalid_or_foreign_registrations() {
        let error = ExplicitNNumberTargets::parse(vec!["C-GABC".to_string()]).unwrap_err();
        assert!(error.to_string().contains("invalid --include-n-number"));

        let error = ExplicitNNumberTargets::parse(vec!["N12I".to_string()]).unwrap_err();
        assert!(error.to_string().contains("invalid --include-n-number"));
    }

    #[test]
    fn import_targets_merge_and_dedupe_listing_and_explicit_sources() {
        let explicit =
            ExplicitNNumberTargets::parse(vec!["N1925X".to_string(), "n-123ab".to_string()])
                .unwrap();
        let targets = FaaImportTargets::merge(listing_targets_fixture(), explicit);

        assert_eq!(targets.n_numbers, ["N123AB", "N1925X", "N99999"]);
        assert_eq!(targets.listing_targets.listing_counts.row_count, 4);
        assert_eq!(targets.explicit_targets.accepted, ["N123AB", "N1925X"]);
    }

    #[test]
    fn pending_submission_candidates_are_counted_and_never_silently_admitted() {
        let targets = collect_targets(
            vec![
                Some("N123AB".to_string()),
                Some("n-99999".to_string()),
                None,
                Some("C-GABC".to_string()),
                Some("N12I".to_string()),
            ],
            vec![
                None,
                Some("{".to_string()),
                Some("{}".to_string()),
                Some(r#"{"registration_number":"C-GXYZ"}"#.to_string()),
                Some(r#"{"registration_number":"N12O"}"#.to_string()),
                Some(r#"{"registration_number":123}"#.to_string()),
                Some(r#"{"registration_number":"n-1925 x"}"#.to_string()),
                Some(r#"{"registration_number":"N123AB"}"#.to_string()),
                Some("[]".to_string()),
            ],
        );

        assert_eq!(targets.n_numbers, ["N123AB", "N1925X", "N99999"]);
        assert_eq!(
            targets.listing_counts,
            ListingTargetCounts {
                row_count: 5,
                accepted_registration_count: 2,
                missing_registration_count: 1,
                foreign_registration_count: 1,
                invalid_n_number_count: 1,
            }
        );
        assert_eq!(
            targets.pending_submission_counts,
            PendingSubmissionTargetCounts {
                row_count: 9,
                accepted_registration_count: 2,
                missing_payload_count: 1,
                malformed_json_count: 2,
                missing_registration_count: 1,
                foreign_registration_count: 1,
                invalid_n_number_count: 2,
            }
        );
    }

    #[tokio::test]
    async fn collector_includes_unlinked_and_linked_blank_identity_submissions_only() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test database is SQLite")
        };

        let manufacturer_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES ('Target Test Aircraft', 'target-test-aircraft') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let model_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name) VALUES (?, '182', '182') RETURNING id",
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let variant_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name) VALUES (?, '182H', '182h') RETURNING id",
        )
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let listing_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, model_year,
              asking_price_usd, registration_number, airframe_hours
            )
            VALUES (?, ?, 1965, 189000, 'N111AA', 3130)
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let blank_identity_listing_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, model_year,
              asking_price_usd, registration_number, airframe_hours
            )
            VALUES (?, ?, 1965, 189000, NULL, 3130)
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let install_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'test-key') RETURNING id",
        )
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json
            )
            VALUES (?, ?, 'https://example.test/pending', '', 'hash-1', 'signature',
                    '{"registration_number":"n-1925x"}')
            "#,
        )
        .bind(user.id)
        .bind(install_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              canonical_listing_id
            )
            VALUES (?, ?, 'https://example.test/blank-identity', '', 'hash-blank', 'signature',
                    '{"registration_number":"N182KW"}', ?)
            "#,
        )
        .bind(user.id)
        .bind(install_id)
        .bind(blank_identity_listing_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              canonical_listing_id
            )
            VALUES (?, ?, 'https://example.test/canonical', '', 'hash-2', 'signature',
                    '{"registration_number":"N54321"}', ?)
            "#,
        )
        .bind(user.id)
        .bind(install_id)
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json
            )
            VALUES (?, ?, 'https://example.test/malformed', '', 'hash-3', 'signature', '{')
            "#,
        )
        .bind(user.id)
        .bind(install_id)
        .execute(pool)
        .await
        .unwrap();

        let targets = listing_targets(&db).await.unwrap();

        assert_eq!(targets.n_numbers, ["N111AA", "N182KW", "N1925X"]);
        assert_eq!(targets.listing_counts.row_count, 2);
        assert_eq!(targets.listing_counts.accepted_registration_count, 1);
        assert_eq!(targets.listing_counts.missing_registration_count, 1);
        assert_eq!(targets.pending_submission_counts.row_count, 3);
        assert_eq!(
            targets
                .pending_submission_counts
                .accepted_registration_count,
            2
        );
        assert_eq!(targets.pending_submission_counts.malformed_json_count, 1);
        assert!(
            !targets.n_numbers.iter().any(|value| value == "N54321"),
            "a submission linked to an already-identified listing must not be treated as pending"
        );
    }
}
