use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::fs::File;
use std::path::PathBuf;

use aircost_rs::aircraft::curation::application::{
    apply_aircraft_hierarchy_curation_report, AircraftHierarchyApplicationReport,
};
use aircost_rs::aircraft::curation::workflow::{
    curate_aircraft_hierarchy_observations_with_config,
    curate_aircraft_hierarchy_observations_with_operator_tcds,
};
use aircost_rs::aircraft::faa::{
    drs::{parse_operator_supplied_current_tcds, CurrentTcdsMetadata, DrsClient, TcdsDocument},
    listing_targets, parse_release_archive, require_listing_faa_admission, store_release,
    ExplicitNNumberTargets, FaaImportTargets,
};
use aircost_rs::aircraft::reference::persistence::{
    assemble_and_publish_reference_version, preview_reference_version,
    ApprovedReferenceVersionDraft,
};
use aircost_rs::aircraft::verification::AircraftVerificationServices;
use aircost_rs::avionics::consolidation::{
    audit_avionics_catalog_duplicates, consolidate_avionics_models,
    plan_canonical_legacy_duplicates, preview_avionics_model_consolidation,
};
use aircost_rs::avionics::{
    curate_avionics_models_with_gemini, enrich_listing_avionics_metadata,
    enrich_missing_avionics_metadata,
};
use aircost_rs::cleanup::cleanup_orphan_records;
use aircost_rs::db::{database_url_from_arg, DEFAULT_DATABASE_PATH};
use aircost_rs::extract::GeminiListingExtractor;
use aircost_rs::fit::fit_structural_valuation;
use aircost_rs::gemini::benchmark::{
    execute as execute_gemini_benchmark, load_suite as load_gemini_benchmark_suite,
    BenchmarkPricing, BenchmarkSelection, BenchmarkTaskKind,
};
use aircost_rs::gemini::config::{GeminiRuntimeConfig, GeminiTask};
use aircost_rs::gemini::interactions::GeminiInteractionsClient;
use aircost_rs::gemini::live_benchmark::LiveBenchmarkRunner;
use aircost_rs::gemini::usage::Store as GeminiUsageStore;
use aircost_rs::listing::backfill::{default_stage_limit, stage_legacy_listing_reviews};
use aircost_rs::listing::replay::run::{replay_captures, ReplayCapturesRequest, ReplayPhase};
use aircost_rs::listing::replay::{
    build_trusted_capture_manifest, import_trusted_capture_manifest,
    reconcile_replay_occurrence_dispositions, trusted_bound_capture_ids, TrustedCaptureManifest,
};
use aircost_rs::listing::verification::{
    verify_listings, ListingVerificationMode, ListingVerificationScope, ListingVerificationServices,
};
use aircost_rs::plugin::{
    checkpoint_plugin_submission_extraction, inspect_plugin_submission_extraction,
    materialize_plugin_submission_checkpoint, plugin_submission_owner,
    preflight_plugin_submission_extraction,
};
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
        AdminCommand::PublishAircraftReference {
            database,
            draft,
            apply,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let input = File::open(&draft)
                .with_context(|| format!("could not open reference draft {}", draft.display()))?;
            let normalized: ApprovedReferenceVersionDraft = serde_json::from_reader(input)
                .with_context(|| {
                    format!(
                        "reference draft {} is not valid normalized reference JSON",
                        draft.display()
                    )
                })?;
            let ids = if apply {
                assemble_and_publish_reference_version(&db, &normalized).await?
            } else {
                preview_reference_version(&db, &normalized).await?
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "dry_run": !apply,
                    "validated_normalized_draft": true,
                    "database_publication_gates_passed": true,
                    "configuration_id": ids.configuration_id,
                    "version_id": ids.version_id,
                    "persisted": apply,
                    "stored_provider_dossiers": 0,
                }))?
            );
        }
        AdminCommand::ExportReplayManifest {
            database,
            output,
            submission_ids,
            all_bound,
            apply,
        } => {
            let db = aircost_rs::db::AppDb::connect_diagnostic(&database).await?;
            let selected = if all_bound {
                trusted_bound_capture_ids(&db)
                    .await
                    .map_err(anyhow::Error::msg)?
            } else {
                submission_ids
            };
            let manifest = build_trusted_capture_manifest(&db, &selected)
                .await
                .map_err(anyhow::Error::msg)?;
            if apply {
                let bytes = serde_json::to_vec_pretty(&manifest)?;
                fs::write(&output, bytes).with_context(|| {
                    format!("could not write replay manifest {}", output.display())
                })?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "dry_run": !apply,
                    "output": output,
                    "capture_count": manifest.captures.len(),
                    "manifest_sha256": manifest.manifest_sha256,
                    "submission_ids": manifest.captures.iter().map(|row| row.submission_id).collect::<Vec<_>>(),
                }))?
            );
        }
        AdminCommand::ImportReplayManifest {
            source_database,
            database,
            manifest,
            apply,
        } => {
            if aircost_rs::db::sqlite_database_urls_equal(&source_database, &database)? {
                bail!("source and replay target databases must be different");
            }
            let manifest: TrustedCaptureManifest =
                serde_json::from_slice(&fs::read(&manifest).with_context(|| {
                    format!("could not read replay manifest {}", manifest.display())
                })?)?;
            let source = aircost_rs::db::AppDb::connect_diagnostic(&source_database).await?;
            let target = if apply {
                aircost_rs::db::AppDb::connect(&database).await?
            } else {
                aircost_rs::db::AppDb::connect_diagnostic(&database).await?
            };
            let report = import_trusted_capture_manifest(&source, &target, &manifest, apply)
                .await
                .map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::ReplayCaptures {
            database,
            manifest,
            phase,
            submission_id,
            apply,
            recover_stale,
        } => {
            let manifest: TrustedCaptureManifest =
                serde_json::from_slice(&fs::read(&manifest).with_context(|| {
                    format!("could not read replay manifest {}", manifest.display())
                })?)?;
            let db = if apply {
                aircost_rs::db::AppDb::connect(&database).await?
            } else {
                aircost_rs::db::AppDb::connect_diagnostic(&database).await?
            };
            let extractor = apply
                .then(|| GeminiListingExtractor::from_environment_with_usage(&db))
                .transpose()?;
            let report = replay_captures(
                &db,
                extractor.as_ref(),
                &ReplayCapturesRequest {
                    manifest: &manifest,
                    phase,
                    submission_id,
                    apply,
                    recover_stale,
                },
            )
            .await
            .map_err(anyhow::Error::new)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::ReplayExtraction {
            database,
            submission_id,
            apply,
        } => {
            let db = if apply {
                aircost_rs::db::AppDb::connect(&database).await?
            } else {
                aircost_rs::db::AppDb::connect_diagnostic(&database).await?
            };
            let user = plugin_submission_owner(&db, submission_id).await?;
            if apply {
                let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
                let checkpoint =
                    checkpoint_plugin_submission_extraction(&db, &user, submission_id, &extractor)
                        .await?;
                println!("{}", serde_json::to_string_pretty(&checkpoint)?);
            } else {
                let preflight =
                    preflight_plugin_submission_extraction(&db, user.id, submission_id).await?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "dry_run": true,
                        "submission_id": submission_id,
                        "provider_calls": 0,
                        "capture_valid": preflight.capture_valid,
                        "current_checkpoint": preflight.current_checkpoint,
                        "next_action": "rerun with --apply to perform extraction only; no aircraft, avionics catalog, listing, or finalization writes will run"
                    }))?
                );
            }
        }
        AdminCommand::ReconcileReplayAvionics {
            database,
            listing_id,
            submission_id,
            apply,
        } => {
            let db = if apply {
                aircost_rs::db::AppDb::connect(&database).await?
            } else {
                aircost_rs::db::AppDb::connect_diagnostic(&database).await?
            };
            let owner = plugin_submission_owner(&db, submission_id).await?;
            let report = reconcile_replay_occurrence_dispositions(
                &db,
                listing_id,
                submission_id,
                owner.id,
                apply,
            )
            .await
            .map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::ReplayListing {
            database,
            submission_id,
            apply,
        } => {
            let db = if apply {
                aircost_rs::db::AppDb::connect(&database).await?
            } else {
                aircost_rs::db::AppDb::connect_diagnostic(&database).await?
            };
            let owner = plugin_submission_owner(&db, submission_id).await?;
            if apply {
                let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
                let checkpoint =
                    inspect_plugin_submission_extraction(&db, owner.id, submission_id).await?;
                let outcome = materialize_plugin_submission_checkpoint(
                    &db,
                    &owner,
                    submission_id,
                    &checkpoint.extracted_listing_sha256,
                    &extractor,
                )
                .await?;
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                let checkpoint =
                    inspect_plugin_submission_extraction(&db, owner.id, submission_id).await?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "dry_run": true,
                        "provider_calls": 0,
                        "checkpoint": checkpoint,
                        "next_action": "rerun with --apply to materialize this exact checkpoint through normal listing analysis without repeating extraction"
                    }))?
                );
            }
        }
        AdminCommand::ImportFaaRegistry {
            database,
            archive,
            explicit_targets,
            apply,
        } => {
            let report = import_faa_registry(database, archive, explicit_targets, apply).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
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
            let drs_client = if supplied_tcds.is_none() {
                let api_key = env::var("FAA_DRS_API_KEY")
                    .context("FAA_DRS_API_KEY is required for unknown aircraft identities")?;
                Some(DrsClient::new(api_key)?)
            } else {
                None
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
                        drs_client
                            .as_ref()
                            .expect("normal curation path constructs an FAA DRS client"),
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
                apply_aircraft_hierarchy_curation_report(&db, &report, listing_limit, listing_id)
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
        AdminCommand::VerifyListings {
            database,
            mode,
            limit,
            listing_id,
            after_listing_id,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let scope = ListingVerificationScope::new(limit, listing_id, after_listing_id);
            match mode {
                ListingVerificationCommandMode::Preflight => {
                    let report = verify_listings(
                        &db,
                        ListingVerificationMode::Preflight,
                        &scope,
                        ListingVerificationServices::unavailable(),
                    )
                    .await?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                ListingVerificationCommandMode::Preview | ListingVerificationCommandMode::Apply => {
                    let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
                    let runtime_config = GeminiRuntimeConfig::from_environment()?;
                    let gemini_api_key = env::var("GEMINI_API_KEY")
                        .context("GEMINI_API_KEY is required for automatic verification")?;
                    let aircraft_gemini = GeminiInteractionsClient::new(gemini_api_key)?
                        .with_usage_store(GeminiUsageStore::new(&db));
                    let aircraft_drs = env::var("FAA_DRS_API_KEY")
                        .ok()
                        .map(DrsClient::new)
                        .transpose()?;
                    let aircraft = aircraft_drs
                        .as_ref()
                        .map(|drs| AircraftVerificationServices {
                            gemini: &aircraft_gemini,
                            drs,
                            config: &runtime_config,
                        });
                    let verification_mode = match mode {
                        ListingVerificationCommandMode::Preview => ListingVerificationMode::Preview,
                        ListingVerificationCommandMode::Apply => ListingVerificationMode::Apply,
                        ListingVerificationCommandMode::Preflight => unreachable!(),
                    };
                    let report = verify_listings(
                        &db,
                        verification_mode,
                        &scope,
                        ListingVerificationServices {
                            extractor: Some(&extractor),
                            aircraft,
                        },
                    )
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

async fn import_faa_registry(
    database: String,
    archive: PathBuf,
    explicit_targets: ExplicitNNumberTargets,
    apply: bool,
) -> Result<serde_json::Value> {
    let db = if apply {
        aircost_rs::db::AppDb::connect(&database).await?
    } else {
        aircost_rs::db::AppDb::connect_diagnostic(&database).await?
    };
    let targets = FaaImportTargets::merge(listing_targets(&db).await?, explicit_targets);
    if targets.n_numbers.is_empty() {
        bail!(
            "the database and --include-n-number arguments have no valid N-number targets for an FAA import"
        );
    }
    let parse_targets = targets.n_numbers.clone();
    let release = tokio::task::spawn_blocking(move || -> Result<_> {
        let archive_file = File::open(&archive)
            .with_context(|| format!("could not open FAA release ZIP {}", archive.display()))?;
        parse_release_archive(archive_file, &parse_targets)
    })
    .await
    .context("FAA registry parser task failed")??;
    let release_summary = release.summary();
    let stored = if apply {
        Some(store_release(&db, &release).await?)
    } else {
        None
    };
    Ok(serde_json::json!({
        "dry_run": !apply,
        "listing_targets": targets.listing_targets,
        "explicit_targets": targets.explicit_targets,
        "snapshot_date": release_summary.snapshot_date,
        "source_url": release_summary.source_url,
        "archive_sha256": release_summary.archive_sha256,
        "source_manifest_sha256": release_summary.source_manifest_sha256,
        "target_set_sha256": release_summary.target_set_sha256,
        "record_hash_domain": release_summary.record_hash_domain,
        "member_sha256": release_summary.member_sha256,
        "target_count": release_summary.target_count,
        "matched_count": release_summary.matched_count,
        "absent_count": release_summary.absent_count,
        "aircraft_reference_count": release_summary.aircraft_reference_count,
        "engine_reference_count": release_summary.engine_reference_count,
        "stored": stored,
        "canonical_catalog_writes": 0,
    }))
}

#[derive(Debug)]
enum AdminCommand {
    PublishAircraftReference {
        database: String,
        draft: PathBuf,
        apply: bool,
    },
    ExportReplayManifest {
        database: String,
        output: PathBuf,
        submission_ids: Vec<i64>,
        all_bound: bool,
        apply: bool,
    },
    ImportReplayManifest {
        source_database: String,
        database: String,
        manifest: PathBuf,
        apply: bool,
    },
    ReplayCaptures {
        database: String,
        manifest: PathBuf,
        phase: ReplayPhase,
        submission_id: Option<i64>,
        apply: bool,
        recover_stale: bool,
    },
    ReplayExtraction {
        database: String,
        submission_id: i64,
        apply: bool,
    },
    ReconcileReplayAvionics {
        database: String,
        listing_id: i64,
        submission_id: i64,
        apply: bool,
    },
    ReplayListing {
        database: String,
        submission_id: i64,
        apply: bool,
    },
    ImportFaaRegistry {
        database: String,
        archive: PathBuf,
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
    VerifyListings {
        database: String,
        mode: ListingVerificationCommandMode,
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
enum ListingVerificationCommandMode {
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

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        print_usage();
        bail!("missing admin command");
    };

    match command.as_str() {
        "publish-aircraft-reference" => parse_publish_aircraft_reference_args(args),
        "export-replay-manifest" => parse_export_replay_manifest_args(args),
        "import-replay-manifest" => parse_import_replay_manifest_args(args),
        "replay-captures" => parse_replay_captures_args(args),
        "replay-extraction" => parse_replay_extraction_args(args),
        "reconcile-replay-avionics" => parse_reconcile_replay_avionics_args(args),
        "replay-listing" => parse_replay_listing_args(args),
        "import-faa-registry" => parse_import_faa_registry_args(args),
        "curate-aircraft-hierarchy" => parse_curate_aircraft_hierarchy_args(args),
        "benchmark-gemini" => parse_benchmark_gemini_args(args),
        "verify-listings" => parse_verify_listings_args(args),
        "stage-listing-reviews" => parse_stage_listing_reviews_args(args),
        "audit-avionics-duplicates" => parse_audit_avionics_duplicates_args(args),
        "consolidate-legacy-avionics" => parse_consolidate_legacy_avionics_args(args),
        "enrich-avionics" => parse_enrich_avionics_args(args),
        "cleanup-orphans" => parse_cleanup_orphans_args(args),
        "curate-avionics" => parse_curate_avionics_args(args),
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

fn parse_publish_aircraft_reference_args(
    args: impl IntoIterator<Item = String>,
) -> Result<AdminCommand> {
    let mut database = None;
    let mut draft = None;
    let mut apply = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--draft" => {
                draft = Some(PathBuf::from(
                    args.next().context("--draft requires a value")?,
                ));
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown publish-aircraft-reference argument: {arg}"),
        }
    }
    Ok(AdminCommand::PublishAircraftReference {
        database: database_url_from_arg(database),
        draft: draft.context("--draft is required")?,
        apply,
    })
}

fn parse_export_replay_manifest_args(
    args: impl IntoIterator<Item = String>,
) -> Result<AdminCommand> {
    let mut database = None;
    let mut output = None;
    let mut submission_ids = Vec::new();
    let mut all_bound = false;
    let mut apply = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--output" => {
                output = Some(PathBuf::from(
                    args.next().context("--output requires a value")?,
                ));
            }
            "--submission-id" => {
                let value = args.next().context("--submission-id requires a value")?;
                submission_ids.push(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --submission-id value: {value}"))?,
                );
            }
            "--all-bound" => all_bound = true,
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown export-replay-manifest argument: {arg}"),
        }
    }
    if all_bound == !submission_ids.is_empty() {
        bail!("choose exactly one of --all-bound or one or more --submission-id values");
    }
    Ok(AdminCommand::ExportReplayManifest {
        database: database_url_from_arg(database),
        output: output.context("--output is required")?,
        submission_ids,
        all_bound,
        apply,
    })
}

fn parse_import_replay_manifest_args(
    args: impl IntoIterator<Item = String>,
) -> Result<AdminCommand> {
    let mut source_database = None;
    let mut database = None;
    let mut manifest = None;
    let mut apply = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--source-database" | "--source-database-url" => {
                source_database = Some(args.next().context("--source-database requires a value")?);
            }
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--manifest" => {
                manifest = Some(PathBuf::from(
                    args.next().context("--manifest requires a value")?,
                ));
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown import-replay-manifest argument: {arg}"),
        }
    }
    Ok(AdminCommand::ImportReplayManifest {
        source_database: database_url_from_arg(Some(
            source_database.context("--source-database is required")?,
        )),
        database: database_url_from_arg(database),
        manifest: manifest.context("--manifest is required")?,
        apply,
    })
}

fn parse_replay_captures_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut manifest = None;
    let mut phase = None;
    let mut submission_id = None;
    let mut apply = false;
    let mut recover_stale = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--manifest" => {
                manifest = Some(PathBuf::from(
                    args.next().context("--manifest requires a value")?,
                ));
            }
            "--phase" => {
                let value = args.next().context("--phase requires a value")?;
                phase = Some(match value.as_str() {
                    "extraction" => ReplayPhase::Extraction,
                    "materialization" => ReplayPhase::Materialization,
                    _ => bail!("--phase must be extraction or materialization"),
                });
            }
            "--submission-id" => {
                let value = args.next().context("--submission-id requires a value")?;
                submission_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --submission-id value: {value}"))?,
                );
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--recover-stale" => recover_stale = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown replay-captures argument: {arg}"),
        }
    }
    if submission_id.is_some_and(|id| id <= 0) {
        bail!("--submission-id must be positive");
    }
    if recover_stale && !apply {
        bail!("--recover-stale requires --apply");
    }
    Ok(AdminCommand::ReplayCaptures {
        database: database_url_from_arg(database),
        manifest: manifest.context("--manifest is required")?,
        phase: phase.context("--phase is required")?,
        submission_id,
        apply,
        recover_stale,
    })
}

