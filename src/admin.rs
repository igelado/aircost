use std::env;

use aircost_rs::aircraft::enrich_aircraft_specs_from_plugin_submissions;
use aircost_rs::avionics::{
    curate_avionics_models_with_gemini, enrich_listing_avionics_metadata,
    enrich_missing_avionics_metadata, enrich_model_year_avionics_and_price_points,
    normalize_avionics_models,
};
use aircost_rs::cleanup::cleanup_orphan_records;
use aircost_rs::db::{database_url_from_arg, DEFAULT_DATABASE_PATH};
use aircost_rs::extract::GeminiListingExtractor;
use aircost_rs::fit::{fit_depreciation_profiles, fit_structural_valuation};
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
        AdminCommand::NormalizeVariants {
            database,
            manufacturer,
            model,
            apply,
        } => {
            let db = aircost_rs::db::AppDb::connect(&database).await?;
            let extractor = GeminiListingExtractor::from_environment()?;
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
            let extractor = GeminiListingExtractor::from_environment()?;
            let report = heal_aircraft_models(&db, &extractor, apply, limit).await?;
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
            let extractor = GeminiListingExtractor::from_environment()?;
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
            let extractor = GeminiListingExtractor::from_environment()?;
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
            let extractor = GeminiListingExtractor::from_environment()?;
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
            let extractor = GeminiListingExtractor::from_environment()?;
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

enum AdminCommand {
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
        "normalize-variants" => parse_normalize_variants_args(args),
        "heal-aircraft-models" => parse_heal_aircraft_models_args(args),
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
        "Usage:\n  aircost-admin normalize-variants --manufacturer Cirrus --model SR22 [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin heal-aircraft-models [--limit 100] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin normalize-avionics [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin cleanup-orphans [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin curate-avionics [--limit ROWS] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin enrich-avionics [--limit 10] [--listing-id LISTING_ID] [--value-reference-year 2026] [--refresh-existing] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin enrich-model-year-avionics [--limit 10] [--value-reference-year 2026] [--refresh-existing] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin enrich-aircraft-specs [--limit 10] [--value-reference-year 2026] [--refresh-existing] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin snapshot-valuations [--max-age-days 180] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin fit-valuation --kind structural|dnn --snapshot-id ID [--maximum-epochs 500] [--apply] [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin validate-valuation --model-version-id ID [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin activate-valuation --model-version-id ID [--database {DEFAULT_DATABASE_PATH}]\n  aircost-admin fit-depreciation [legacy] [--min-model-samples 4] [--value-reference-year 2026] [--apply] [--database {DEFAULT_DATABASE_PATH}]"
    );
}
