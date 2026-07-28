use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::File;
use std::path::PathBuf;

use aircost_rs::aircraft::catalog::AircraftHierarchy;
use aircost_rs::aircraft::curation::persistence::{
    persist_reviewable_aircraft_hierarchy, PersistReviewableAircraftHierarchy,
};
use aircost_rs::aircraft::curation::workflow::{
    curate_aircraft_hierarchy_observations_with_config,
    curate_aircraft_hierarchy_observations_with_operator_tcds, AircraftHierarchyCurationCaseReport,
    AircraftHierarchyCurationReport,
};
use aircost_rs::aircraft::enrich_aircraft_specs_from_plugin_submissions;
use aircost_rs::aircraft::faa::{
    drs::{parse_operator_supplied_current_tcds, CurrentTcdsMetadata, TcdsDocument},
    listing_targets, parse_release, require_listing_faa_admission, store_release,
    AircraftGrounding, Eligibility, ExplicitNNumberTargets, FaaImportTargets, ReleaseMetadata,
    ReleaseReaders,
};
use aircost_rs::aircraft::identity::{
    ensure_listing_identity_assignment_from_approved_catalog,
    CanonicalAircraftCompatibilityIdentity, CanonicalAircraftIdentityAssignment,
    EnsureIdentityAssignmentOutcome,
};
use aircost_rs::aircraft::observations::{
    load_aircraft_identity_observations, AircraftIdentityObservation,
};
use aircost_rs::avionics::consolidation::{
    audit_avionics_catalog_duplicates, consolidate_avionics_models,
    plan_canonical_legacy_duplicates, preview_avionics_model_consolidation,
};
use aircost_rs::avionics::repopulate::{
    preflight_listing_avionics_repopulation, repopulate_listing_avionics,
    AvionicsRepopulationExecutionMode, AvionicsRepopulationScope,
};
use aircost_rs::avionics::{
    curate_avionics_models_with_gemini, enrich_listing_avionics_metadata,
    enrich_missing_avionics_metadata, enrich_model_year_avionics_and_price_points,
};
use aircost_rs::cleanup::cleanup_orphan_records;
use aircost_rs::db::{database_url_from_arg, DEFAULT_DATABASE_PATH};
use aircost_rs::extract::GeminiListingExtractor;
use aircost_rs::fit::{fit_depreciation_profiles, fit_structural_valuation};
use aircost_rs::gemini::benchmark::{
    execute as execute_gemini_benchmark, load_suite as load_gemini_benchmark_suite,
    BenchmarkPricing, BenchmarkSelection, BenchmarkTaskKind,
};
use aircost_rs::gemini::config::{GeminiRuntimeConfig, GeminiTask};
use aircost_rs::gemini::interactions::GeminiInteractionsClient;
use aircost_rs::gemini::live_benchmark::LiveBenchmarkRunner;
use aircost_rs::gemini::usage::Store as GeminiUsageStore;
use aircost_rs::listing::backfill::{default_stage_limit, stage_legacy_listing_reviews};
#[cfg(feature = "dnn")]
use aircost_rs::valuation::dataset::load_snapshot;
use aircost_rs::valuation::dataset::{create_snapshot, SnapshotPolicy};
#[cfg(feature = "dnn")]
use aircost_rs::valuation::dnn::{
    evaluate_candidate_gates, fit_dnn_candidate, persist_dnn_candidate, structural_baseline_config,
    structural_baseline_id, DnnFitConfig,
};
use aircost_rs::valuation::store::{activate_model_version, validate_model_version};
use anyhow::{bail, Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let command = parse_args(env::args().skip(1))?;
    match command {
        AdminCommand::ImportFaaRegistry {
            database,
            master,
            aircraft_reference,
            engine_reference,
            snapshot_date,
            archive_sha256,
            explicit_targets,
            apply,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let targets = FaaImportTargets::merge(listing_targets(&db).await?, explicit_targets);
            if targets.n_numbers.is_empty() {
                bail!(
                    "the database and --include-n-number arguments have no valid N-number targets for an FAA import"
                );
            }
            let parse_targets = targets.n_numbers.clone();
            let release = tokio::task::spawn_blocking(move || -> Result<_> {
                let master_file = File::open(&master).with_context(|| {
                    format!("could not open FAA MASTER file {}", master.display())
                })?;
                let aircraft_file = File::open(&aircraft_reference).with_context(|| {
                    format!(
                        "could not open FAA ACFTREF file {}",
                        aircraft_reference.display()
                    )
                })?;
                let engine_file = File::open(&engine_reference).with_context(|| {
                    format!(
                        "could not open FAA ENGINE file {}",
                        engine_reference.display()
                    )
                })?;
                parse_release(
                    ReleaseMetadata::official(snapshot_date, archive_sha256),
                    ReleaseReaders::new(master_file, aircraft_file, engine_file),
                    &parse_targets,
                )
            })
            .await
            .context("FAA registry parser task failed")??;
            let stored = if apply {
                Some(store_release(&db, &release).await?)
            } else {
                None
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "dry_run": !apply,
                    "listing_targets": targets.listing_targets,
                    "explicit_targets": targets.explicit_targets,
                    "snapshot_date": release.metadata.snapshot_date,
                    "source_url": release.metadata.source_url,
                    "archive_sha256": release.metadata.archive_sha256,
                    "source_manifest_sha256": release.source_manifest_sha256,
                    "target_set_sha256": release.target_set_sha256,
                    "member_sha256": {
                        "master": release.master.sha256,
                        "aircraft_reference": release.aircraft_reference.sha256,
                        "engine_reference": release.engine_reference.sha256,
                    },
                    "target_count": release.coverage.len(),
                    "matched_count": release.aircraft.len(),
                    "absent_count": release.coverage.iter().filter(|row| !row.matched).count(),
                    "aircraft_reference_count": release.aircraft_references.len(),
                    "engine_reference_count": release.engine_references.len(),
                    "stored": stored,
                    "canonical_catalog_writes": 0,
                }))?
            );
        }
        AdminCommand::CurateAircraftHierarchy {
            database,
            listing_limit,
            cluster_limit,
            listing_id,
            operator_tcds,
            apply,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let api_key = env::var("GEMINI_API_KEY")
                .context("GEMINI_API_KEY is required for curate-aircraft-hierarchy")?;
            let runtime_config = GeminiRuntimeConfig::from_environment()?;
            let client = GeminiInteractionsClient::new(api_key)?
                .with_usage_store(GeminiUsageStore::new(&db));
            let supplied_tcds = match operator_tcds.as_ref() {
                Some(input) => Some(load_operator_tcds(&db, listing_id, input).await?),
                None => None,
            };
            let mut report = match (listing_id, supplied_tcds.as_ref()) {
                (Some(listing_id), Some(document)) => {
                    curate_aircraft_hierarchy_observations_with_operator_tcds(
                        &db,
                        &client,
                        listing_id,
                        &runtime_config,
                        document,
                    )
                    .await?
                }
                (_, None) => {
                    curate_aircraft_hierarchy_observations_with_config(
                        &db,
                        &client,
                        listing_limit,
                        listing_id,
                        cluster_limit,
                        &runtime_config,
                    )
                    .await?
                }
                (None, Some(_)) => unreachable!("operator TCDS parser requires listing id"),
            };
            let application = if apply {
                apply_reviewable_aircraft_hierarchies(&db, &report, listing_limit, listing_id)
                    .await?
            } else {
                AircraftHierarchyApplicationReport::dry_run()
            };
            report.canonical_catalog_writes = application.canonical_catalog_writes;
            let mut output = serde_json::to_value(&report)?;
            let output_object = output
                .as_object_mut()
                .context("aircraft curation report did not serialize as an object")?;
            output_object.insert("dry_run".to_string(), serde_json::json!(!apply));
            output_object.insert(
                "application".to_string(),
                serde_json::to_value(application)?,
            );
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        AdminCommand::BenchmarkGemini {
            database,
            config,
            listing_limit,
            max_avionics_per_listing,
            max_visual_assets,
            seed,
            tasks,
            models,
            submission_ids,
            execute,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let runtime_config = match config {
                Some(path) => GeminiRuntimeConfig::from_path(path)?,
                None => GeminiRuntimeConfig::from_environment()?,
            };
            let selection = resolve_benchmark_selection(
                &runtime_config,
                listing_limit,
                seed,
                max_avionics_per_listing,
                max_visual_assets,
                submission_ids,
            )?;
            let suite = load_gemini_benchmark_suite(&db, &selection).await?;
            if !execute {
                println!("{}", serde_json::to_string_pretty(&suite)?);
            } else {
                let runner = LiveBenchmarkRunner::from_environment(&db, runtime_config.clone())?;
                let pricing = BenchmarkPricing::official_standard_defaults();
                let mut reports = Vec::new();
                for task in tasks {
                    let mut task_suite = suite.clone();
                    task_suite.cases.retain(|case| case.task == task);
                    let task_models = if models.is_empty() {
                        benchmark_models_for_task(&runtime_config, task)?
                    } else {
                        models.clone()
                    };
                    let report =
                        execute_gemini_benchmark(&task_suite, &task_models, &runner, &pricing)
                            .await?;
                    reports.push(report);
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "selection": selection,
                        "selected_submission_ids": suite.selected_submission_ids,
                        "domain_writes": 0,
                        "usage_accounting_writes": true,
                        "reports": reports,
                    }))?
                );
            }
        }
        AdminCommand::RepopulateAvionics {
            database,
            mode,
            limit,
            listing_id,
            after_listing_id,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let scope = AvionicsRepopulationScope::new(limit, listing_id, after_listing_id);
            match mode {
                AvionicsRepopulationCommandMode::Preflight => {
                    let report = preflight_listing_avionics_repopulation(&db, &scope).await?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                AvionicsRepopulationCommandMode::Preview
                | AvionicsRepopulationCommandMode::Apply => {
                    let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
                    let execution_mode = match mode {
                        AvionicsRepopulationCommandMode::Preview => {
                            AvionicsRepopulationExecutionMode::Preview
                        }
                        AvionicsRepopulationCommandMode::Apply => {
                            AvionicsRepopulationExecutionMode::Apply
                        }
                        AvionicsRepopulationCommandMode::Preflight => unreachable!(),
                    };
                    let report =
                        repopulate_listing_avionics(&db, &extractor, execution_mode, &scope)
                            .await?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
            }
        }
        AdminCommand::StageListingReviews {
            database,
            apply,
            limit,
            listing_id,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let report = stage_legacy_listing_reviews(&db, apply, limit, listing_id).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::AuditAvionicsDuplicates { database } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let audit = audit_avionics_catalog_duplicates(&db).await?;
            println!("{}", serde_json::to_string_pretty(&audit)?);
        }
        AdminCommand::ConsolidateLegacyAvionics { database, apply } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let before = audit_avionics_catalog_duplicates(&db).await?;
            let model_plans = plan_canonical_legacy_duplicates(&db).await?;
            let mut model_previews = Vec::with_capacity(model_plans.len());
            for plan in &model_plans {
                model_previews
                    .push(preview_avionics_model_consolidation(&db, &plan.request).await?);
            }
            if apply {
                let blockers = model_previews
                    .iter()
                    .filter(|report| !report.can_apply)
                    .flat_map(|report| {
                        report.blockers.iter().map(move |blocker| {
                            format!("survivor {}: {blocker}", report.survivor.id)
                        })
                    })
                    .collect::<Vec<_>>();
                if !blockers.is_empty() {
                    bail!(
                        "legacy avionics consolidation preflight failed; no product rows were changed: {}",
                        blockers.join("; ")
                    );
                }
            }
            let mut model_reports = model_previews;
            if apply {
                model_reports.clear();
                for plan in &model_plans {
                    model_reports.push(consolidate_avionics_models(&db, &plan.request).await?);
                }
            }

            let after = audit_avionics_catalog_duplicates(&db).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "dry_run": !apply,
                    "applied": apply,
                    "before": before,
                    "model_plans": model_plans,
                    "model_reports": model_reports,
                    "after": after,
                    "mutation_rule": "unreviewed legacy products only; every pair in an automatically applied component must directly share the same non-null stable-identifier kind and normalized value inside one evidence-authorized manufacturer identity scope; raw manufacturer rows are immutable history and are never reparented or deleted",
                }))?
            );
        }
        AdminCommand::EnrichAvionics {
            database,
            apply,
            limit,
            value_reference_year,
            refresh_existing,
            listing_id,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
            let report = if let Some(listing_id) = listing_id {
                enrich_listing_avionics_metadata(
                    &db,
                    &extractor,
                    apply,
                    listing_id,
                    value_reference_year,
                    refresh_existing,
                )
                .await?
            } else {
                enrich_missing_avionics_metadata(
                    &db,
                    &extractor,
                    apply,
                    limit,
                    value_reference_year,
                    refresh_existing,
                )
                .await?
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::CleanupOrphans { database } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let report = cleanup_orphan_records(&db).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::CurateAvionics {
            database,
            apply,
            limit,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
            let report = curate_avionics_models_with_gemini(&db, &extractor, apply, limit).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::EnrichModelYearAvionics {
            database,
            apply,
            limit,
            value_reference_year,
            refresh_existing,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
            let report = enrich_model_year_avionics_and_price_points(
                &db,
                &extractor,
                apply,
                limit,
                value_reference_year,
                refresh_existing,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::EnrichAircraftSpecs {
            database,
            apply,
            limit,
            value_reference_year,
            refresh_existing,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
            let report = enrich_aircraft_specs_from_plugin_submissions(
                &db,
                &extractor,
                apply,
                limit,
                value_reference_year,
                refresh_existing,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::FitDepreciation {
            database,
            apply,
            min_model_samples,
            value_reference_year,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let report =
                fit_depreciation_profiles(&db, apply, min_model_samples, value_reference_year)
                    .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::SnapshotValuations {
            database,
            apply,
            max_age_days,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let policy = SnapshotPolicy {
                max_listing_age_days: max_age_days,
                ..SnapshotPolicy::default()
            };
            let report = create_snapshot(&db, &policy, apply).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::FitValuation {
            database,
            kind,
            snapshot_id,
            apply,
            maximum_epochs,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            match kind.as_str() {
                "structural" => {
                    let report = fit_structural_valuation(&db, snapshot_id, apply).await?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                "dnn" => {
                    #[cfg(not(feature = "dnn"))]
                    {
                        let _ = (db, snapshot_id, apply, maximum_epochs);
                        bail!("DNN fitting requires rebuilding aircost-admin with --features dnn");
                    }
                    #[cfg(feature = "dnn")]
                    {
                        let rows = load_snapshot(&db, snapshot_id).await?;
                        let baseline_model_version_id =
                            structural_baseline_id(&db, snapshot_id).await?;
                        let structural_fit_config =
                            structural_baseline_config(&db, baseline_model_version_id).await?;
                        let mut report = fit_dnn_candidate(
                            &rows,
                            &DnnFitConfig {
                                snapshot_id,
                                baseline_model_version_id,
                                structural_fit_config,
                                maximum_epochs,
                                ..DnnFitConfig::default()
                            },
                        )?;
                        let gate_report = evaluate_candidate_gates(&db, &report).await?;
                        let persisted_model_version_id = if apply {
                            Some(persist_dnn_candidate(&db, &mut report).await?)
                        } else {
                            None
                        };
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "snapshot_id": snapshot_id,
                                "deduplicated_listings": report.artifact.metadata.group_counts.total,
                                "capacity": report.artifact.metadata.capacity,
                                "parameter_count_per_member": report.artifact.metadata.parameter_count_per_member,
                                "training_schedule": report.artifact.metadata.training_schedule,
                                "ensemble_metrics": report.metrics,
                                "activation_gates": gate_report.activation_gates,
                                "persisted_model_version_id": persisted_model_version_id,
                                "dry_run": !apply,
                            }))?
                        );
                    }
                }
                _ => bail!("unknown valuation model kind: {kind}"),
            }
        }
        AdminCommand::ValidateValuation {
            database,
            model_version_id,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let report = validate_model_version(&db, model_version_id).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::ActivateValuation {
            database,
            model_version_id,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let report = activate_model_version(&db, model_version_id).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}

#[derive(Debug)]
enum AdminCommand {
    ImportFaaRegistry {
        database: String,
        master: PathBuf,
        aircraft_reference: PathBuf,
        engine_reference: PathBuf,
        snapshot_date: String,
        archive_sha256: String,
        explicit_targets: ExplicitNNumberTargets,
        apply: bool,
    },
    CurateAircraftHierarchy {
        database: String,
        listing_limit: i64,
        cluster_limit: usize,
        listing_id: Option<i64>,
        operator_tcds: Option<OperatorTcdsInput>,
        apply: bool,
    },
    BenchmarkGemini {
        database: String,
        config: Option<PathBuf>,
        listing_limit: Option<usize>,
        max_avionics_per_listing: usize,
        max_visual_assets: usize,
        seed: Option<String>,
        tasks: Vec<BenchmarkTaskKind>,
        models: Vec<String>,
        submission_ids: Vec<i64>,
        execute: bool,
    },
    RepopulateAvionics {
        database: String,
        mode: AvionicsRepopulationCommandMode,
        limit: i64,
        listing_id: Option<i64>,
        after_listing_id: Option<i64>,
    },
    StageListingReviews {
        database: String,
        apply: bool,
        limit: i64,
        listing_id: Option<i64>,
    },
    AuditAvionicsDuplicates {
        database: String,
    },
    ConsolidateLegacyAvionics {
        database: String,
        apply: bool,
    },
    EnrichAvionics {
        database: String,
        apply: bool,
        limit: i64,
        value_reference_year: Option<i64>,
        refresh_existing: bool,
        listing_id: Option<i64>,
    },
    CleanupOrphans {
        database: String,
    },
    CurateAvionics {
        database: String,
        apply: bool,
        limit: i64,
    },
    EnrichModelYearAvionics {
        database: String,
        apply: bool,
        limit: i64,
        value_reference_year: Option<i64>,
        refresh_existing: bool,
    },
    EnrichAircraftSpecs {
        database: String,
        apply: bool,
        limit: i64,
        value_reference_year: Option<i64>,
        refresh_existing: bool,
    },
    FitDepreciation {
        database: String,
        apply: bool,
        min_model_samples: usize,
        value_reference_year: Option<i64>,
    },
    SnapshotValuations {
        database: String,
        apply: bool,
        max_age_days: i64,
    },
    FitValuation {
        database: String,
        kind: String,
        snapshot_id: i64,
        apply: bool,
        maximum_epochs: usize,
    },
    ValidateValuation {
        database: String,
        model_version_id: i64,
    },
    ActivateValuation {
        database: String,
        model_version_id: i64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OperatorTcdsInput {
    pdf: PathBuf,
    pdf_sha256: String,
    document_guid: String,
    document_id: String,
    tcds_number: String,
    revision_number: Option<String>,
    revision_date: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AvionicsRepopulationCommandMode {
    Preflight,
    Preview,
    Apply,
}

async fn load_operator_tcds(
    db: &aircost_rs::db::AppDb,
    listing_id: Option<i64>,
    input: &OperatorTcdsInput,
) -> Result<TcdsDocument> {
    let listing_id = listing_id.context("operator-supplied FAA TCDS requires --listing-id")?;
    if !input
        .document_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        || input.document_id.is_empty()
    {
        bail!("--faa-drs-document-id contains unsupported characters");
    }
    let grounding = require_listing_faa_admission(db, listing_id)
        .await
        .context("operator TCDS listing did not pass raw FAA registry admission")?;
    let reference = grounding
        .aircraft
        .as_ref()
        .context("operator TCDS listing has no FAA aircraft reference")?;
    let exact_model = reference
        .model_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("operator TCDS listing has no exact FAA model")?;
    if reference
        .type_certificate_data_sheet
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| value != input.tcds_number)
    {
        bail!(
            "operator TCDS number {:?} disagrees with the FAA aircraft reference",
            input.tcds_number
        );
    }
    let metadata = CurrentTcdsMetadata {
        document_guid: input.document_guid.clone(),
        document_url: format!("https://drs.faa.gov/browse/TCDSMODEL/{}", input.document_id),
        tcds_number: input.tcds_number.clone(),
        revision_number: input.revision_number.clone(),
        revision_date: input.revision_date.clone(),
        tc_holder: reference.type_certificate_holder.clone(),
        former_tc_holders: Vec::new(),
        models: vec![exact_model.to_string()],
        exact_model: exact_model.to_string(),
    };
    let source_url = format!(
        "https://drs.faa.gov/api/drs/data-pull/download/{}",
        input.document_guid
    );
    let pdf = std::fs::read(&input.pdf)
        .with_context(|| format!("could not read FAA TCDS PDF {}", input.pdf.display()))?;
    parse_operator_supplied_current_tcds(metadata, source_url, &input.pdf_sha256, pdf)
        .map_err(anyhow::Error::new)
        .context("operator-supplied FAA TCDS failed bounded provenance/PDF validation")
}

#[derive(Debug, serde::Serialize)]
struct AircraftHierarchyApplicationOutcome {
    cluster_key: String,
    listing_id: Option<i64>,
    observation_sha256: Option<String>,
    status: &'static str,
    catalog_writes: usize,
    assignment_id: Option<i64>,
    assignment_status: Option<&'static str>,
    approval_fingerprint: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct AircraftHierarchyApplicationReport {
    requested: bool,
    attempted_observations: usize,
    applied_observations: usize,
    idempotent_observations: usize,
    catalog_reused_observations: usize,
    blocked_outcomes: usize,
    canonical_catalog_writes: usize,
    outcomes: Vec<AircraftHierarchyApplicationOutcome>,
}

impl AircraftHierarchyApplicationReport {
    fn dry_run() -> Self {
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
    /// A paid/reviewable result may create canonical catalog state. It must
    /// therefore remain bound to literal hierarchy labels in retained source.
    ReviewableCatalogWrite,
    /// This path can only assign an already-approved catalog identity. The
    /// exact FAA record and catalog relationship are re-resolved immediately
    /// before assignment, so listing hierarchy prose is not identity evidence.
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
    db: &aircost_rs::db::AppDb,
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

async fn apply_reviewable_aircraft_hierarchies(
    db: &aircost_rs::db::AppDb,
    report: &AircraftHierarchyCurationReport,
    listing_limit: i64,
    listing_id: Option<i64>,
) -> Result<AircraftHierarchyApplicationReport> {
    // Do not trust an observation retained in memory across paid model calls.
    // Persistence receives only a freshly derived observation that still has
    // the exact listing id + fingerprint returned by the curation case.
    let fresh = load_aircraft_identity_observations(db, listing_limit, listing_id)
        .await
        .map_err(|error| anyhow::anyhow!(error))
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

        // The approval decision is cluster-scoped. Reusing the persistence API
        // with another observation would collide with its per-listing
        // validation provenance, so remaining listings use only the exact
        // approved-catalog assignment path.
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
                                approval_fingerprint: Some(
                                    approval_fingerprint.clone(),
                                ),
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

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        print_usage();
        bail!("missing admin command");
    };

    match command.as_str() {
        "import-faa-registry" => parse_import_faa_registry_args(args),
        "curate-aircraft-hierarchy" => parse_curate_aircraft_hierarchy_args(args),
        "benchmark-gemini" => parse_benchmark_gemini_args(args),
        "repopulate-avionics" => parse_repopulate_avionics_args(args),
        "stage-listing-reviews" => parse_stage_listing_reviews_args(args),
        "audit-avionics-duplicates" => parse_audit_avionics_duplicates_args(args),
        "consolidate-legacy-avionics" => parse_consolidate_legacy_avionics_args(args),
        "enrich-avionics" => parse_enrich_avionics_args(args),
        "cleanup-orphans" => parse_cleanup_orphans_args(args),
        "curate-avionics" => parse_curate_avionics_args(args),
        "enrich-model-year-avionics" => parse_enrich_model_year_avionics_args(args),
        "enrich-aircraft-specs" => parse_enrich_aircraft_specs_args(args),
        "fit-depreciation" => parse_fit_depreciation_args(args),
        "snapshot-valuations" => parse_snapshot_valuations_args(args),
        "fit-valuation" => parse_fit_valuation_args(args),
        "validate-valuation" => parse_model_version_args(args, false),
        "activate-valuation" => parse_model_version_args(args, true),
        "--help" | "-h" => {
            print_usage();
            std::process::exit(0);
        }
        _ => bail!("unknown admin command: {command}"),
    }
}

fn parse_import_faa_registry_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut master = None;
    let mut aircraft_reference = None;
    let mut engine_reference = None;
    let mut snapshot_date = None;
    let mut archive_sha256 = None;
    let mut include_n_numbers = Vec::new();
    let mut apply = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--master" => {
                master = Some(PathBuf::from(
                    args.next().context("--master requires a value")?,
                ));
            }
            "--aircraft-reference" => {
                aircraft_reference = Some(PathBuf::from(
                    args.next()
                        .context("--aircraft-reference requires a value")?,
                ));
            }
            "--engine-reference" => {
                engine_reference = Some(PathBuf::from(
                    args.next().context("--engine-reference requires a value")?,
                ));
            }
            "--snapshot-date" => {
                snapshot_date = Some(args.next().context("--snapshot-date requires a value")?);
            }
            "--archive-sha256" => {
                archive_sha256 = Some(args.next().context("--archive-sha256 requires a value")?);
            }
            "--include-n-number" => {
                include_n_numbers.push(args.next().context("--include-n-number requires a value")?);
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown import-faa-registry argument: {arg}"),
        }
    }

    let snapshot_date = snapshot_date.context("--snapshot-date is required")?;
    if snapshot_date.len() != 10
        || snapshot_date.as_bytes().get(4) != Some(&b'-')
        || snapshot_date.as_bytes().get(7) != Some(&b'-')
        || snapshot_date
            .chars()
            .enumerate()
            .any(|(index, character)| index != 4 && index != 7 && !character.is_ascii_digit())
    {
        bail!("--snapshot-date must use YYYY-MM-DD");
    }
    let archive_sha256 = archive_sha256.context("--archive-sha256 is required")?;
    if archive_sha256.len() != 64
        || !archive_sha256
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("--archive-sha256 must be a 64-character hexadecimal digest");
    }
    let explicit_targets = ExplicitNNumberTargets::parse(include_n_numbers)?;

    Ok(AdminCommand::ImportFaaRegistry {
        database: database_url_from_arg(database),
        master: master.context("--master is required")?,
        aircraft_reference: aircraft_reference.context("--aircraft-reference is required")?,
        engine_reference: engine_reference.context("--engine-reference is required")?,
        snapshot_date,
        archive_sha256,
        explicit_targets,
        apply,
    })
}

fn parse_curate_aircraft_hierarchy_args(
    args: impl IntoIterator<Item = String>,
) -> Result<AdminCommand> {
    const DEFAULT_LISTING_LIMIT: i64 = 25;
    const DEFAULT_CLUSTER_LIMIT: usize = 5;

    let mut database = None;
    let mut listing_limit = DEFAULT_LISTING_LIMIT;
    let mut cluster_limit = DEFAULT_CLUSTER_LIMIT;
    let mut listing_id = None;
    let mut faa_drs_pdf = None;
    let mut faa_drs_pdf_sha256 = None;
    let mut faa_drs_document_guid = None;
    let mut faa_drs_document_id = None;
    let mut faa_drs_tcds_number = None;
    let mut faa_drs_revision_number = None;
    let mut faa_drs_revision_date = None;
    let mut apply = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--listing-limit" => {
                let value = args.next().context("--listing-limit requires a value")?;
                listing_limit = value
                    .parse::<i64>()
                    .with_context(|| format!("invalid --listing-limit value: {value}"))?;
            }
            "--cluster-limit" => {
                let value = args.next().context("--cluster-limit requires a value")?;
                cluster_limit = value
                    .parse::<usize>()
                    .with_context(|| format!("invalid --cluster-limit value: {value}"))?;
            }
            "--listing-id" => {
                let value = args.next().context("--listing-id requires a value")?;
                listing_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --listing-id value: {value}"))?,
                );
            }
            "--faa-drs-pdf" => {
                faa_drs_pdf = Some(PathBuf::from(
                    args.next().context("--faa-drs-pdf requires a value")?,
                ));
            }
            "--faa-drs-pdf-sha256" => {
                faa_drs_pdf_sha256 = Some(
                    args.next()
                        .context("--faa-drs-pdf-sha256 requires a value")?,
                );
            }
            "--faa-drs-document-guid" => {
                faa_drs_document_guid = Some(
                    args.next()
                        .context("--faa-drs-document-guid requires a value")?,
                );
            }
            "--faa-drs-document-id" => {
                faa_drs_document_id = Some(
                    args.next()
                        .context("--faa-drs-document-id requires a value")?,
                );
            }
            "--faa-drs-tcds-number" => {
                faa_drs_tcds_number = Some(
                    args.next()
                        .context("--faa-drs-tcds-number requires a value")?,
                );
            }
            "--faa-drs-revision-number" => {
                faa_drs_revision_number = Some(
                    args.next()
                        .context("--faa-drs-revision-number requires a value")?,
                );
            }
            "--faa-drs-revision-date" => {
                faa_drs_revision_date = Some(
                    args.next()
                        .context("--faa-drs-revision-date requires a value")?,
                );
            }
            "--apply" => apply = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown curate-aircraft-hierarchy argument: {arg}"),
        }
    }

    if listing_limit < 1 {
        bail!("--listing-limit must be at least 1");
    }
    if cluster_limit < 1 {
        bail!("--cluster-limit must be at least 1");
    }
    if listing_id.is_some_and(|listing_id| listing_id < 1) {
        bail!("--listing-id must be a positive integer");
    }
    let supplied_tcds_field_count = [
        faa_drs_pdf.is_some(),
        faa_drs_pdf_sha256.is_some(),
        faa_drs_document_guid.is_some(),
        faa_drs_document_id.is_some(),
        faa_drs_tcds_number.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    let operator_tcds = if supplied_tcds_field_count == 0
        && faa_drs_revision_number.is_none()
        && faa_drs_revision_date.is_none()
    {
        None
    } else {
        if supplied_tcds_field_count != 5 {
            bail!(
                "operator TCDS mode requires --faa-drs-pdf, --faa-drs-pdf-sha256, --faa-drs-document-guid, --faa-drs-document-id, and --faa-drs-tcds-number together"
            );
        }
        if listing_id.is_none() {
            bail!("operator TCDS mode requires --listing-id");
        }
        Some(OperatorTcdsInput {
            pdf: faa_drs_pdf.expect("field count checked"),
            pdf_sha256: faa_drs_pdf_sha256.expect("field count checked"),
            document_guid: faa_drs_document_guid.expect("field count checked"),
            document_id: faa_drs_document_id.expect("field count checked"),
            tcds_number: faa_drs_tcds_number.expect("field count checked"),
            revision_number: faa_drs_revision_number,
            revision_date: faa_drs_revision_date,
        })
    };

    Ok(AdminCommand::CurateAircraftHierarchy {
        database: database_url_from_arg(database),
        listing_limit,
        cluster_limit,
        listing_id,
        operator_tcds,
        apply,
    })
}

fn parse_benchmark_gemini_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut config = None;
    let mut listing_limit = None;
    let mut max_avionics_per_listing = 1usize;
    let mut max_visual_assets = 8usize;
    let mut seed = None;
    let mut tasks = Vec::new();
    let mut models = Vec::new();
    let mut submission_ids = Vec::new();
    let mut execute = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--config" => {
                config = Some(PathBuf::from(
                    args.next().context("--config requires a value")?,
                ));
            }
            "--listing-limit" => {
                let value = args.next().context("--listing-limit requires a value")?;
                listing_limit = Some(
                    value
                        .parse::<usize>()
                        .with_context(|| format!("invalid --listing-limit value: {value}"))?,
                );
            }
            "--max-avionics-per-listing" => {
                let value = args
                    .next()
                    .context("--max-avionics-per-listing requires a value")?;
                max_avionics_per_listing = value.parse::<usize>().with_context(|| {
                    format!("invalid --max-avionics-per-listing value: {value}")
                })?;
            }
            "--max-visual-assets" => {
                let value = args
                    .next()
                    .context("--max-visual-assets requires a value")?;
                max_visual_assets = value
                    .parse::<usize>()
                    .with_context(|| format!("invalid --max-visual-assets value: {value}"))?;
            }
            "--seed" => seed = Some(args.next().context("--seed requires a value")?),
            "--task" => {
                let value = args.next().context("--task requires a value")?;
                tasks.push(parse_benchmark_task(&value)?);
            }
            "--model" => {
                models.push(args.next().context("--model requires a value")?);
            }
            "--submission-id" => {
                let value = args.next().context("--submission-id requires a value")?;
                let id = value
                    .parse::<i64>()
                    .with_context(|| format!("invalid --submission-id value: {value}"))?;
                if id < 1 {
                    bail!("--submission-id must be positive");
                }
                submission_ids.push(id);
            }
            "--execute" => execute = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown benchmark-gemini argument: {arg}"),
        }
    }

    if tasks.is_empty() {
        tasks = vec![
            BenchmarkTaskKind::ListingExtraction,
            BenchmarkTaskKind::GroundedMetadata,
            BenchmarkTaskKind::AvionicsGroundingReview,
            BenchmarkTaskKind::VisualIdentity,
        ];
    }
    let mut unique_tasks = BTreeSet::new();
    tasks.retain(|task| unique_tasks.insert(*task));
    let mut unique_models = BTreeSet::new();
    models.retain(|model| unique_models.insert(model.trim().to_string()));
    let mut unique_submissions = BTreeSet::new();
    submission_ids.retain(|id| unique_submissions.insert(*id));

    let defaults = BenchmarkSelection::default();
    BenchmarkSelection {
        seed: seed.clone().unwrap_or(defaults.seed),
        listing_limit: listing_limit.unwrap_or(defaults.listing_limit),
        listing_ids: Vec::new(),
        submission_ids: Vec::new(),
        max_avionics_per_listing,
        max_visual_assets,
    }
    .validate()?;

    Ok(AdminCommand::BenchmarkGemini {
        database: database_url_from_arg(database),
        config,
        listing_limit,
        max_avionics_per_listing,
        max_visual_assets,
        seed,
        tasks,
        models,
        submission_ids,
        execute,
    })
}

