use std::collections::BTreeSet;
use std::env;
use std::fs::File;
use std::path::PathBuf;

use aircost_rs::aircraft::curation::workflow::curate_aircraft_hierarchy_observations_with_config;
use aircost_rs::aircraft::enrich_aircraft_specs_from_plugin_submissions;
use aircost_rs::aircraft::faa::{
    listing_targets, parse_release, store_release, ExplicitNNumberTargets, FaaImportTargets,
    ReleaseMetadata, ReleaseReaders,
};
use aircost_rs::avionics::repopulate::repopulate_listing_avionics;
use aircost_rs::avionics::{
    curate_avionics_models_with_gemini, enrich_listing_avionics_metadata,
    enrich_missing_avionics_metadata, enrich_model_year_avionics_and_price_points,
    normalize_avionics_models,
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
use aircost_rs::listings::{heal_aircraft_models, normalize_variants_for_model};
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
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let api_key = env::var("GEMINI_API_KEY")
                .context("GEMINI_API_KEY is required for curate-aircraft-hierarchy")?;
            let runtime_config = GeminiRuntimeConfig::from_environment()?;
            let client = GeminiInteractionsClient::new(api_key)?
                .with_usage_store(GeminiUsageStore::new(&db));
            let report = curate_aircraft_hierarchy_observations_with_config(
                &db,
                &client,
                listing_limit,
                listing_id,
                cluster_limit,
                &runtime_config,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
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
        AdminCommand::NormalizeVariants {
            database,
            manufacturer,
            model,
            apply,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
            let report =
                normalize_variants_for_model(&db, &extractor, &manufacturer, &model, apply).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::HealAircraftModels {
            database,
            apply,
            limit,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
            let report = heal_aircraft_models(&db, &extractor, apply, limit).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AdminCommand::RepopulateAvionics {
            database,
            apply,
            limit,
            listing_id,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let extractor = GeminiListingExtractor::from_environment_with_usage(&db)?;
            let report =
                repopulate_listing_avionics(&db, &extractor, apply, limit, listing_id).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
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
        AdminCommand::NormalizeAvionics { database, apply } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let report = normalize_avionics_models(&db, apply).await?;
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
    NormalizeVariants {
        database: String,
        manufacturer: String,
        model: String,
        apply: bool,
    },
    HealAircraftModels {
        database: String,
        apply: bool,
        limit: i64,
    },
    RepopulateAvionics {
        database: String,
        apply: bool,
        limit: i64,
        listing_id: Option<i64>,
    },
    EnrichAvionics {
        database: String,
        apply: bool,
        limit: i64,
        value_reference_year: Option<i64>,
        refresh_existing: bool,
        listing_id: Option<i64>,
    },
    NormalizeAvionics {
        database: String,
        apply: bool,
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
        "normalize-variants" => parse_normalize_variants_args(args),
        "heal-aircraft-models" => parse_heal_aircraft_models_args(args),
        "repopulate-avionics" => parse_repopulate_avionics_args(args),
        "enrich-avionics" => parse_enrich_avionics_args(args),
        "normalize-avionics" => parse_normalize_avionics_args(args),
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

    Ok(AdminCommand::CurateAircraftHierarchy {
        database: database_url_from_arg(database),
        listing_limit,
        cluster_limit,
        listing_id,
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
    let mut apply = false;
    let mut limit = 10_i64;
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
            _ => bail!("unknown repopulate-avionics argument: {arg}"),
        }
    }

    if limit < 1 {
        bail!("--limit must be at least 1");
    }
    if listing_id.is_some_and(|listing_id| listing_id < 1) {
        bail!("--listing-id must be a positive integer");
    }

    Ok(AdminCommand::RepopulateAvionics {
        database: database_url_from_arg(database),
        apply,
        limit,
        listing_id,
    })
}

fn parse_heal_aircraft_models_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut apply = false;
    let mut limit = 100_i64;
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
            _ => bail!("unknown heal-aircraft-models argument: {arg}"),
        }
    }

    Ok(AdminCommand::HealAircraftModels {
        database: database_url_from_arg(database),
        apply,
        limit,
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

fn parse_normalize_variants_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
    let mut database = None;
    let mut manufacturer = None;
    let mut model = None;
    let mut apply = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" | "--database-url" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--manufacturer" => {
                manufacturer = Some(
                    args.next()
                        .context("--manufacturer requires a value")?
                        .trim()
                        .to_string(),
                );
            }
            "--model" => {
                model = Some(
                    args.next()
                        .context("--model requires a value")?
                        .trim()
                        .to_string(),
                );
            }
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown normalize-variants argument: {arg}"),
        }
    }

    let manufacturer = required_arg(manufacturer, "--manufacturer")?;
    let model = required_arg(model, "--model")?;
    Ok(AdminCommand::NormalizeVariants {
        database: database_url_from_arg(database),
        manufacturer,
        model,
        apply,
    })
}

fn parse_normalize_avionics_args(args: impl IntoIterator<Item = String>) -> Result<AdminCommand> {
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
            _ => bail!("unknown normalize-avionics argument: {arg}"),
        }
    }

    Ok(AdminCommand::NormalizeAvionics {
        database: database_url_from_arg(database),
        apply,
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

fn required_arg(value: Option<String>, name: &str) -> Result<String> {
    value
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{name} is required"))
}

fn print_usage() {
    println!(
        "Usage:\n  aircost-admin import-faa-registry --master MASTER.txt --aircraft-reference ACFTREF.txt --engine-reference ENGINE.txt --snapshot-date YYYY-MM-DD --archive-sha256 HEX [--include-n-number N123AB]... [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Scans the official files and stores only target-scoped, non-PII FAA evidence. Explicit N-number targets are normalized, validated, and merged with listing and pending-submission targets; dry-run is the default.\n  aircost-admin curate-aircraft-hierarchy [--listing-limit 25] [--cluster-limit 5] [--listing-id LISTING_ID] [--database {DEFAULT_DATABASE_PATH}]\n    Read-only grounded Gemini hierarchy review; never writes canonical or staging data.\n  aircost-admin benchmark-gemini [--task listing|metadata|avionics|visual]... [--model PINNED_MODEL]... [--listing-limit SAMPLE_SIZE] [--submission-id ID]... [--max-avionics-per-listing 1] [--max-visual-assets 8] [--seed TEXT] [--config FILE] [--execute] [--database {DEFAULT_DATABASE_PATH}]\n    Without --execute, exports a deterministic real-data suite using benchmark selection defaults from Gemini config. With --execute, makes paid calls and writes only gemini_api_usage accounting rows.\n  aircost-admin normalize-variants --manufacturer Cirrus --model SR22 [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin heal-aircraft-models [--limit 100] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin repopulate-avionics [--limit 10] [--listing-id LISTING_ID] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n    Budget one listing-extraction call per incompatible legacy payload, approximately two grounded Gemini calls per attempted identity, and correction retries.\n  aircost-admin normalize-avionics [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin cleanup-orphans [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin curate-avionics [--limit ROWS] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin enrich-avionics [--limit 10] [--listing-id LISTING_ID] [--value-reference-year 2026] [--refresh-existing] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin enrich-model-year-avionics [--limit 10] [--value-reference-year 2026] [--refresh-existing] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin enrich-aircraft-specs [--limit 10] [--value-reference-year 2026] [--refresh-existing] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin snapshot-valuations [--max-age-days 180] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin fit-valuation --kind structural|dnn --snapshot-id ID [--maximum-epochs 500] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin validate-valuation --model-version-id ID [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin activate-valuation --model-version-id ID [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin fit-depreciation [legacy] [--min-model-samples 4] [--value-reference-year 2026] [--apply] [--database {DEFAULT_DATABASE_PATH}]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn curate_aircraft_hierarchy_cli_is_read_only_with_bounded_defaults() {
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
        } = command
        else {
            panic!("expected curate-aircraft-hierarchy command")
        };
        assert_eq!(database, "sqlite::memory:");
        assert_eq!(listing_limit, 25);
        assert_eq!(cluster_limit, 5);
        assert_eq!(listing_id, None);
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
        } = command
        else {
            panic!("expected curate-aircraft-hierarchy command")
        };
        assert_eq!(database, "postgres://aircost.test/db");
        assert_eq!(listing_limit, 80);
        assert_eq!(cluster_limit, 12);
        assert_eq!(listing_id, Some(29));
    }

    #[test]
    fn curate_aircraft_hierarchy_cli_rejects_apply() {
        let error = parse_args(
            ["curate-aircraft-hierarchy", "--apply"]
                .into_iter()
                .map(str::to_string),
        )
        .err()
        .expect("--apply must not be accepted by a read-only workflow");

        assert!(error
            .to_string()
            .contains("unknown curate-aircraft-hierarchy argument: --apply"));
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
    fn repopulate_avionics_cli_is_dry_run_by_default() {
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
            apply,
            limit,
            listing_id,
        } = command
        else {
            panic!("expected repopulate-avionics command")
        };
        assert_eq!(database, "sqlite::memory:");
        assert!(!apply);
        assert_eq!(limit, 10);
        assert_eq!(listing_id, Some(29));
    }

    #[test]
    fn repopulate_avionics_cli_parses_apply_and_limit() {
        let command = parse_args(
            ["repopulate-avionics", "--apply", "--limit", "7"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();

        let AdminCommand::RepopulateAvionics {
            apply,
            limit,
            listing_id,
            ..
        } = command
        else {
            panic!("expected repopulate-avionics command")
        };
        assert!(apply);
        assert_eq!(limit, 7);
        assert_eq!(listing_id, None);
    }
}