fn parse_replay_extraction_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut submission_id = None;
    let mut apply = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--submission-id" => {
                let value = args.next().context("--submission-id requires a value")?;
                submission_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --submission-id value: {value}"))?,
                );
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown replay-extraction argument: {arg}"),
        }
    }
    let submission_id = submission_id.context("--submission-id is required")?;
    if submission_id <= 0 {
        bail!("--submission-id must be positive");
    }
    Ok(AdminCommand::ReplayExtraction {
        database: database_url_from_arg(database),
        submission_id,
        apply,
    })
}

fn parse_reconcile_replay_avionics_args(
    args: impl IntoIterator<Item = String>,
) -> Result<AdminCommand> {
    let mut database = None;
    let mut listing_id = None;
    let mut submission_id = None;
    let mut apply = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--listing-id" => {
                let value = args.next().context("--listing-id requires a value")?;
                listing_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --listing-id value: {value}"))?,
                );
            }
            "--submission-id" => {
                let value = args.next().context("--submission-id requires a value")?;
                submission_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --submission-id value: {value}"))?,
                );
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown reconcile-replay-avionics argument: {arg}"),
        }
    }
    let listing_id = listing_id.context("--listing-id is required")?;
    let submission_id = submission_id.context("--submission-id is required")?;
    if listing_id <= 0 || submission_id <= 0 {
        bail!("--listing-id and --submission-id must be positive");
    }
    Ok(AdminCommand::ReconcileReplayAvionics {
        database: database_url_from_arg(database),
        listing_id,
        submission_id,
        apply,
    })
}