fn resolve_benchmark_selection(
    config: &GeminiRuntimeConfig,
    listing_limit: Option<usize>,
    seed: Option<String>,
    max_avionics_per_listing: usize,
    max_visual_assets: usize,
    submission_ids: Vec<i64>,
) -> Result<BenchmarkSelection> {
    let listing_ids = if submission_ids.is_empty() {
        config.benchmark.listing_ids.clone()
    } else {
        Vec::new()
    };
    let selection = BenchmarkSelection {
        seed: seed.unwrap_or_else(|| config.benchmark.seed.to_string()),
        listing_limit: listing_limit.unwrap_or(config.benchmark.sample_size),
        listing_ids,
        submission_ids,
        max_avionics_per_listing,
        max_visual_assets,
    };
    selection.validate()?;
    Ok(selection)
}

fn parse_benchmark_task(value: &str) -> Result<BenchmarkTaskKind> {
    match value.trim() {
        "listing" | "listing-extraction" | "listing_extraction" => {
            Ok(BenchmarkTaskKind::ListingExtraction)
        }
        "metadata" | "grounded-metadata" | "grounded_metadata" => {
            Ok(BenchmarkTaskKind::GroundedMetadata)
        }
        "avionics" | "avionics-grounding-review" | "avionics_grounding_review" => {
            Ok(BenchmarkTaskKind::AvionicsGroundingReview)
        }
        "visual" | "visual-identity" | "visual_identity" => Ok(BenchmarkTaskKind::VisualIdentity),
        _ => bail!("unknown benchmark task {value:?}; use listing, metadata, avionics, or visual"),
    }
}

fn benchmark_models_for_task(
    config: &GeminiRuntimeConfig,
    task: BenchmarkTaskKind,
) -> Result<Vec<String>> {
    let route_task = match task {
        BenchmarkTaskKind::ListingExtraction => GeminiTask::ListingExtraction,
        BenchmarkTaskKind::GroundedMetadata => GeminiTask::GroundedMetadata,
        BenchmarkTaskKind::AvionicsGroundingReview => GeminiTask::AvionicsIdentity,
        BenchmarkTaskKind::VisualIdentity => GeminiTask::AircraftVisualIdentity,
    };
    let mut models = Vec::new();
    for variant in config.benchmark_variants(route_task)? {
        if !models.contains(&variant.route.model) {
            models.push(variant.route.model);
        }
    }
    Ok(models)
}

fn parse_repopulate_avionics_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut requested_mode = None;
    let mut limit = 10_i64;
    let mut listing_id = None;
    let mut after_listing_id = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--preflight" => set_repopulation_mode(
                &mut requested_mode,
                AvionicsRepopulationCommandMode::Preflight,
                "--preflight",
            )?,
            "--dry-run" | "--preview" => set_repopulation_mode(
                &mut requested_mode,
                AvionicsRepopulationCommandMode::Preview,
                arg.as_str(),
            )?,
            "--apply" => set_repopulation_mode(
                &mut requested_mode,
                AvionicsRepopulationCommandMode::Apply,
                "--apply",
            )?,
            "--limit" => {
                let value = args.next().context("--limit requires a value")?;
                limit = value
                    .parse::<i64>()
                    .with_context(|| format!("invalid --limit value: {value}"))?;
            }
            "--listing-id" => {
                let value = args.next().context("--listing-id requires a value")?;
                listing_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --listing-id value: {value}"))?,
                );
            }
            "--after-listing-id" => {
                let value = args.next().context("--after-listing-id requires a value")?;
                after_listing_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --after-listing-id value: {value}"))?,
                );
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown repopulate-avionics argument: {arg}"),
        }
    }

    if limit < 1 {
        bail!("--limit must be at least 1");
    }
    if listing_id.is_some_and(|listing_id| listing_id < 1) {
        bail!("--listing-id must be a positive integer");
    }
    if after_listing_id.is_some_and(|listing_id| listing_id < 1) {
        bail!("--after-listing-id must be a positive integer");
    }
    if listing_id.is_some() && after_listing_id.is_some() {
        bail!("--listing-id and --after-listing-id are mutually exclusive");
    }

    Ok(AdminCommand::RepopulateAvionics {
        database: database_url_from_arg(database),
        mode: requested_mode.unwrap_or(AvionicsRepopulationCommandMode::Preflight),
        limit,
        listing_id,
        after_listing_id,
    })
}