fn parse_replay_listing_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut submission_id = None;
    let mut apply = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--submission-id" => {
                let value = args.next().context("--submission-id requires a value")?;
                submission_id = Some(
                    value
                        .parse::<i64>()
                        .with_context(|| format!("invalid --submission-id value: {value}"))?,
                );
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown replay-listing argument: {arg}"),
        }
    }
    let submission_id = submission_id.context("--submission-id is required")?;
    if submission_id <= 0 {
        bail!("--submission-id must be positive");
    }
    Ok(AdminCommand::ReplayListing {
        database: database_url_from_arg(database),
        submission_id,
        apply,
    })
}

fn parse_import_faa_registry_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut archive = None;
    let mut include_n_numbers = Vec::new();
    let mut apply = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--archive" => {
                archive = Some(PathBuf::from(
                    args.next().context("--archive requires a value")?,
                ));
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

    let explicit_targets = ExplicitNNumberTargets::parse(include_n_numbers)?;

    Ok(AdminCommand::ImportFaaRegistry {
        database: database_url_from_arg(database),
        archive: archive.context("--archive is required")?,
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

fn parse_verify_listings_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
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
            "--preflight" => set_listing_verification_mode(
                &mut requested_mode,
                ListingVerificationCommandMode::Preflight,
                "--preflight",
            )?,
            "--preview" => set_listing_verification_mode(
                &mut requested_mode,
                ListingVerificationCommandMode::Preview,
                "--preview",
            )?,
            "--apply" => set_listing_verification_mode(
                &mut requested_mode,
                ListingVerificationCommandMode::Apply,
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
            _ => bail!("unknown verify-listings argument: {arg}"),
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

    Ok(AdminCommand::VerifyListings {
        database: database_url_from_arg(database),
        mode: requested_mode.unwrap_or(ListingVerificationCommandMode::Preflight),
        limit,
        listing_id,
        after_listing_id,
    })
}

fn set_listing_verification_mode(
    requested: &mut Option<ListingVerificationCommandMode>,
    mode: ListingVerificationCommandMode,
    flag: &str,
) -> Result<()> {
    if let Some(previous) = requested {
        if *previous != mode {
            bail!("{flag} conflicts with the previously selected verify-listings execution mode");
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
        "Usage:\n  aircost-admin publish-aircraft-reference --draft NORMALIZED.json [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin export-replay-manifest (--all-bound | --submission-id ID...) --output FILE [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Verifies exact capture bytes, install ownership, and P-256 signatures. Dry-run prints the selection; --apply writes the credential-free manifest.\n  aircost-admin import-replay-manifest --source-database SOURCE --manifest FILE [--apply] [--database TARGET]\n    Re-verifies the manifest against SOURCE and imports exactly those signed captures into an empty target, preserving IDs/timestamps while resetting every derived field. Dry-run is the default.\n  aircost-admin replay-captures --manifest FILE --phase extraction|materialization [--submission-id ID] [--apply] [--recover-stale] [--database TARGET]\n    Resumes the manifest-backed batch ledger. Dry-run is provider-free; stale ownership requires explicit recovery after its conservative heartbeat threshold.\n  aircost-admin replay-extraction --submission-id ID [--apply] [--database TARGET]\n    Dry-run is provider-free. --apply performs only current-schema extraction and stops before aircraft, avionics identity, listing insertion, or finalization.\n  aircost-admin replay-listing --submission-id ID [--apply] [--database TARGET]\n    Dry-run revalidates the signed checkpoint without provider calls. --apply uses create-only normal admission; the listing insert and exact signed-capture bind share one transaction, and receipt-gated retries resume the bound row deterministically.\n  aircost-admin import-faa-registry --archive ReleasableAircraft.zip [--include-n-number N123AB]... [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Hashes and validates the official ZIP, derives its date from the required FAA members, then stores only target-scoped, non-PII FAA evidence. Explicit N-number targets are normalized, validated, and merged with listing and pending-submission targets; dry-run is the default.\n  aircost-admin curate-aircraft-hierarchy [--listing-limit 25] [--cluster-limit 5] [--listing-id LISTING_ID] [--faa-drs-pdf FILE --faa-drs-pdf-sha256 HEX --faa-drs-document-guid UUID --faa-drs-document-id ID --faa-drs-tcds-number NUMBER [--faa-drs-revision-number REV] [--faa-drs-revision-date DATE]] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Grounded Gemini hierarchy review is read-only by default. --apply atomically persists only independently verified, fully reviewable cases against their exact observation, FAA grounding, and catalog revision. Normal unknown-identity runs require FAA_DRS_API_KEY. The complete --faa-drs-* group is an explicit one-listing admin migration path for an already obtained current official PDF; it is digest-checked and never used by the web server.\n  aircost-admin benchmark-gemini [--task listing|metadata|avionics|visual]... [--model PINNED_MODEL]... [--listing-limit SAMPLE_SIZE] [--submission-id ID]... [--max-avionics-per-listing 1] [--max-visual-assets 8] [--seed TEXT] [--config FILE] [--execute] [--database {DEFAULT_DATABASE_PATH}]\n    Without --execute, exports a deterministic real-data suite using benchmark selection defaults from Gemini config. With --execute, makes paid calls and writes only gemini_api_usage accounting rows.\n  aircost-admin verify-listings [--limit 10] [--listing-id LISTING_ID | --after-listing-id LISTING_ID] [--preflight | --preview | --apply] [--database {DEFAULT_DATABASE_PATH}]\n    Runs the permanent aircraft, avionics, and listing-finalization verifier. Provider-free preflight is the default. --preview permits accounted Gemini requests without domain writes; --apply performs guarded, idempotent writes. FAA_DRS_API_KEY enables unknown-aircraft grounding; without it those aircraft remain pending while other safe work can continue.\n  aircost-admin cleanup-orphans [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin curate-avionics [--limit ROWS] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin enrich-avionics [--limit 10] [--listing-id LISTING_ID] [--value-reference-year 2026] [--refresh-existing] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin snapshot-valuations [--max-age-days 180] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin fit-valuation --kind structural|dnn --snapshot-id ID [--maximum-epochs 500] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin validate-valuation --model-version-id ID [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin activate-valuation --model-version-id ID [--database {DEFAULT_DATABASE_PATH}]"
    );
    println!(
        "  aircost-admin stage-listing-reviews [--limit 100] [--listing-id LISTING_ID] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Prepares pending reviews from retained extraction data without Gemini, catalog writes, or listing-link writes; dry-run is the default."
    );
    println!(
        "  aircost-admin audit-avionics-duplicates [--database {DEFAULT_DATABASE_PATH}]\n    Reports model collisions by stored keys, current canonical maker/product keys, and exact maker-scoped stable-identifier kind/value pairs without writing.\n  aircost-admin consolidate-legacy-avionics [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Previews or applies explicitly verified unreviewed product duplicates. Automatic product consolidation requires every pair in a component to share the same non-null manufacturer identifier kind and normalized value inside one evidence-authorized manufacturer identity scope. Raw manufacturer spellings remain immutable source history; manufacturer aliases are resolved only through identity membership or redirect. Dry-run is the default."
    );
    println!(
        "  aircost-admin reconcile-replay-avionics --listing-id ID --submission-id ID [--apply] [--database TARGET]\n    Provider-free audit of occurrence coverage. --apply records only provable current links; missing components remain unknown and are never inferred as discards."
    );
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::*;

    fn unique_test_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "aircost-admin-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    fn write_faa_archive_fixture(path: &PathBuf) {
        const MASTER: &str = "\u{feff}N-NUMBER,SERIAL NUMBER,MFR MDL CODE,ENG MFR MDL,YEAR MFR,NAME,STREET,MODE S CODE\n123AB,182-01234,2072738,41528,2006,PRIVATE OWNER,SECRET ADDRESS,50000000\n";
        const AIRCRAFT: &str = "\u{feff}CODE,MFR,MODEL,TYPE-ACFT,TYPE-ENG,AC-CAT,BUILD-CERT-IND,NO-ENG,NO-SEATS,AC-WEIGHT,SPEED,TC-DATA-SHEET,TC-DATA-HOLDER\n2072738,CESSNA AIRCRAFT CO,182T,4,1,1,0,01,004,CLASS 1,0145,3A13,TEXTRON AVIATION INC\n";
        const ENGINE: &str = "\u{feff}CODE,MFR,MODEL,TYPE,HORSEPOWER,THRUST\n41528,LYCOMING,IO-540-AB1A5,1,00230,000000\n";
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, contents) in [
            ("MASTER.txt", MASTER),
            ("ACFTREF.txt", AIRCRAFT),
            ("ENGINE.txt", ENGINE),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }

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
            "--archive",
            "/tmp/ReleasableAircraft.zip",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn publish_aircraft_reference_cli_is_dry_run_by_default() {
        let AdminCommand::PublishAircraftReference {
            database,
            draft,
            apply,
        } = parse_args(
            [
                "publish-aircraft-reference",
                "--database",
                "sqlite::memory:",
                "--draft",
                "/tmp/reference.json",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap()
        else {
            panic!("expected publish-aircraft-reference command")
        };
        assert_eq!(database, "sqlite::memory:");
        assert_eq!(draft, PathBuf::from("/tmp/reference.json"));
        assert!(!apply);
    }

    #[test]
    fn publish_aircraft_reference_cli_requires_draft_and_explicit_apply() {
        let error = parse_args(
            ["publish-aircraft-reference"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap_err();
        assert!(error.to_string().contains("--draft is required"));

        let command = parse_args(
            [
                "publish-aircraft-reference",
                "--draft",
                "/tmp/reference.json",
                "--apply",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            command,
            AdminCommand::PublishAircraftReference { apply: true, .. }
        ));
    }

    #[test]
    fn import_faa_registry_cli_is_dry_run_by_default() {
        let command = parse_args(faa_import_args()).unwrap();
        let AdminCommand::ImportFaaRegistry {
            database,
            archive,
            explicit_targets,
            apply,
        } = command
        else {
            panic!("expected import-faa-registry command")
        };
        assert_eq!(database, "sqlite::memory:");
        assert_eq!(archive, PathBuf::from("/tmp/ReleasableAircraft.zip"));
        assert_eq!(explicit_targets, ExplicitNNumberTargets::default());
        assert!(!apply);
    }

    #[tokio::test]
    async fn import_faa_registry_dry_run_keeps_existing_sqlite_byte_exact() {
        let database_path = unique_test_path("faa-dry-run", "sqlite3");
        let archive_path = unique_test_path("faa-dry-run", "zip");
        let database_url = format!("sqlite://{}", database_path.display());
        let db = aircost_rs::db::AppDb::connect(&database_url).await.unwrap();
        db.close().await;
        write_faa_archive_fixture(&archive_path);
        let before = fs::read(&database_path).unwrap();

        let report = import_faa_registry(
            database_url,
            archive_path.clone(),
            ExplicitNNumberTargets::parse(vec!["N123AB".to_string()]).unwrap(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(report["dry_run"], true);
        assert_eq!(report["stored"], serde_json::Value::Null);
        assert_eq!(fs::read(&database_path).unwrap(), before);
        assert!(!PathBuf::from(format!("{}-wal", database_path.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", database_path.display())).exists());
        fs::remove_file(database_path).unwrap();
        fs::remove_file(archive_path).unwrap();
    }

    #[tokio::test]
    async fn import_faa_registry_dry_run_keeps_missing_sqlite_absent() {
        let database_path = unique_test_path("faa-dry-run-missing", "sqlite3");
        let database_url = format!("sqlite://{}", database_path.display());
        assert!(!database_path.exists());
        let error = import_faa_registry(
            database_url,
            unique_test_path("unused-faa-dry-run", "zip"),
            ExplicitNNumberTargets::parse(vec!["N123AB".to_string()]).unwrap(),
            false,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("could not open diagnostic SQLite database"));
        assert!(!database_path.exists());
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn import_faa_registry_dry_run_keeps_postgres_rows_and_markers_unchanged() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let archive_path = unique_test_path("faa-pg-dry-run", "zip");
        write_faa_archive_fixture(&archive_path);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let before_counts = sqlx::query_as::<_, (i64, i64)>(
            "SELECT (SELECT count(*) FROM public.faa_registry_snapshots), \
                    (SELECT count(*) FROM public.curation_evidence_sources)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let before_markers = sqlx::query_as::<_, (String, String)>(
            "SELECT migration_name, installed_at FROM public.schema_migration_contracts \
             WHERE migration_name IN \
               ('20260819_faa_reference_reachability', '20260820_faa_record_hash_domain') \
             ORDER BY migration_name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        let report = import_faa_registry(
            database_url,
            archive_path.clone(),
            ExplicitNNumberTargets::parse(vec!["N123AB".to_string()]).unwrap(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(report["dry_run"], true);
        assert_eq!(report["stored"], serde_json::Value::Null);
        let after_counts = sqlx::query_as::<_, (i64, i64)>(
            "SELECT (SELECT count(*) FROM public.faa_registry_snapshots), \
                    (SELECT count(*) FROM public.curation_evidence_sources)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let after_markers = sqlx::query_as::<_, (String, String)>(
            "SELECT migration_name, installed_at FROM public.schema_migration_contracts \
             WHERE migration_name IN \
               ('20260819_faa_reference_reachability', '20260820_faa_record_hash_domain') \
             ORDER BY migration_name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(after_counts, before_counts);
        assert_eq!(after_markers, before_markers);
        pool.close().await;
        fs::remove_file(archive_path).unwrap();
    }

    #[test]
    fn import_faa_registry_cli_rejects_operator_supplied_snapshot_date() {
        let mut args = faa_import_args();
        args.extend(
            ["--snapshot-date", "2026-07-20"]
                .into_iter()
                .map(str::to_string),
        );
        assert!(parse_args(args)
            .unwrap_err()
            .to_string()
            .contains("unknown import-faa-registry argument: --snapshot-date"));
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
    fn import_faa_registry_cli_requires_explicit_apply_and_archive() {
        let mut args = faa_import_args();
        args.push("--apply".to_string());
        assert!(matches!(
            parse_args(args).unwrap(),
            AdminCommand::ImportFaaRegistry { apply: true, .. }
        ));

        let missing_archive = faa_import_args()
            .into_iter()
            .filter(|value| value != "--archive" && value != "/tmp/ReleasableAircraft.zip")
            .collect::<Vec<_>>();
        assert!(parse_args(missing_archive)
            .unwrap_err()
            .to_string()
            .contains("--archive is required"));

        for removed_flag in [
            "--master",
            "--aircraft-reference",
            "--engine-reference",
            "--archive-sha256",
        ] {
            let mut args = faa_import_args();
            args.extend([removed_flag.to_string(), "removed".to_string()]);
            let error = parse_args(args).unwrap_err().to_string();
            assert!(error.contains("unknown import-faa-registry argument"));
        }
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

    #[test]
    fn verify_listings_cli_is_zero_call_preflight_by_default() {
        let command = parse_args(
            [
                "verify-listings",
                "--database",
                "sqlite::memory:",
                "--listing-id",
                "29",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();

        let AdminCommand::VerifyListings {
            database,
            mode,
            limit,
            listing_id,
            after_listing_id,
        } = command
        else {
            panic!("expected verify-listings command")
        };
        assert_eq!(database, "sqlite::memory:");
        assert_eq!(mode, ListingVerificationCommandMode::Preflight);
        assert_eq!(limit, 10);
        assert_eq!(listing_id, Some(29));
        assert_eq!(after_listing_id, None);
    }

    #[test]
    fn verify_listings_cli_parses_apply_limit_and_cursor() {
        let command = parse_args(
            [
                "verify-listings",
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

        let AdminCommand::VerifyListings {
            mode,
            limit,
            listing_id,
            after_listing_id,
            ..
        } = command
        else {
            panic!("expected verify-listings command")
        };
        assert_eq!(mode, ListingVerificationCommandMode::Apply);
        assert_eq!(limit, 7);
        assert_eq!(listing_id, None);
        assert_eq!(after_listing_id, Some(29));
    }

    #[test]
    fn verify_listings_cli_requires_explicit_paid_preview() {
        let command = parse_args(
            ["verify-listings", "--preview"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            command,
            AdminCommand::VerifyListings {
                mode: ListingVerificationCommandMode::Preview,
                ..
            }
        ));
    }

    #[test]
    fn verify_listings_cli_rejects_conflicting_mode_and_scope_flags() {
        for arguments in [
            vec!["verify-listings", "--preflight", "--apply"],
            vec![
                "verify-listings",
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

    #[test]
    fn replay_phase_commands_are_dry_run_by_default() {
        let export = parse_args(
            [
                "export-replay-manifest",
                "--all-bound",
                "--output",
                "/tmp/captures.json",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            export,
            AdminCommand::ExportReplayManifest { apply: false, .. }
        ));
        let import = parse_args(
            [
                "import-replay-manifest",
                "--source-database",
                "/tmp/source.sqlite3",
                "--manifest",
                "/tmp/captures.json",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            import,
            AdminCommand::ImportReplayManifest { apply: false, .. }
        ));
        let batch = parse_args(
            [
                "replay-captures",
                "--manifest",
                "/tmp/captures.json",
                "--phase",
                "materialization",
                "--submission-id",
                "7",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            batch,
            AdminCommand::ReplayCaptures {
                phase: ReplayPhase::Materialization,
                submission_id: Some(7),
                apply: false,
                recover_stale: false,
                ..
            }
        ));
        let extraction = parse_args(
            ["replay-extraction", "--submission-id", "7"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            extraction,
            AdminCommand::ReplayExtraction {
                submission_id: 7,
                apply: false,
                ..
            }
        ));
        let listing = parse_args(
            ["replay-listing", "--submission-id", "7"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            listing,
            AdminCommand::ReplayListing {
                submission_id: 7,
                apply: false,
                ..
            }
        ));
        let reconcile = parse_args(
            [
                "reconcile-replay-avionics",
                "--listing-id",
                "11",
                "--submission-id",
                "7",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        assert!(matches!(
            reconcile,
            AdminCommand::ReconcileReplayAvionics {
                listing_id: 11,
                submission_id: 7,
                apply: false,
                ..
            }
        ));
    }
}