fn set_repopulation_mode(
    requested: &mut Option<AvionicsRepopulationCommandMode>,
    mode: AvionicsRepopulationCommandMode,
    flag: &str,
) -> Result<()> {
    if let Some(previous) = requested {
        if *previous != mode {
            bail!(
                "{flag} conflicts with the previously selected repopulate-avionics execution mode"
            );
        }
    }
    *requested = Some(mode);
    Ok(())
}

fn parse_stage_listing_reviews_args(
    args: impl IntoIterator<Item = String>,
) -> Result<AdminCommand> {
    let mut database = None;
    let mut apply = false;
    let mut limit = default_stage_limit();
    let mut listing_id = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--limit" => {
                let value = args.next().context("--limit requires a value")?;
                limit = value
                    .parse::<i64>()
                    .with_context(|| format!("invalid --limit value: {value}"))?;
            }
            "--listing-id" => {
                let value = args.next().context("--listing-id requires a value")?;
                listing_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --listing-id value: {value}"))?,
                );
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown stage-listing-reviews argument: {arg}"),
        }
    }
    if limit < 1 {
        bail!("--limit must be at least 1");
    }
    if listing_id.is_some_and(|listing_id| listing_id < 1) {
        bail!("--listing-id must be a positive integer");
    }

    Ok(AdminCommand::StageListingReviews {
        database: database_url_from_arg(database),
        apply,
        limit,
        listing_id,
    })
}

fn parse_audit_avionics_duplicates_args(
    args: impl IntoIterator<Item = String>,
) -> Result<AdminCommand> {
    let mut database = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown audit-avionics-duplicates argument: {arg}"),
        }
    }
    Ok(AdminCommand::AuditAvionicsDuplicates {
        database: database_url_from_arg(database),
    })
}

fn parse_consolidate_legacy_avionics_args(
    args: impl IntoIterator<Item = String>,
) -> Result<AdminCommand> {
    let mut database = None;
    let mut apply = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown consolidate-legacy-avionics argument: {arg}"),
        }
    }
    Ok(AdminCommand::ConsolidateLegacyAvionics {
        database: database_url_from_arg(database),
        apply,
    })
}

fn parse_fit_depreciation_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut apply = false;
    let mut min_model_samples = 4_usize;
    let mut value_reference_year = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--min-model-samples" => {
                let value = args
                    .next()
                    .context("--min-model-samples requires a value")?;
                min_model_samples = value
                    .parse::<usize>()
                    .with_context(|| format!("invalid --min-model-samples value: {value}"))?;
            }
            "--value-reference-year" => {
                let value = args
                    .next()
                    .context("--value-reference-year requires a value")?;
                value_reference_year =
                    Some(value.parse::<i64>().with_context(|| {
                        format!("invalid --value-reference-year value: {value}")
                    })?);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown fit-depreciation argument: {arg}"),
        }
    }

    Ok(AdminCommand::FitDepreciation {
        database: database_url_from_arg(database),
        apply,
        min_model_samples,
        value_reference_year,
    })
}

fn parse_snapshot_valuations_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut apply = false;
    let mut max_age_days = 180_i64;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--max-age-days" => {
                let value = args.next().context("--max-age-days requires a value")?;
                max_age_days = value
                    .parse::<i64>()
                    .with_context(|| format!("invalid --max-age-days value: {value}"))?;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown snapshot-valuations argument: {arg}"),
        }
    }
    Ok(AdminCommand::SnapshotValuations {
        database: database_url_from_arg(database),
        apply,
        max_age_days,
    })
}

fn parse_fit_valuation_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut apply = false;
    let mut snapshot_id = None;
    let mut kind = None;
    let mut maximum_epochs = 500_usize;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--snapshot-id" => {
                let value = args.next().context("--snapshot-id requires a value")?;
                snapshot_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --snapshot-id value: {value}"))?,
                );
            }
            "--kind" => kind = Some(args.next().context("--kind requires a value")?),
            "--maximum-epochs" => {
                let value = args.next().context("--maximum-epochs requires a value")?;
                maximum_epochs = value
                    .parse::<usize>()
                    .with_context(|| format!("invalid --maximum-epochs value: {value}"))?;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown fit-valuation argument: {arg}"),
        }
    }
    Ok(AdminCommand::FitValuation {
        database: database_url_from_arg(database),
        kind: kind.unwrap_or_else(|| "structural".to_string()),
        snapshot_id: snapshot_id.context("--snapshot-id is required")?,
        apply,
        maximum_epochs,
    })
}

fn parse_model_version_args(
    args: impl IntoIterator<Item = String>,
    activate: bool,
) -> Result<AdminCommand> {
    let mut database = None;
    let mut model_version_id = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--model-version-id" => {
                let value = args.next().context("--model-version-id requires a value")?;
                model_version_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --model-version-id value: {value}"))?,
                );
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown valuation model argument: {arg}"),
        }
    }
    let database = database_url_from_arg(database);
    let model_version_id = model_version_id.context("--model-version-id is required")?;
    if activate {
        Ok(AdminCommand::ActivateValuation {
            database,
            model_version_id,
        })
    } else {
        Ok(AdminCommand::ValidateValuation {
            database,
            model_version_id,
        })
    }
}

fn parse_enrich_aircraft_specs_args(
    args: impl IntoIterator<Item = String>,
) -> Result<AdminCommand> {
    let mut database = None;
    let mut apply = false;
    let mut limit = 10_i64;
    let mut value_reference_year = None;
    let mut refresh_existing = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--refresh-existing" => refresh_existing = true,
            "--limit" => {
                let value = args.next().context("--limit requires a value")?;
                limit = value
                    .parse::<i64>()
                    .with_context(|| format!("invalid --limit value: {value}"))?;
            }
            "--value-reference-year" => {
                let value = args
                    .next()
                    .context("--value-reference-year requires a value")?;
                value_reference_year =
                    Some(value.parse::<i64>().with_context(|| {
                        format!("invalid --value-reference-year value: {value}")
                    })?);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown enrich-aircraft-specs argument: {arg}"),
        }
    }

    Ok(AdminCommand::EnrichAircraftSpecs {
        database: database_url_from_arg(database),
        apply,
        limit,
        value_reference_year,
        refresh_existing,
    })
}

fn parse_cleanup_orphans_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown cleanup-orphans argument: {arg}"),
        }
    }

    Ok(AdminCommand::CleanupOrphans {
        database: database_url_from_arg(database),
    })
}

fn parse_curate_avionics_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut apply = false;
    let mut limit = i64::MAX;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--limit" => {
                let value = args.next().context("--limit requires a value")?;
                limit = value
                    .parse::<i64>()
                    .with_context(|| format!("invalid --limit value: {value}"))?;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown curate-avionics argument: {arg}"),
        }
    }

    Ok(AdminCommand::CurateAvionics {
        database: database_url_from_arg(database),
        apply,
        limit,
    })
}

fn parse_enrich_model_year_avionics_args(
    args: impl IntoIterator<Item = String>,
) -> Result<AdminCommand> {
    let mut database = None;
    let mut apply = false;
    let mut limit = 10_i64;
    let mut value_reference_year = None;
    let mut refresh_existing = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--refresh-existing" => refresh_existing = true,
            "--limit" => {
                let value = args.next().context("--limit requires a value")?;
                limit = value
                    .parse::<i64>()
                    .with_context(|| format!("invalid --limit value: {value}"))?;
            }
            "--value-reference-year" => {
                let value = args
                    .next()
                    .context("--value-reference-year requires a value")?;
                value_reference_year =
                    Some(value.parse::<i64>().with_context(|| {
                        format!("invalid --value-reference-year value: {value}")
                    })?);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown enrich-model-year-avionics argument: {arg}"),
        }
    }

    Ok(AdminCommand::EnrichModelYearAvionics {
        database: database_url_from_arg(database),
        apply,
        limit,
        value_reference_year,
        refresh_existing,
    })
}

fn parse_enrich_avionics_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut apply = false;
    let mut limit = 10_i64;
    let mut value_reference_year = None;
    let mut refresh_existing = false;
    let mut listing_id = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--refresh-existing" => refresh_existing = true,
            "--listing-id" => {
                let value = args.next().context("--listing-id requires a value")?;
                listing_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --listing-id value: {value}"))?,
                );
            }
            "--limit" => {
                let value = args.next().context("--limit requires a value")?;
                limit = value
                    .parse::<i64>()
                    .with_context(|| format!("invalid --limit value: {value}"))?;
            }
            "--value-reference-year" => {
                let value = args
                    .next()
                    .context("--value-reference-year requires a value")?;
                value_reference_year =
                    Some(value.parse::<i64>().with_context(|| {
                        format!("invalid --value-reference-year value: {value}")
                    })?);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown enrich-avionics argument: {arg}"),
        }
    }

    Ok(AdminCommand::EnrichAvionics {
        database: database_url_from_arg(database),
        apply,
        limit,
        value_reference_year,
        refresh_existing,
        listing_id,
    })
}

fn print_usage() {
    println!(
        "Usage:\n  aircost-admin import-faa-registry --master MASTER.txt --aircraft-reference ACFTREF.txt --engine-reference ENGINE.txt --snapshot-date YYYY-MM-DD --archive-sha256 HEX [--include-n-number N123AB]... [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Scans the official files and stores only target-scoped, non-PII FAA evidence. Explicit N-number targets are normalized, validated, and merged with listing and pending-submission targets; dry-run is the default.\n  aircost-admin curate-aircraft-hierarchy [--listing-limit 25] [--cluster-limit 5] [--listing-id LISTING_ID] [--faa-drs-pdf FILE --faa-drs-pdf-sha256 HEX --faa-drs-document-guid UUID --faa-drs-document-id ID --faa-drs-tcds-number NUMBER [--faa-drs-revision-number REV] [--faa-drs-revision-date DATE]] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Grounded Gemini hierarchy review is read-only by default. --apply atomically persists only independently verified, fully reviewable cases against their exact observation, FAA grounding, and catalog revision. Normal unknown-identity runs require FAA_DRS_API_KEY. The complete --faa-drs-* group is an explicit one-listing admin migration path for an already obtained current official PDF; it is digest-checked and never used by the web server.\n  aircost-admin benchmark-gemini [--task listing|metadata|avionics|visual]... [--model PINNED_MODEL]... [--listing-limit SAMPLE_SIZE] [--submission-id ID]... [--max-avionics-per-listing 1] [--max-visual-assets 8] [--seed TEXT] [--config FILE] [--execute] [--database {DEFAULT_DATABASE_PATH}]\n    Without --execute, exports a deterministic real-data suite using benchmark selection defaults from Gemini config. With --execute, makes paid calls and writes only gemini_api_usage accounting rows.\n  aircost-admin repopulate-avionics [--limit 10] [--listing-id LISTING_ID | --after-listing-id LISTING_ID] [--preflight | --dry-run | --apply] [--database {DEFAULT_DATABASE_PATH}]\n    Zero-Gemini preflight is the default and reports a resumable checkpoint plus logical provider-request baseline/envelope. --dry-run explicitly enables paid preview requests; --apply enables paid requests and per-listing atomic writes.\n  aircost-admin cleanup-orphans [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin curate-avionics [--limit ROWS] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin enrich-avionics [--limit 10] [--listing-id LISTING_ID] [--value-reference-year 2026] [--refresh-existing] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin enrich-model-year-avionics [--limit 10] [--value-reference-year 2026] [--refresh-existing] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin enrich-aircraft-specs [--limit 10] [--value-reference-year 2026] [--refresh-existing] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin snapshot-valuations [--max-age-days 180] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin fit-valuation --kind structural|dnn --snapshot-id ID [--maximum-epochs 500] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin validate-valuation --model-version-id ID [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin activate-valuation --model-version-id ID [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin fit-depreciation [legacy] [--min-model-samples 4] [--value-reference-year 2026] [--apply] [--database {DEFAULT_DATABASE_PATH}]"
    );
    println!(
        "  aircost-admin stage-listing-reviews [--limit 100] [--listing-id LISTING_ID] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Prepares pending reviews from retained extraction data without Gemini, catalog writes, or listing-link writes; dry-run is the default."
    );
    println!(
        "  aircost-admin audit-avionics-duplicates [--database {DEFAULT_DATABASE_PATH}]\n    Reports model collisions by stored keys, current canonical maker/product keys, and exact maker-scoped stable-identifier kind/value pairs without writing.\n  aircost-admin consolidate-legacy-avionics [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Previews or applies explicitly verified unreviewed product duplicates. Automatic product consolidation requires every pair in a component to share the same non-null manufacturer identifier kind and normalized value inside one evidence-authorized manufacturer identity scope. Raw manufacturer spellings remain immutable source history; manufacturer aliases are resolved only through identity membership or redirect. Dry-run is the default."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operator_tcds_args() -> Vec<String> {
        [
            "curate-aircraft-hierarchy",
            "--listing-id",
            "23",
            "--faa-drs-pdf",
            "/tmp/aircost-drs-3a13.pdf",
            "--faa-drs-pdf-sha256",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--faa-drs-document-guid",
            "cbe9c99d-492f-4d25-9d37-925d57816f27",
            "--faa-drs-document-id",
            "DRSDOCID109699679420240809163108.0001",
            "--faa-drs-tcds-number",
            "3A13",
            "--faa-drs-revision-number",
            "75",
            "--faa-drs-revision-date",
            "08/07/2024",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn faa_import_args() -> Vec<String> {
        [
            "import-faa-registry",
            "--database",
            "sqlite::memory:",
            "--master",
            "/tmp/MASTER.txt",
            "--aircraft-reference",
            "/tmp/ACFTREF.txt",
            "--engine-reference",
            "/tmp/ENGINE.txt",
            "--snapshot-date",
            "2026-07-20",
            "--archive-sha256",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn import_faa_registry_cli_is_dry_run_by_default() {
        let command = parse_args(faa_import_args()).unwrap();
        let AdminCommand::ImportFaaRegistry {
            database,
            master,
            aircraft_reference,
            engine_reference,
            snapshot_date,
            archive_sha256,
            explicit_targets,
            apply,
        } = command
        else {
            panic!("expected import-faa-registry command")
        };
        assert_eq!(database, "sqlite::memory:");
        assert_eq!(master, PathBuf::from("/tmp/MASTER.txt"));
        assert_eq!(aircraft_reference, PathBuf::from("/tmp/ACFTREF.txt"));
        assert_eq!(engine_reference, PathBuf::from("/tmp/ENGINE.txt"));
        assert_eq!(snapshot_date, "2026-07-20");
        assert_eq!(archive_sha256, "a".repeat(64));
        assert_eq!(explicit_targets, ExplicitNNumberTargets::default());
        assert!(!apply);
    }

    #[test]
    fn import_faa_registry_cli_accepts_repeatable_normalized_explicit_targets() {
        let mut args = faa_import_args();
        args.extend(
            [
                "--include-n-number",
                "n-1925 x",
                "--include-n-number",
                "N1925X",
                "--include-n-number",
                "N123AB",
            ]
            .into_iter()
            .map(str::to_string),
        );

        let AdminCommand::ImportFaaRegistry {
            explicit_targets, ..
        } = parse_args(args).unwrap()
        else {
            panic!("expected import-faa-registry command")
        };
        assert_eq!(explicit_targets.requested, ["n-1925 x", "N1925X", "N123AB"]);
        assert_eq!(explicit_targets.accepted, ["N123AB", "N1925X"]);
    }

    #[test]
    fn import_faa_registry_cli_rejects_invalid_explicit_targets() {
        let mut args = faa_import_args();
        args.extend(
            ["--include-n-number", "C-GABC"]
                .into_iter()
                .map(str::to_string),
        );

        let error = parse_args(args).unwrap_err();
        assert!(error.to_string().contains("invalid --include-n-number"));
    }

    #[test]
    fn import_faa_registry_cli_requires_explicit_apply_and_valid_provenance() {
        let mut args = faa_import_args();
        args.push("--apply".to_string());
        assert!(matches!(
            parse_args(args).unwrap(),
            AdminCommand::ImportFaaRegistry { apply: true, .. }
        ));

        let invalid_hash = faa_import_args()
            .into_iter()
            .map(|value| {
                if value == "a".repeat(64) {
                    "not-a-digest".to_string()
                } else {
                    value
                }
            })
            .collect::<Vec<_>>();
        assert!(parse_args(invalid_hash)
            .unwrap_err()
            .to_string()
            .contains("64-character hexadecimal"));

        let missing_master = faa_import_args()
            .into_iter()
            .filter(|value| value != "--master" && value != "/tmp/MASTER.txt")
            .collect::<Vec<_>>();
        assert!(parse_args(missing_master)
            .unwrap_err()
            .to_string()
            .contains("--master is required"));
    }

    #[test]
    fn benchmark_cli_preserves_omitted_config_backed_overrides() {
        let command = parse_args(
            ["benchmark-gemini", "--config", "/tmp/gemini.toml"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();

        let AdminCommand::BenchmarkGemini {
            config,
            listing_limit,
            seed,
            ..
        } = command
        else {
            panic!("expected benchmark-gemini command")
        };
        assert_eq!(config, Some(PathBuf::from("/tmp/gemini.toml")));
        assert_eq!(listing_limit, None);
        assert_eq!(seed, None);
    }

    #[test]
    fn benchmark_cli_parses_explicit_selection_overrides() {
        let command = parse_args(
            [
                "benchmark-gemini",
                "--listing-limit",
                "7",
                "--seed",
                "cli-seed",
                "--task",
                "metadata",
                "--submission-id",
                "44",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();

        let AdminCommand::BenchmarkGemini {
            listing_limit,
            seed,
            tasks,
            submission_ids,
            ..
        } = command
        else {
            panic!("expected benchmark-gemini command")
        };
        assert_eq!(listing_limit, Some(7));
        assert_eq!(seed.as_deref(), Some("cli-seed"));
        assert_eq!(tasks, [BenchmarkTaskKind::GroundedMetadata]);
        assert_eq!(submission_ids, [44]);
    }

    #[test]
    fn grounded_metadata_models_come_from_the_grounded_metadata_matrix() {
        let config = GeminiRuntimeConfig::from_toml_str(
            r#"
version = 1

[[benchmark.matrices]]
task = "grounded_metadata"
models = ["gemini-3.1-flash-lite", "gemini-3.5-flash"]
"#,
        )
        .unwrap();

        assert_eq!(
            benchmark_models_for_task(&config, BenchmarkTaskKind::GroundedMetadata).unwrap(),
            ["gemini-3.1-flash-lite", "gemini-3.5-flash"]
        );
    }

    #[test]
    fn benchmark_selection_uses_config_and_cli_precedence() {
        let mut config = GeminiRuntimeConfig::default();
        config.benchmark.sample_size = 0;
        config.benchmark.seed = 91;
        config.benchmark.listing_ids = vec![301, 205];

        let configured = resolve_benchmark_selection(&config, None, None, 1, 8, Vec::new())
            .expect("configured explicit listing IDs should bypass sampling");
        assert_eq!(configured.listing_limit, 0);
        assert_eq!(configured.seed, "91");
        assert_eq!(configured.listing_ids, [301, 205]);
        assert!(configured.submission_ids.is_empty());

        let overridden = resolve_benchmark_selection(
            &config,
            Some(3),
            Some("cli-seed".to_string()),
            1,
            8,
            vec![44],
        )
        .expect("CLI overrides should resolve");
        assert_eq!(overridden.listing_limit, 3);
        assert_eq!(overridden.seed, "cli-seed");
        assert!(overridden.listing_ids.is_empty());
        assert_eq!(overridden.submission_ids, [44]);
    }

    #[test]
    fn curate_aircraft_hierarchy_cli_is_dry_run_with_bounded_defaults() {
        let command = parse_args(
            ["curate-aircraft-hierarchy", "--database", "sqlite::memory:"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();

        let AdminCommand::CurateAircraftHierarchy {
            database,
            listing_limit,
            cluster_limit,
            listing_id,
            operator_tcds,
            apply,
        } = command
        else {
            panic!("expected curate-aircraft-hierarchy command")
        };
        assert_eq!(database, "sqlite::memory:");
        assert_eq!(listing_limit, 25);
        assert_eq!(cluster_limit, 5);
        assert_eq!(listing_id, None);
        assert_eq!(operator_tcds, None);
        assert!(!apply);
    }

    #[test]
    fn curate_aircraft_hierarchy_cli_parses_scope_overrides() {
        let command = parse_args(
            [
                "curate-aircraft-hierarchy",
                "--database-url",
                "postgres://aircost.test/db",
                "--listing-limit",
                "80",
                "--cluster-limit",
                "12",
                "--listing-id",
                "29",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();

        let AdminCommand::CurateAircraftHierarchy {
            database,
            listing_limit,
            cluster_limit,
            listing_id,
            operator_tcds,
            apply,
        } = command
        else {
            panic!("expected curate-aircraft-hierarchy command")
        };
        assert_eq!(database, "postgres://aircost.test/db");
        assert_eq!(listing_limit, 80);
        assert_eq!(cluster_limit, 12);
        assert_eq!(listing_id, Some(29));
        assert_eq!(operator_tcds, None);
        assert!(!apply);
    }

    #[test]
    fn curate_aircraft_hierarchy_cli_accepts_explicit_apply() {
        let command = parse_args(
            ["curate-aircraft-hierarchy", "--apply"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();

        let AdminCommand::CurateAircraftHierarchy { apply, .. } = command else {
            panic!("expected curate-aircraft-hierarchy command")
        };
        assert!(apply);
    }

    #[test]
    fn curate_aircraft_hierarchy_cli_accepts_complete_operator_tcds_group() {
        let command = parse_args(operator_tcds_args()).unwrap();

        let AdminCommand::CurateAircraftHierarchy {
            listing_id,
            operator_tcds,
            apply,
            ..
        } = command
        else {
            panic!("expected curate-aircraft-hierarchy command")
        };
        assert_eq!(listing_id, Some(23));
        assert_eq!(
            operator_tcds,
            Some(OperatorTcdsInput {
                pdf: PathBuf::from("/tmp/aircost-drs-3a13.pdf"),
                pdf_sha256: "a".repeat(64),
                document_guid: "cbe9c99d-492f-4d25-9d37-925d57816f27".to_string(),
                document_id: "DRSDOCID109699679420240809163108.0001".to_string(),
                tcds_number: "3A13".to_string(),
                revision_number: Some("75".to_string()),
                revision_date: Some("08/07/2024".to_string()),
            })
        );
        assert!(!apply);
    }

    #[test]
    fn curate_aircraft_hierarchy_cli_rejects_incomplete_operator_tcds_group() {
        for missing_argument in [
            "--faa-drs-pdf",
            "--faa-drs-pdf-sha256",
            "--faa-drs-document-guid",
            "--faa-drs-document-id",
            "--faa-drs-tcds-number",
        ] {
            let mut args = operator_tcds_args();
            let position = args
                .iter()
                .position(|argument| argument == missing_argument)
                .expect("fixture must contain every required operator TCDS argument");
            args.remove(position);
            args.remove(position);

            let error = parse_args(args)
                .err()
                .expect("an incomplete operator TCDS group must be rejected");
            assert!(
                error.to_string().contains(
                    "operator TCDS mode requires --faa-drs-pdf, --faa-drs-pdf-sha256, \
                     --faa-drs-document-guid, --faa-drs-document-id, and \
                     --faa-drs-tcds-number together"
                ),
                "unexpected error when {missing_argument} was omitted: {error:#}"
            );
        }
    }

    #[test]
    fn curate_aircraft_hierarchy_cli_requires_listing_id_for_operator_tcds() {
        let mut args = operator_tcds_args();
        let position = args
            .iter()
            .position(|argument| argument == "--listing-id")
            .expect("fixture must contain --listing-id");
        args.remove(position);
        args.remove(position);

        let error = parse_args(args)
            .err()
            .expect("operator TCDS mode without a listing ID must be rejected");
        assert_eq!(
            error.to_string(),
            "operator TCDS mode requires --listing-id"
        );
    }

    #[test]
    fn curate_aircraft_hierarchy_cli_rejects_invalid_bounds() {
        for (argument, value) in [
            ("--listing-limit", "0"),
            ("--cluster-limit", "0"),
            ("--listing-id", "0"),
        ] {
            let error = parse_args(
                ["curate-aircraft-hierarchy", argument, value]
                    .into_iter()
                    .map(str::to_string),
            )
            .err()
            .expect("non-positive scope must be rejected");
            assert!(error.to_string().contains("must be"));
        }
    }

    fn aircraft_apply_grounding() -> AircraftGrounding {
        use aircost_rs::aircraft::faa::{AircraftReference, SerialMatch, Snapshot};

        AircraftGrounding {
            snapshot: Snapshot {
                id: 2,
                evidence_source_id: 3,
                snapshot_date: "2026-07-23".to_string(),
                source_url: "https://www.faa.gov/registry".to_string(),
                archive_sha256: "a".repeat(64),
                source_manifest_sha256: "b".repeat(64),
                target_set_sha256: "c".repeat(64),
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

    fn aircraft_apply_observation() -> AircraftIdentityObservation {
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

    fn aircraft_apply_case() -> AircraftHierarchyCurationCaseReport {
        use aircost_rs::aircraft::curation::workflow::{
            FaaObservationAudit, FaaRegistryFunctionResult, FaaRegistryObservationGrounding,
        };
        use aircost_rs::aircraft::curation::AircraftCatalogSearchResponse;

        let grounding = aircraft_apply_grounding();
        let observation = aircraft_apply_observation();
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
                search_request: aircost_rs::aircraft::curation::AircraftCatalogSearchRequest {
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
    fn aircraft_apply_trace_rejects_validation_errors_before_planning() {
        let mut case = aircraft_apply_case();
        case.validation_errors
            .push("not independently verified".to_string());

        let error = require_reviewable_apply_trace(&case).unwrap_err();
        assert!(error.contains("validation errors"));
    }

    #[test]
    fn aircraft_apply_trace_rejects_catalog_revision_mismatch() {
        let mut case = aircraft_apply_case();
        case.catalog_function_results[0].catalog_revision = "stale-revision".to_string();

        let error = require_reviewable_apply_trace(&case).unwrap_err();
        assert!(error.contains("catalog"));
    }

    #[test]
    fn aircraft_apply_trace_rejects_ambiguous_faa_result() {
        let mut case = aircraft_apply_case();
        case.faa_function_result_count = 2;

        let error = require_reviewable_apply_trace(&case).unwrap_err();
        assert!(error.contains("FAA"));
    }

    #[test]
    fn aircraft_apply_planning_rejects_missing_and_duplicate_observation_hashes() {
        let case = aircraft_apply_case();
        let (_, groundings) = require_reviewable_apply_trace(&case).unwrap();
        let observation = aircraft_apply_observation();
        let mut stale_observation = observation.clone();
        stale_observation.observation_sha256 = "0".repeat(64);

        let missing = plan_case_observations(
            &case,
            &[stale_observation],
            &groundings,
            AircraftObservationApplyPolicy::ReviewableCatalogWrite,
        )
        .unwrap_err();
        assert!(missing.contains("missing"));

        let duplicate = plan_case_observations(
            &case,
            &[observation.clone(), observation],
            &groundings,
            AircraftObservationApplyPolicy::ReviewableCatalogWrite,
        )
        .unwrap_err();
        assert!(duplicate.contains("ambiguous"));
    }

    #[test]
    fn aircraft_apply_planning_rejects_faa_observation_mismatch_as_one_case() {
        let mut case = aircraft_apply_case();
        case.faa_function_results[0].observations[0].observation_sha256 = "0".repeat(64);
        let (_, groundings) = require_reviewable_apply_trace(&case).unwrap();

        let error = plan_case_observations(
            &case,
            &[aircraft_apply_observation()],
            &groundings,
            AircraftObservationApplyPolicy::ReviewableCatalogWrite,
        )
        .unwrap_err();
        assert!(error.contains("not bound"));
    }

    #[test]
    fn aircraft_catalog_writes_reject_non_exact_listing_source() {
        let case = aircraft_apply_case();
        let (_, groundings) = require_reviewable_apply_trace(&case).unwrap();
        let mut observation = aircraft_apply_observation();
        observation.source_excerpt = Some("fallback values assembled from fields".to_string());
        observation.source_excerpt_is_exact = false;

        let error = plan_case_observations(
            &case,
            &[observation],
            &groundings,
            AircraftObservationApplyPolicy::ReviewableCatalogWrite,
        )
        .unwrap_err();

        assert!(error.contains("exact listing source"));
    }

    #[test]
    fn approved_catalog_reuse_accepts_non_exact_listing_source_without_model_trace() {
        let mut case = aircraft_apply_case();
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
        assert!(case.interactions.is_empty());

        let groundings = approved_catalog_apply_groundings(&case).unwrap();
        let mut observation = aircraft_apply_observation();
        observation.source_excerpt = None;
        observation.source_excerpt_is_exact = false;
        let fresh_observations = [observation];

        let planned = plan_case_observations(
            &case,
            &fresh_observations,
            &groundings,
            AircraftObservationApplyPolicy::ApprovedCatalogReuse,
        )
        .expect("exact FAA-backed approved catalog reuse does not depend on listing prose");

        assert_eq!(planned.len(), 1);
        assert!(!planned[0].0.source_excerpt_is_exact);
    }

    #[test]
    fn non_exact_observation_cannot_claim_unapproved_or_unverified_catalog_reuse() {
        let mut case = aircraft_apply_case();
        let groundings = approved_catalog_apply_groundings(&case).unwrap();
        let mut observation = aircraft_apply_observation();
        observation.source_excerpt = Some("unverified fallback".to_string());
        observation.source_excerpt_is_exact = false;

        let missing_approval = plan_case_observations(
            &case,
            &[observation],
            &groundings,
            AircraftObservationApplyPolicy::ApprovedCatalogReuse,
        )
        .unwrap_err();
        assert!(missing_approval.contains("approved exact catalog identity"));

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
        case.faa_observations[0].eligibility = None;
        let unverified = approved_catalog_apply_groundings(&case).unwrap_err();
        assert!(unverified.contains("no exact eligible grounding"));

        case.faa_observations[0].faa_eligible = false;
        case.faa_observations[0].included_in_curation = false;
        let garbage = approved_catalog_apply_groundings(&case).unwrap_err();
        assert!(garbage.contains("no exact FAA-eligible observation grounding"));
    }

    #[test]
    fn repopulate_avionics_cli_is_zero_call_preflight_by_default() {
        let command = parse_args(
            [
                "repopulate-avionics",
                "--database",
                "sqlite::memory:",
                "--listing-id",
                "29",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();

        let AdminCommand::RepopulateAvionics {
            database,
            mode,
            limit,
            listing_id,
            after_listing_id,
        } = command
        else {
            panic!("expected repopulate-avionics command")
        };
        assert_eq!(database, "sqlite::memory:");
        assert_eq!(mode, AvionicsRepopulationCommandMode::Preflight);
        assert_eq!(limit, 10);
        assert_eq!(listing_id, Some(29));
        assert_eq!(after_listing_id, None);
    }

    #[test]
    fn repopulate_avionics_cli_parses_apply_limit_and_cursor() {
        let command = parse_args(
            [
                "repopulate-avionics",
                "--apply",
                "--limit",
                "7",
                "--after-listing-id",
                "29",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();

        let AdminCommand::RepopulateAvionics {
            mode,
            limit,
            listing_id,
            after_listing_id,
            ..
        } = command
        else {
            panic!("expected repopulate-avionics command")
        };
        assert_eq!(mode, AvionicsRepopulationCommandMode::Apply);
        assert_eq!(limit, 7);
        assert_eq!(listing_id, None);
        assert_eq!(after_listing_id, Some(29));
    }

    #[test]
    fn repopulate_avionics_cli_requires_explicit_paid_preview() {
        let command = parse_args(
            ["repopulate-avionics", "--dry-run"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            command,
            AdminCommand::RepopulateAvionics {
                mode: AvionicsRepopulationCommandMode::Preview,
                ..
            }
        ));
    }

    #[test]
    fn repopulate_avionics_cli_rejects_conflicting_mode_and_scope_flags() {
        for arguments in [
            vec!["repopulate-avionics", "--preflight", "--apply"],
            vec![
                "repopulate-avionics",
                "--listing-id",
                "29",
                "--after-listing-id",
                "28",
            ],
        ] {
            let error = parse_args(arguments.into_iter().map(str::to_string))
                .expect_err("conflicting options must be rejected");
            assert!(
                error.to_string().contains("conflict")
                    || error.to_string().contains("mutually exclusive")
            );
        }
    }

    #[test]
    fn stage_listing_reviews_cli_is_dry_run_by_default() {
        let command = parse_args(
            [
                "stage-listing-reviews",
                "--database",
                "sqlite::memory:",
                "--listing-id",
                "29",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        let AdminCommand::StageListingReviews {
            database,
            apply,
            limit,
            listing_id,
        } = command
        else {
            panic!("expected stage-listing-reviews command")
        };
        assert_eq!(database, "sqlite::memory:");
        assert!(!apply);
        assert_eq!(limit, 100);
        assert_eq!(listing_id, Some(29));
    }

    #[test]
    fn stage_listing_reviews_cli_requires_explicit_apply() {
        let command = parse_args(
            ["stage-listing-reviews", "--apply", "--limit", "70"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        let AdminCommand::StageListingReviews {
            apply,
            limit,
            listing_id,
            ..
        } = command
        else {
            panic!("expected stage-listing-reviews command")
        };
        assert!(apply);
        assert_eq!(limit, 70);
        assert_eq!(listing_id, None);
    }

    #[test]
    fn stage_listing_reviews_cli_rejects_invalid_scope() {
        for (argument, value) in [("--limit", "0"), ("--listing-id", "0")] {
            let error = parse_args(
                ["stage-listing-reviews", argument, value]
                    .into_iter()
                    .map(str::to_string),
            )
            .expect_err("non-positive scope must be rejected");
            assert!(error.to_string().contains("must be"));
        }
    }

    #[test]
    fn duplicate_audit_cli_is_read_only() {
        let command = parse_args(
            ["audit-avionics-duplicates", "--database", "sqlite::memory:"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            command,
            AdminCommand::AuditAvionicsDuplicates { database }
                if database == "sqlite::memory:"
        ));
    }

    #[test]
    fn legacy_avionics_consolidation_requires_explicit_apply() {
        let dry_run = parse_args(
            ["consolidate-legacy-avionics"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            dry_run,
            AdminCommand::ConsolidateLegacyAvionics { apply: false, .. }
        ));
        let apply = parse_args(
            ["consolidate-legacy-avionics", "--apply"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            apply,
            AdminCommand::ConsolidateLegacyAvionics { apply: true, .. }
        ));
    }
}
