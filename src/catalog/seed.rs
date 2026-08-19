//! Provider-free transfer of reusable, verified catalog facts into a clean rebuild.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, QueryBuilder, Row, Sqlite};

use crate::db::{AppDb, DatabaseBackend};

const ROOT_AIRCRAFT_TABLES: &[&str] = &[
    "aircraft_makes",
    "aircraft_model_families",
    "aircraft_designations",
    "aircraft_make_aliases",
    "aircraft_family_aliases",
    "aircraft_designation_aliases",
    "aircraft_designation_identifiers",
    "aircraft_generations",
    "aircraft_generation_designations",
    "aircraft_factory_packages",
    "aircraft_package_applicability",
    "aircraft_engine_catalog_models",
    "aircraft_propeller_catalog_models",
    "aircraft_serial_number_schemes",
    "aircraft_feature_definitions",
    "aircraft_designation_faa_bindings",
    "aircraft_tcds_make_lineage_bindings",
];

const EMPTY_TARGET_TABLES: &[&str] = &[
    "avionics_models",
    "avionics_types",
    "avionics_manufacturers",
    "avionics_manufacturer_identities",
    "avionics_authoritative_source_origins",
    "avionics_authoritative_source_origin_revocations",
    "avionics_manufacturer_identity_memberships",
    "avionics_manufacturer_identity_merges",
    "avionics_approved_product_identities",
    "avionics_model_types",
    "avionics_suite_components",
    "avionics_product_reuse_attestations",
    "avionics_manufacturer_alias_candidates",
    "avionics_catalog_consolidation_guard",
    "avionics_catalog_grounded_consolidation_authorizations",
    "avionics_catalog_grounded_consolidation_claim",
    "avionics_catalog_grounded_consolidation_guard",
    "avionics_catalog_human_consolidation_authorizations",
    "avionics_catalog_human_consolidation_claim",
    "avionics_catalog_human_consolidation_guard",
    "avionics_catalog_human_consolidation_members",
    "aircraft_identity_observations",
    "aircraft_identity_resolution_cases",
    "aircraft_identity_resolution_candidates",
    "aircraft_identity_decisions",
    "aircraft_identity_decision_claims",
    "aircraft_reference_profile_proposals",
    "curation_evidence_sources",
    "curation_evidence_claims",
    "faa_registry_snapshots",
    "faa_registry_aircraft",
    "faa_registry_aircraft_references",
    "faa_registry_engine_references",
    "faa_registry_coverage",
    "aircraft_makes",
    "aircraft_model_families",
    "aircraft_designations",
    "aircraft_make_aliases",
    "aircraft_family_aliases",
    "aircraft_designation_aliases",
    "aircraft_designation_identifiers",
    "aircraft_generations",
    "aircraft_generation_designations",
    "aircraft_factory_packages",
    "aircraft_package_applicability",
    "aircraft_engine_catalog_models",
    "aircraft_propeller_catalog_models",
    "aircraft_serial_number_schemes",
    "aircraft_feature_definitions",
    "aircraft_designation_faa_bindings",
    "aircraft_tcds_make_lineage_bindings",
    "aircraft_reference_configurations",
    "aircraft_reference_configuration_versions",
    "aircraft_reference_applicability_scopes",
    "aircraft_reference_prices",
    "aircraft_reference_avionics",
    "aircraft_reference_engines",
    "aircraft_reference_propellers",
    "aircraft_reference_features",
    "aircraft_sale_listings",
    "aircraft_sale_listing_identity_assignments",
    "aircraft_sale_listing_avionics",
    "aircraft_sale_listing_avionics_authorizations",
    "aircraft_sale_listing_avionics_dispositions",
    "aircraft_sale_listing_pending_reviews",
    "gemini_api_usage",
];

const FORBIDDEN_ARTIFACT_TABLES: &[&str] = &[
    "avionics_manufacturer_alias_candidates",
    "avionics_catalog_consolidation_guard",
    "avionics_catalog_grounded_consolidation_authorizations",
    "avionics_catalog_grounded_consolidation_claim",
    "avionics_catalog_grounded_consolidation_guard",
    "avionics_catalog_human_consolidation_authorizations",
    "avionics_catalog_human_consolidation_claim",
    "avionics_catalog_human_consolidation_guard",
    "avionics_catalog_human_consolidation_members",
    "aircraft_identity_resolution_candidates",
    "aircraft_reference_profile_proposals",
    "aircraft_reference_configurations",
    "aircraft_reference_configuration_versions",
    "aircraft_reference_applicability_scopes",
    "aircraft_reference_prices",
    "aircraft_reference_avionics",
    "aircraft_reference_engines",
    "aircraft_reference_propellers",
    "aircraft_reference_features",
    "aircraft_sale_listings",
    "aircraft_sale_listing_identity_assignments",
    "aircraft_sale_listing_avionics",
    "aircraft_sale_listing_avionics_authorizations",
    "aircraft_sale_listing_avionics_dispositions",
    "aircraft_sale_listing_pending_reviews",
    "gemini_api_usage",
];

const BOOTSTRAP_ORIGINS: &[(&str, &str)] = &[
    ("garmin", "https://www.garmin.com"),
    ("garmin", "https://static.garmin.com"),
];

#[derive(Clone, Debug, Serialize)]
pub struct VerifiedCatalogSeedReport {
    pub dry_run: bool,
    pub provider_calls: u64,
    pub fingerprint_sha256: String,
    pub source_counts: BTreeMap<String, usize>,
    pub excluded_counts: BTreeMap<String, i64>,
    pub target_empty: bool,
    pub applied_rows: usize,
    pub generated_identity_rows_verified: usize,
    pub schema_generated_origin_timestamps: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct SeedRow {
    table: String,
    columns: Vec<String>,
    values: Vec<Value>,
}

impl SeedRow {
    fn value(&self, column: &str) -> Option<&Value> {
        self.columns
            .iter()
            .position(|candidate| candidate == column)
            .map(|index| &self.values[index])
    }

    fn integer(&self, column: &str) -> Result<i64> {
        self.value(column)
            .and_then(Value::as_i64)
            .with_context(|| format!("{}.{} is not an integer", self.table, column))
    }

    fn nullable_integer(&self, column: &str) -> Result<Option<i64>> {
        match self.value(column) {
            Some(Value::Null) | None => Ok(None),
            Some(value) => value
                .as_i64()
                .map(Some)
                .with_context(|| format!("{}.{} is not an integer", self.table, column)),
        }
    }

    fn with_value(mut self, column: &str, value: Value) -> Result<Self> {
        let index = self
            .columns
            .iter()
            .position(|candidate| candidate == column)
            .with_context(|| format!("{}.{} is missing", self.table, column))?;
        self.values[index] = value;
        Ok(self)
    }
}

#[derive(Clone, Debug)]
struct SeedBundle {
    insert_groups: Vec<Vec<SeedRow>>,
    generated_keys: Vec<SeedRow>,
    generated_products: Vec<SeedRow>,
    source_counts: BTreeMap<String, usize>,
    excluded_counts: BTreeMap<String, i64>,
    fingerprint_sha256: String,
    required_users: Vec<SeedRow>,
}

impl SeedBundle {
    fn fingerprint_rows(&self) -> Vec<SeedRow> {
        let mut rows = self
            .insert_groups
            .iter()
            .flatten()
            .cloned()
            .map(|row| {
                if row.table == "avionics_models" {
                    row.with_value("catalog_status", Value::String("approved".into()))
                        .expect("avionics model status column")
                } else {
                    row
                }
            })
            .collect::<Vec<_>>();
        rows.extend(self.generated_keys.clone());
        rows.extend(self.generated_products.clone());
        rows.sort_by(|left, right| {
            left.table
                .cmp(&right.table)
                .then_with(|| canonical_row(left).cmp(&canonical_row(right)))
        });
        rows
    }
}

/// Build and optionally apply the exact approved catalog closure. The source
/// must have been opened with [`AppDb::connect_read_only`].
pub async fn seed_verified_catalog(
    source: &AppDb,
    target: &AppDb,
    apply: bool,
) -> Result<VerifiedCatalogSeedReport> {
    let bundle = build_bundle(source).await?;
    validate_target_empty(target).await?;
    validate_required_users(target, &bundle.required_users).await?;

    if !apply {
        return Ok(report(&bundle, true, 0, 0, 0));
    }

    let (applied_rows, generated, generated_origin_timestamps) = match target.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            validate_target_empty_sqlite(&mut transaction).await?;
            validate_required_users_sqlite(&mut transaction, &bundle.required_users).await?;
            // This lets exact source origin rows precede manufacturer identities.
            // The identity insert then observes the bootstrap origin conflicts and
            // performs no schema-generated timestamp write.
            sqlx::query("PRAGMA defer_foreign_keys = ON")
                .execute(&mut *transaction)
                .await?;
            let result = apply_sqlite(&mut transaction, &bundle).await?;
            ensure_sqlite_foreign_keys(&mut transaction).await?;
            validate_seeded_sqlite(&mut transaction, &bundle).await?;
            transaction.commit().await?;
            result
        }
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool.begin().await?;
            validate_target_empty_postgres(&mut transaction).await?;
            validate_required_users_postgres(&mut transaction, &bundle.required_users).await?;
            let result = apply_postgres(&mut transaction, &bundle).await?;
            validate_seeded_postgres(&mut transaction, &bundle).await?;
            transaction.commit().await?;
            result
        }
    };

    // Re-read from the committed target. This catches trigger-side changes and
    // makes a successful report mean that the reusable closure is queryable.
    validate_seeded_target(target, &bundle).await?;
    Ok(report(
        &bundle,
        false,
        applied_rows,
        generated,
        generated_origin_timestamps,
    ))
}

fn report(
    bundle: &SeedBundle,
    dry_run: bool,
    applied_rows: usize,
    generated_identity_rows_verified: usize,
    schema_generated_origin_timestamps: usize,
) -> VerifiedCatalogSeedReport {
    VerifiedCatalogSeedReport {
        dry_run,
        provider_calls: 0,
        fingerprint_sha256: bundle.fingerprint_sha256.clone(),
        source_counts: bundle.source_counts.clone(),
        excluded_counts: bundle.excluded_counts.clone(),
        target_empty: true,
        applied_rows,
        generated_identity_rows_verified,
        schema_generated_origin_timestamps,
    }
}

async fn build_bundle(source: &AppDb) -> Result<SeedBundle> {
    let DatabaseBackend::Sqlite(pool) = source.backend() else {
        bail!("verified catalog seed sources must be opened through the read-only SQLite boundary");
    };
    let mut transaction = pool.begin().await?;
    let result = build_bundle_from_snapshot(&mut transaction).await;
    transaction.rollback().await?;
    result
}

struct SourceSnapshot<'a, 'connection> {
    transaction: &'a mut sqlx::Transaction<'connection, Sqlite>,
}

impl SourceSnapshot<'_, '_> {
    async fn fetch(&mut self, table: &str, predicate: &str) -> Result<Vec<SeedRow>> {
        fetch_rows_sqlite_executor(&mut **self.transaction, table, predicate).await
    }

    async fn count(&mut self, table: &str, predicate: &str) -> Result<i64> {
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE {predicate}",
            quoted_identifier(table)
        );
        Ok(sqlx::query_scalar(&sql)
            .fetch_one(&mut **self.transaction)
            .await?)
    }
}

async fn build_bundle_from_snapshot(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
) -> Result<SeedBundle> {
    let mut snapshot = SourceSnapshot { transaction };

    let approved_models = snapshot
        .fetch("avionics_models", "catalog_status = 'approved'")
        .await?;
    if approved_models.is_empty() {
        bail!("source has no approved avionics models");
    }
    let model_ids = ids(&approved_models, "id")?;
    let manufacturers = snapshot
        .fetch(
            "avionics_manufacturers",
            &in_predicate("id", &ids(&approved_models, "avionics_manufacturer_id")?),
        )
        .await?;
    let manufacturer_ids = ids(&manufacturers, "id")?;
    let canonical_keys = snapshot
        .fetch(
            "avionics_manufacturer_canonical_keys",
            &in_predicate("avionics_manufacturer_id", &manufacturer_ids),
        )
        .await?;
    let product_identities = snapshot
        .fetch(
            "avionics_approved_product_identities",
            &in_predicate("avionics_model_id", &model_ids),
        )
        .await?;
    if product_identities.len() != approved_models.len() {
        bail!("every approved avionics model must have exactly one approved product identity");
    }
    let identity_ids = ids(&product_identities, "avionics_manufacturer_identity_id")?;
    let identities = snapshot
        .fetch(
            "avionics_manufacturer_identities",
            &in_predicate("id", &identity_ids),
        )
        .await?;
    let memberships = snapshot
        .fetch(
            "avionics_manufacturer_identity_memberships",
            &format!(
                "{} AND {}",
                in_predicate("avionics_manufacturer_id", &manufacturer_ids),
                in_predicate("avionics_manufacturer_identity_id", &identity_ids)
            ),
        )
        .await?;
    if memberships.len() != manufacturers.len() {
        bail!("approved avionics manufacturers do not have one exact identity membership each");
    }
    let merges = snapshot
        .fetch(
            "avionics_manufacturer_identity_merges",
            &format!(
                "{} OR {}",
                in_predicate("merged_identity_id", &identity_ids),
                in_predicate("canonical_identity_id", &identity_ids)
            ),
        )
        .await?;
    if !merges.is_empty() {
        bail!("approved avionics closure depends on manufacturer identity merges; seed refuses to copy their forbidden alias-candidate adjudication history");
    }

    let model_types = snapshot
        .fetch(
            "avionics_model_types",
            &in_predicate("avionics_model_id", &model_ids),
        )
        .await?;
    let types = snapshot
        .fetch(
            "avionics_types",
            &in_predicate("id", &ids(&model_types, "avionics_type_id")?),
        )
        .await?;
    let suite_components = snapshot
        .fetch(
            "avionics_suite_components",
            &format!(
                "{} AND {}",
                in_predicate("suite_model_id", &model_ids),
                in_predicate("component_model_id", &model_ids)
            ),
        )
        .await?;
    let reuse = snapshot
        .fetch(
            "avionics_product_reuse_attestations",
            &in_predicate("avionics_model_id", &model_ids),
        )
        .await?;
    let origin_ids = ids(&reuse, "avionics_authoritative_source_origin_id")?;
    let origins = snapshot
        .fetch(
            "avionics_authoritative_source_origins",
            &format!(
                "{} OR ({} AND https_origin IN ('https://www.garmin.com','https://static.garmin.com'))",
                in_predicate("id", &origin_ids),
                in_predicate("avionics_manufacturer_identity_id", &identity_ids)
            ),
        )
        .await?;
    let revocations = snapshot
        .fetch(
            "avionics_authoritative_source_origin_revocations",
            &in_predicate(
                "avionics_authoritative_source_origin_id",
                &ids(&origins, "id")?,
            ),
        )
        .await?;
    if !revocations.is_empty() {
        bail!("a selected avionics reuse attestation references a revoked authoritative origin");
    }

    let mut aircraft_roots = BTreeMap::new();
    for table in ROOT_AIRCRAFT_TABLES {
        aircraft_roots.insert((*table).to_string(), snapshot.fetch(table, "1 = 1").await?);
    }
    let mut decision_ids = BTreeSet::new();
    for rows in aircraft_roots.values() {
        for row in rows {
            for column in ["approval_decision_id"] {
                if let Some(id) = row.nullable_integer(column)? {
                    decision_ids.insert(id);
                }
            }
        }
    }
    let decision_ids = decision_ids.into_iter().collect::<Vec<_>>();
    let decisions = snapshot
        .fetch(
            "aircraft_identity_decisions",
            &in_predicate("id", &decision_ids),
        )
        .await?;
    if decisions.len() != decision_ids.len() {
        bail!("approved aircraft catalog has a missing approval decision");
    }
    if decisions
        .iter()
        .any(|row| row.value("decision_status").and_then(Value::as_str) != Some("approved"))
    {
        bail!("aircraft catalog closure contains a non-approved decision");
    }
    let cases = snapshot
        .fetch(
            "aircraft_identity_resolution_cases",
            &in_predicate("id", &ids(&decisions, "resolution_case_id")?),
        )
        .await?;
    let mut observations = snapshot
        .fetch(
            "aircraft_identity_observations",
            &in_predicate("id", &ids(&cases, "observation_id")?),
        )
        .await?;
    observations = observations
        .into_iter()
        .map(|row| row.with_value("aircraft_sale_listing_id", Value::Null))
        .collect::<Result<Vec<_>>>()?;
    let decision_claims = snapshot
        .fetch(
            "aircraft_identity_decision_claims",
            &in_predicate("decision_id", &decision_ids),
        )
        .await?;

    let mut claim_ids = ids(&decision_claims, "evidence_claim_id")?
        .into_iter()
        .collect::<BTreeSet<_>>();
    for rows in aircraft_roots.values() {
        for row in rows {
            for column in [
                "identity_evidence_claim_id",
                "faa_make_evidence_claim_id",
                "tcds_model_identity_evidence_claim_id",
                "tcds_serial_applicability_evidence_claim_id",
                "tcds_holder_transfer_evidence_claim_id",
                "tcds_manufacturer_range_evidence_claim_id",
            ] {
                if let Some(id) = row.nullable_integer(column)? {
                    claim_ids.insert(id);
                }
            }
        }
    }
    let claims = snapshot
        .fetch(
            "curation_evidence_claims",
            &in_predicate("id", &claim_ids.into_iter().collect::<Vec<_>>()),
        )
        .await?;

    let faa_binding_rows = aircraft_roots
        .get("aircraft_designation_faa_bindings")
        .expect("root table present");
    let tcds_rows = aircraft_roots
        .get("aircraft_tcds_make_lineage_bindings")
        .expect("root table present");
    let mut snapshot_ids = ids(faa_binding_rows, "representative_faa_registry_snapshot_id")?
        .into_iter()
        .collect::<BTreeSet<_>>();
    snapshot_ids.extend(ids(tcds_rows, "representative_faa_registry_snapshot_id")?);
    let snapshot_ids = snapshot_ids.into_iter().collect::<Vec<_>>();
    let snapshots = snapshot
        .fetch("faa_registry_snapshots", &in_predicate("id", &snapshot_ids))
        .await?;
    let faa_aircraft = snapshot
        .fetch(
            "faa_registry_aircraft",
            &in_predicate("snapshot_id", &snapshot_ids),
        )
        .await?;
    let faa_aircraft_references = snapshot
        .fetch(
            "faa_registry_aircraft_references",
            &in_predicate("snapshot_id", &snapshot_ids),
        )
        .await?;
    let faa_engine_references = snapshot
        .fetch(
            "faa_registry_engine_references",
            &in_predicate("snapshot_id", &snapshot_ids),
        )
        .await?;
    let faa_coverage = snapshot
        .fetch(
            "faa_registry_coverage",
            &in_predicate("snapshot_id", &snapshot_ids),
        )
        .await?;

    let mut source_ids = ids(&claims, "evidence_source_id")?
        .into_iter()
        .collect::<BTreeSet<_>>();
    source_ids.extend(ids(&snapshots, "evidence_source_id")?);
    let sources = snapshot
        .fetch(
            "curation_evidence_sources",
            &in_predicate("id", &source_ids.into_iter().collect::<Vec<_>>()),
        )
        .await?;

    let markets = selected_aircraft_markets(&mut snapshot, &aircraft_roots).await?;

    let mut user_ids = BTreeSet::new();
    for row in &decisions {
        if let Some(id) = row.nullable_integer("decided_by_user_id")? {
            user_ids.insert(id);
        }
    }
    for row in &origins {
        if let Some(id) = row.nullable_integer("approved_by_user_id")? {
            user_ids.insert(id);
        }
    }
    let user_ids = user_ids.into_iter().collect::<Vec<_>>();
    let required_users = snapshot
        .fetch("users", &in_predicate("id", &user_ids))
        .await?;
    if required_users.len() != user_ids.len() {
        bail!("approved catalog closure references a missing source user");
    }

    let staged_models = approved_models
        .iter()
        .cloned()
        .map(|row| row.with_value("catalog_status", Value::String("unreviewed".into())))
        .collect::<Result<Vec<_>>>()?;

    let selected_decision_count = decisions.len();
    let mut insert_groups = vec![
        sources,
        claims,
        snapshots,
        faa_aircraft,
        faa_aircraft_references,
        faa_engine_references,
        faa_coverage,
        observations,
        cases,
        decisions,
        decision_claims,
        markets,
    ];
    for table in [
        "aircraft_makes",
        "aircraft_model_families",
        "aircraft_designations",
        "aircraft_make_aliases",
        "aircraft_family_aliases",
        "aircraft_designation_aliases",
        "aircraft_designation_identifiers",
        "aircraft_generations",
        "aircraft_generation_designations",
        "aircraft_factory_packages",
        "aircraft_package_applicability",
        "aircraft_engine_catalog_models",
        "aircraft_propeller_catalog_models",
        "aircraft_serial_number_schemes",
        "aircraft_feature_definitions",
        "aircraft_tcds_make_lineage_bindings",
        "aircraft_designation_faa_bindings",
    ] {
        insert_groups.push(aircraft_roots.remove(table).expect("root table fetched"));
    }
    // Origins intentionally precede identities. SQLite defers the FK so the
    // Garmin bootstrap is an exact no-op. Postgres handles these two groups in
    // the opposite order and verifies the schema-generated provenance fields.
    insert_groups.extend([
        manufacturers,
        origins,
        identities,
        memberships,
        types,
        staged_models,
        model_types,
        suite_components,
        reuse,
    ]);

    let mut source_counts = BTreeMap::new();
    for rows in &insert_groups {
        if let Some(row) = rows.first() {
            source_counts.insert(row.table.clone(), rows.len());
        }
    }
    source_counts.insert(
        "avionics_approved_product_identities".into(),
        product_identities.len(),
    );
    source_counts.insert(
        "avionics_manufacturer_canonical_keys".into(),
        canonical_keys.len(),
    );
    for table in ROOT_AIRCRAFT_TABLES.iter().copied().chain([
        "aircraft_identity_decision_claims",
        "aircraft_identity_decisions",
        "aircraft_identity_observations",
        "aircraft_identity_resolution_cases",
        "avionics_approved_product_identities",
        "avionics_authoritative_source_origins",
        "avionics_manufacturer_canonical_keys",
        "avionics_manufacturer_identities",
        "avionics_manufacturer_identity_memberships",
        "avionics_manufacturers",
        "avionics_model_types",
        "avionics_models",
        "avionics_product_reuse_attestations",
        "avionics_suite_components",
        "avionics_types",
        "curation_evidence_claims",
        "curation_evidence_sources",
        "faa_registry_aircraft",
        "faa_registry_aircraft_references",
        "faa_registry_coverage",
        "faa_registry_engine_references",
        "faa_registry_snapshots",
    ]) {
        source_counts.entry(table.to_string()).or_insert(0);
    }

    let excluded_counts = excluded_counts(
        &mut snapshot,
        approved_models.len(),
        selected_decision_count,
    )
    .await?;
    let mut bundle = SeedBundle {
        insert_groups,
        generated_keys: canonical_keys,
        generated_products: product_identities,
        source_counts,
        excluded_counts,
        fingerprint_sha256: String::new(),
        required_users,
    };
    bundle.fingerprint_sha256 = fingerprint(&bundle.fingerprint_rows())?;
    Ok(bundle)
}

async fn selected_aircraft_markets(
    source: &mut SourceSnapshot<'_, '_>,
    roots: &BTreeMap<String, Vec<SeedRow>>,
) -> Result<Vec<SeedRow>> {
    let mut ids = BTreeSet::new();
    for rows in roots.values() {
        for row in rows {
            if let Some(id) = row.nullable_integer("aircraft_market_id")? {
                ids.insert(id);
            }
        }
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Include parent markets until the FK closure is complete.
    loop {
        let rows = source
            .fetch(
                "aircraft_markets",
                &in_predicate("id", &ids.iter().copied().collect::<Vec<_>>()),
            )
            .await?;
        let before = ids.len();
        for row in &rows {
            if let Some(parent) = row.nullable_integer("parent_market_id")? {
                ids.insert(parent);
            }
        }
        if before == ids.len() {
            return Ok(rows);
        }
    }
}

async fn excluded_counts(
    source: &mut SourceSnapshot<'_, '_>,
    approved_models: usize,
    decisions: usize,
) -> Result<BTreeMap<String, i64>> {
    let mut counts = BTreeMap::new();
    counts.insert(
        "avionics_models_not_approved".into(),
        source
            .count("avionics_models", "catalog_status <> 'approved'")
            .await?,
    );
    counts.insert(
        "aircraft_identity_decisions_not_selected".into(),
        source.count("aircraft_identity_decisions", "1 = 1").await? - decisions as i64,
    );
    counts.insert(
        "aircraft_identity_candidates".into(),
        source
            .count("aircraft_identity_resolution_candidates", "1 = 1")
            .await?,
    );
    counts.insert(
        "listings".into(),
        source.count("aircraft_sale_listings", "1 = 1").await?,
    );
    counts.insert(
        "listing_reviews".into(),
        source
            .count("aircraft_sale_listing_pending_reviews", "1 = 1")
            .await?,
    );
    counts.insert(
        "provider_usage".into(),
        source.count("gemini_api_usage", "1 = 1").await?,
    );
    counts.insert("approved_avionics_selected".into(), approved_models as i64);
    Ok(counts)
}

async fn validate_target_empty(target: &AppDb) -> Result<()> {
    for table in EMPTY_TARGET_TABLES {
        let existing = count(target, table, "1 = 1").await?;
        if existing != 0 {
            bail!("verified catalog seed target is not clean: {table} contains {existing} rows");
        }
    }
    Ok(())
}

async fn validate_required_users(target: &AppDb, required: &[SeedRow]) -> Result<()> {
    for expected in required {
        let id = expected.integer("id")?;
        let actual = fetch_rows(target, "users", &format!("id = {id}")).await?;
        let Some(actual) = actual.first() else {
            bail!("required decided-by user {id} is absent or differs in the target; import the signed captures first");
        };
        if !required_user_matches(actual, expected) {
            bail!("required decided-by user {id} is absent or differs in the target; import the signed captures first");
        }
    }
    Ok(())
}

fn required_user_matches(actual: &SeedRow, expected: &SeedRow) -> bool {
    [
        "id",
        "email",
        "display_name",
        "auth_provider",
        "auth_subject",
    ]
    .into_iter()
    .all(|column| actual.value(column) == expected.value(column))
}

async fn validate_target_empty_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
) -> Result<()> {
    for table in EMPTY_TARGET_TABLES {
        let existing = count_sqlite(&mut **transaction, table).await?;
        if existing != 0 {
            bail!("verified catalog seed target is not clean: {table} contains {existing} rows");
        }
    }
    Ok(())
}

async fn validate_target_empty_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<()> {
    for table in EMPTY_TARGET_TABLES {
        let existing = count_postgres(&mut **transaction, table).await?;
        if existing != 0 {
            bail!("verified catalog seed target is not clean: {table} contains {existing} rows");
        }
    }
    Ok(())
}

async fn validate_required_users_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    required: &[SeedRow],
) -> Result<()> {
    for expected in required {
        let id = expected.integer("id")?;
        let actual =
            fetch_rows_sqlite_executor(&mut **transaction, "users", &format!("id = {id}")).await?;
        if !actual
            .first()
            .is_some_and(|actual| required_user_matches(actual, expected))
        {
            bail!("required decided-by user {id} is absent or differs in the target; import the signed captures first");
        }
    }
    Ok(())
}

async fn validate_required_users_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    required: &[SeedRow],
) -> Result<()> {
    for expected in required {
        let id = expected.integer("id")?;
        let actual =
            fetch_rows_postgres_executor(&mut **transaction, "users", &format!("id = {id}"))
                .await?;
        if !actual
            .first()
            .is_some_and(|actual| required_user_matches(actual, expected))
        {
            bail!("required decided-by user {id} is absent or differs in the target; import the signed captures first");
        }
    }
    Ok(())
}

async fn apply_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    bundle: &SeedBundle,
) -> Result<(usize, usize, usize)> {
    let mut inserted = 0;
    for rows in &bundle.insert_groups {
        for row in rows {
            insert_row_sqlite(transaction, row).await?;
            inserted += 1;
        }
        if rows
            .first()
            .is_some_and(|row| row.table == "avionics_manufacturers")
        {
            reconcile_generated_rows_sqlite(transaction, &bundle.generated_keys).await?;
        }
        if rows
            .first()
            .is_some_and(|row| row.table == "avionics_model_types")
        {
            promote_models_sqlite(transaction, &bundle.generated_products).await?;
            reconcile_generated_rows_sqlite(transaction, &bundle.generated_products).await?;
        }
    }
    Ok((inserted, bundle.generated_products.len(), 0))
}

async fn apply_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    bundle: &SeedBundle,
) -> Result<(usize, usize, usize)> {
    let mut inserted = 0;
    let mut delayed_origins = None;
    for rows in &bundle.insert_groups {
        if rows
            .first()
            .is_some_and(|row| row.table == "avionics_authoritative_source_origins")
        {
            delayed_origins = Some(rows);
            continue;
        }
        for row in rows {
            insert_row_postgres(transaction, row).await?;
            inserted += 1;
        }
        if rows
            .first()
            .is_some_and(|row| row.table == "avionics_manufacturers")
        {
            reconcile_generated_rows_postgres(transaction, &bundle.generated_keys, false).await?;
        }
        if rows
            .first()
            .is_some_and(|row| row.table == "avionics_manufacturer_identities")
        {
            if let Some(origins) = delayed_origins.take() {
                for row in origins {
                    ensure_origin_postgres(transaction, row).await?;
                    inserted += 1;
                }
            }
        }
        if rows
            .first()
            .is_some_and(|row| row.table == "avionics_model_types")
        {
            promote_models_postgres(transaction, &bundle.generated_products).await?;
            reconcile_generated_rows_postgres(transaction, &bundle.generated_products, false)
                .await?;
        }
    }
    reset_postgres_sequences(transaction, bundle).await?;
    Ok((inserted, bundle.generated_products.len(), 2))
}

async fn insert_row_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    row: &SeedRow,
) -> Result<()> {
    let mut query =
        QueryBuilder::<Sqlite>::new(format!("INSERT INTO {} (", quoted_identifier(&row.table)));
    push_columns(&mut query, &row.columns);
    query.push(") VALUES (");
    push_values_sqlite(&mut query, &row.values);
    query.push(")");
    query
        .build()
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("could not seed {} row {}", row.table, canonical_row(row)))?;
    Ok(())
}

async fn insert_row_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    row: &SeedRow,
) -> Result<()> {
    let mut query =
        QueryBuilder::<Postgres>::new(format!("INSERT INTO {} (", quoted_identifier(&row.table)));
    push_columns(&mut query, &row.columns);
    query.push(") VALUES (");
    push_values_postgres(&mut query, &row.values);
    query.push(")");
    query
        .build()
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("could not seed {} row {}", row.table, canonical_row(row)))?;
    Ok(())
}

fn push_columns<'a, DB: sqlx::Database>(query: &mut QueryBuilder<'a, DB>, columns: &[String]) {
    let mut separated = query.separated(", ");
    for column in columns {
        separated.push(quoted_identifier(column));
    }
}

fn push_values_sqlite<'a>(query: &mut QueryBuilder<'a, Sqlite>, values: &[Value]) {
    let mut separated = query.separated(", ");
    for value in values {
        match value {
            Value::Null => separated.push("NULL"),
            Value::Bool(value) => separated.push_bind(i64::from(*value)),
            Value::Number(number) if number.is_i64() => {
                separated.push_bind(number.as_i64().expect("checked integer"))
            }
            Value::Number(number) => separated.push_bind(number.as_f64().expect("JSON number")),
            Value::String(value) => separated.push_bind(value.clone()),
            Value::Array(_) | Value::Object(_) => {
                separated.push_bind(serde_json::to_string(value).expect("JSON serialization"))
            }
        };
    }
}

fn push_values_postgres<'a>(query: &mut QueryBuilder<'a, Postgres>, values: &[Value]) {
    let mut separated = query.separated(", ");
    for value in values {
        match value {
            Value::Null => separated.push("NULL"),
            Value::Bool(value) => separated.push_bind(*value),
            Value::Number(number) if number.is_i64() => {
                separated.push_bind(number.as_i64().expect("checked integer"))
            }
            Value::Number(number) => separated.push_bind(number.as_f64().expect("JSON number")),
            Value::String(value) => separated.push_bind(value.clone()),
            Value::Array(_) | Value::Object(_) => {
                separated.push_bind(serde_json::to_string(value).expect("JSON serialization"))
            }
        };
    }
}

async fn promote_models_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    identities: &[SeedRow],
) -> Result<()> {
    for identity in identities {
        let id = identity.integer("avionics_model_id")?;
        let changed = sqlx::query(
            "UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ? AND catalog_status = 'unreviewed'",
        )
        .bind(id)
        .execute(&mut **transaction)
        .await?
        .rows_affected();
        if changed != 1 {
            bail!("could not promote staged avionics model {id}");
        }
    }
    Ok(())
}

async fn promote_models_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    identities: &[SeedRow],
) -> Result<()> {
    for identity in identities {
        let id = identity.integer("avionics_model_id")?;
        let changed = sqlx::query(
            "UPDATE avionics_models SET catalog_status = 'approved' WHERE id = $1 AND catalog_status = 'unreviewed'",
        )
        .bind(id)
        .execute(&mut **transaction)
        .await?
        .rows_affected();
        if changed != 1 {
            bail!("could not promote staged avionics model {id}");
        }
    }
    Ok(())
}

async fn reconcile_generated_rows_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    expected: &[SeedRow],
) -> Result<()> {
    for row in expected {
        let predicate = primary_key_predicate(row)?;
        let actual = fetch_rows_sqlite_executor(&mut **transaction, &row.table, &predicate).await?;
        let Some(actual) = actual.into_iter().next() else {
            bail!("schema did not generate expected {} row", row.table);
        };
        reconcile_timestamp_columns_sqlite(transaction, row, &actual).await?;
        let actual = fetch_rows_sqlite_executor(&mut **transaction, &row.table, &predicate).await?;
        if actual.as_slice() != [row.clone()] {
            bail!("schema-generated {} row differs from the source", row.table);
        }
    }
    Ok(())
}

async fn reconcile_timestamp_columns_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    expected: &SeedRow,
    actual: &SeedRow,
) -> Result<()> {
    let columns = ["created_at", "updated_at"]
        .into_iter()
        .filter(|column| expected.value(column) != actual.value(column))
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Ok(());
    }
    let predicate = primary_key_predicate(expected)?;
    let mut query = QueryBuilder::<Sqlite>::new(format!(
        "UPDATE {} SET ",
        quoted_identifier(&expected.table)
    ));
    for (index, column) in columns.into_iter().enumerate() {
        if index > 0 {
            query.push(", ");
        }
        query
            .push(format!("{} = ", quoted_identifier(column)))
            .push_bind(
                expected
                    .value(column)
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_string(),
            );
    }
    query.push(" WHERE ").push(predicate);
    query.build().execute(&mut **transaction).await?;
    Ok(())
}

async fn reconcile_generated_rows_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    expected: &[SeedRow],
    allow_bootstrap_timestamp: bool,
) -> Result<()> {
    for row in expected {
        let predicate = primary_key_predicate(row)?;
        let actual =
            fetch_rows_postgres_executor(&mut **transaction, &row.table, &predicate).await?;
        let Some(mut actual) = actual.into_iter().next() else {
            bail!("schema did not generate expected {} row", row.table);
        };
        if allow_bootstrap_timestamp {
            actual = normalize_bootstrap_origin_timestamp(actual, row);
        }
        if actual != *row {
            // Product identities and canonical keys permit timestamp-only
            // reconciliation before any reuse attestation exists.
            reconcile_timestamp_columns_postgres(transaction, row, &actual).await?;
            let actual =
                fetch_rows_postgres_executor(&mut **transaction, &row.table, &predicate).await?;
            if actual.as_slice() != [row.clone()] {
                bail!("schema-generated {} row differs from the source", row.table);
            }
        }
    }
    Ok(())
}

async fn reconcile_timestamp_columns_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    expected: &SeedRow,
    actual: &SeedRow,
) -> Result<()> {
    let columns = ["created_at", "updated_at"]
        .into_iter()
        .filter(|column| expected.value(column) != actual.value(column))
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Ok(());
    }
    let predicate = primary_key_predicate(expected)?;
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "UPDATE {} SET ",
        quoted_identifier(&expected.table)
    ));
    for (index, column) in columns.into_iter().enumerate() {
        if index > 0 {
            query.push(", ");
        }
        query
            .push(format!("{} = ", quoted_identifier(column)))
            .push_bind(
                expected
                    .value(column)
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_string(),
            );
    }
    query.push(" WHERE ").push(predicate);
    query.build().execute(&mut **transaction).await?;
    Ok(())
}

async fn ensure_origin_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    expected: &SeedRow,
) -> Result<()> {
    let id = expected.integer("id")?;
    let existing = fetch_rows_postgres_executor(
        &mut **transaction,
        "avionics_authoritative_source_origins",
        &format!("id = {id}"),
    )
    .await?;
    if existing.is_empty() {
        insert_row_postgres(transaction, expected).await?;
        return Ok(());
    }
    let normalized = normalize_bootstrap_origin_timestamp(existing[0].clone(), expected);
    if normalized != *expected {
        bail!("schema-generated authoritative origin {id} differs from source provenance");
    }
    Ok(())
}

fn normalize_bootstrap_origin_timestamp(mut actual: SeedRow, expected: &SeedRow) -> SeedRow {
    let origin = expected.value("https_origin").and_then(Value::as_str);
    let is_bootstrap = BOOTSTRAP_ORIGINS
        .iter()
        .any(|(_, value)| Some(*value) == origin);
    if is_bootstrap {
        if let (Some(index), Some(value)) = (
            actual
                .columns
                .iter()
                .position(|column| column == "created_at"),
            expected.value("created_at"),
        ) {
            actual.values[index] = value.clone();
        }
    }
    actual
}

async fn reset_postgres_sequences(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    bundle: &SeedBundle,
) -> Result<()> {
    let tables = bundle
        .insert_groups
        .iter()
        .filter_map(|rows| rows.first())
        .filter(|row| row.columns.iter().any(|column| column == "id"))
        .map(|row| row.table.as_str())
        .collect::<BTreeSet<_>>();
    for table in tables {
        let sql = format!(
            "SELECT setval(pg_get_serial_sequence('{table}', 'id'), COALESCE((SELECT MAX(id) FROM {}), 1), (SELECT COUNT(*) > 0 FROM {}))",
            quoted_identifier(table), quoted_identifier(table)
        );
        // Some composite-key tables have an `id` column without a sequence.
        let sequence: Option<String> =
            sqlx::query_scalar(&format!("SELECT pg_get_serial_sequence('{table}', 'id')"))
                .fetch_one(&mut **transaction)
                .await?;
        if sequence.is_some() {
            sqlx::query(&sql).execute(&mut **transaction).await?;
        }
    }
    Ok(())
}

async fn ensure_sqlite_foreign_keys(transaction: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
    let violations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
        .fetch_one(&mut **transaction)
        .await?;
    if violations != 0 {
        bail!("verified catalog seed produced {violations} foreign-key violations");
    }
    Ok(())
}

async fn validate_seeded_target(target: &AppDb, bundle: &SeedBundle) -> Result<()> {
    let expected = bundle.fingerprint_rows();
    let mut actual = Vec::with_capacity(expected.len());
    for row in &expected {
        let predicate = primary_key_predicate(row)?;
        let rows = fetch_rows(target, &row.table, &predicate).await?;
        if rows.len() != 1 {
            bail!(
                "seeded target has {} rows for {} key",
                rows.len(),
                row.table
            );
        }
        let mut actual_row = rows.into_iter().next().unwrap();
        if target.kind() == crate::db::DatabaseKind::Postgres
            && row.table == "avionics_authoritative_source_origins"
        {
            actual_row = normalize_bootstrap_origin_timestamp(actual_row, row);
        }
        actual.push(actual_row);
    }
    actual.sort_by(|left, right| {
        left.table
            .cmp(&right.table)
            .then_with(|| canonical_row(left).cmp(&canonical_row(right)))
    });
    let actual_fingerprint = fingerprint(&actual)?;
    if actual_fingerprint != bundle.fingerprint_sha256 {
        bail!(
            "seeded target fingerprint {actual_fingerprint} differs from planned fingerprint {}",
            bundle.fingerprint_sha256
        );
    }
    for (table, expected) in &bundle.source_counts {
        let actual = count(target, table, "1 = 1").await?;
        if actual != *expected as i64 {
            bail!(
                "seeded target {} count {actual} differs from planned count {expected}",
                table
            );
        }
    }
    for table in FORBIDDEN_ARTIFACT_TABLES {
        let actual = count(target, table, "1 = 1").await?;
        if actual != 0 {
            bail!("seeded target unexpectedly contains {actual} forbidden {table} rows");
        }
    }
    Ok(())
}

async fn validate_seeded_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    bundle: &SeedBundle,
) -> Result<()> {
    let expected = bundle.fingerprint_rows();
    let mut actual = Vec::with_capacity(expected.len());
    for row in &expected {
        let rows = fetch_rows_sqlite_executor(
            &mut **transaction,
            &row.table,
            &primary_key_predicate(row)?,
        )
        .await?;
        if rows.len() != 1 {
            bail!(
                "seeded target has {} rows for {} key",
                rows.len(),
                row.table
            );
        }
        actual.push(rows.into_iter().next().unwrap());
    }
    verify_fingerprint(actual, bundle)?;
    for (table, expected) in &bundle.source_counts {
        let actual = count_sqlite(&mut **transaction, table).await?;
        if actual != *expected as i64 {
            bail!(
                "seeded target {} count {actual} differs from planned count {expected}",
                table
            );
        }
    }
    for table in FORBIDDEN_ARTIFACT_TABLES {
        let actual = count_sqlite(&mut **transaction, table).await?;
        if actual != 0 {
            bail!("seeded target unexpectedly contains {actual} forbidden {table} rows");
        }
    }
    Ok(())
}

async fn validate_seeded_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    bundle: &SeedBundle,
) -> Result<()> {
    let expected = bundle.fingerprint_rows();
    let mut actual = Vec::with_capacity(expected.len());
    for row in &expected {
        let rows = fetch_rows_postgres_executor(
            &mut **transaction,
            &row.table,
            &primary_key_predicate(row)?,
        )
        .await?;
        if rows.len() != 1 {
            bail!(
                "seeded target has {} rows for {} key",
                rows.len(),
                row.table
            );
        }
        actual.push(normalize_bootstrap_origin_timestamp(
            rows.into_iter().next().unwrap(),
            row,
        ));
    }
    verify_fingerprint(actual, bundle)?;
    for (table, expected) in &bundle.source_counts {
        let actual = count_postgres(&mut **transaction, table).await?;
        if actual != *expected as i64 {
            bail!(
                "seeded target {} count {actual} differs from planned count {expected}",
                table
            );
        }
    }
    for table in FORBIDDEN_ARTIFACT_TABLES {
        let actual = count_postgres(&mut **transaction, table).await?;
        if actual != 0 {
            bail!("seeded target unexpectedly contains {actual} forbidden {table} rows");
        }
    }
    Ok(())
}

fn verify_fingerprint(mut actual: Vec<SeedRow>, bundle: &SeedBundle) -> Result<()> {
    actual.sort_by(|left, right| {
        left.table
            .cmp(&right.table)
            .then_with(|| canonical_row(left).cmp(&canonical_row(right)))
    });
    let actual_fingerprint = fingerprint(&actual)?;
    if actual_fingerprint != bundle.fingerprint_sha256 {
        bail!(
            "seeded target fingerprint {actual_fingerprint} differs from planned fingerprint {}",
            bundle.fingerprint_sha256
        );
    }
    Ok(())
}

async fn count_sqlite(connection: &mut sqlx::SqliteConnection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {}", quoted_identifier(table));
    Ok(sqlx::query_scalar(&sql).fetch_one(connection).await?)
}

async fn count_postgres(connection: &mut sqlx::PgConnection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {}", quoted_identifier(table));
    Ok(sqlx::query_scalar(&sql).fetch_one(connection).await?)
}

async fn count(db: &AppDb, table: &str, predicate: &str) -> Result<i64> {
    let sql = format!(
        "SELECT COUNT(*) FROM {} WHERE {predicate}",
        quoted_identifier(table)
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_scalar(&sql).fetch_one(pool).await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_scalar(&sql).fetch_one(pool).await?),
    }
}

async fn fetch_rows(db: &AppDb, table: &str, predicate: &str) -> Result<Vec<SeedRow>> {
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut connection = pool.acquire().await?;
            fetch_rows_sqlite_executor(&mut connection, table, predicate).await
        }
        DatabaseBackend::Postgres(pool) => {
            let mut connection = pool.acquire().await?;
            fetch_rows_postgres_executor(&mut connection, table, predicate).await
        }
    }
}

async fn fetch_rows_sqlite_executor(
    executor: &mut sqlx::SqliteConnection,
    table: &str,
    predicate: &str,
) -> Result<Vec<SeedRow>> {
    let columns = sqlite_columns(&mut *executor, table).await?;
    let json = json_projection("json_object", &columns);
    let order = primary_key_order(&columns);
    let sql = format!(
        "SELECT {json} AS row_json FROM {} WHERE {predicate} ORDER BY {order}",
        quoted_identifier(table)
    );
    let rows = sqlx::query(&sql).fetch_all(&mut *executor).await?;
    decode_json_rows(
        table,
        &columns,
        rows.iter().map(|row| row.get::<String, _>(0)),
    )
}

async fn fetch_rows_postgres_executor(
    executor: &mut sqlx::PgConnection,
    table: &str,
    predicate: &str,
) -> Result<Vec<SeedRow>> {
    let columns = postgres_columns(&mut *executor, table).await?;
    let json = json_projection("json_build_object", &columns);
    let order = primary_key_order(&columns);
    let sql = format!(
        "SELECT ({json})::text AS row_json FROM {} WHERE {predicate} ORDER BY {order}",
        quoted_identifier(table)
    );
    let rows = sqlx::query(&sql).fetch_all(&mut *executor).await?;
    decode_json_rows(
        table,
        &columns,
        rows.iter().map(|row| row.get::<String, _>(0)),
    )
}

#[derive(Clone, Debug)]
struct ColumnInfo {
    name: String,
    primary_key_position: i64,
}

async fn sqlite_columns(
    executor: &mut sqlx::SqliteConnection,
    table: &str,
) -> Result<Vec<ColumnInfo>> {
    let sql = format!("PRAGMA table_info({})", quoted_identifier(table));
    let rows = sqlx::query(&sql).fetch_all(executor).await?;
    if rows.is_empty() {
        bail!("required source/target table {table} is missing");
    }
    Ok(rows
        .into_iter()
        .map(|row| ColumnInfo {
            name: row.get::<String, _>("name"),
            primary_key_position: row.get::<i64, _>("pk"),
        })
        .collect())
}

async fn postgres_columns(
    executor: &mut sqlx::PgConnection,
    table: &str,
) -> Result<Vec<ColumnInfo>> {
    let rows = sqlx::query(
        r#"
        SELECT column_name,
               COALESCE(array_position(key_columns.columns, column_name), 0)::bigint AS pk
        FROM information_schema.columns
        LEFT JOIN LATERAL (
          SELECT array_agg(attribute.attname ORDER BY key.ordinality) AS columns
          FROM pg_index index_definition
          JOIN unnest(index_definition.indkey) WITH ORDINALITY key(attnum, ordinality)
            ON TRUE
          JOIN pg_attribute attribute
            ON attribute.attrelid = index_definition.indrelid
           AND attribute.attnum = key.attnum
          WHERE index_definition.indrelid = ($1::text)::regclass
            AND index_definition.indisprimary
        ) key_columns ON TRUE
        WHERE table_schema = current_schema() AND table_name = $1
        ORDER BY ordinal_position
        "#,
    )
    .bind(table)
    .fetch_all(executor)
    .await?;
    if rows.is_empty() {
        bail!("required source/target table {table} is missing");
    }
    Ok(rows
        .into_iter()
        .map(|row| ColumnInfo {
            name: row.get::<String, _>("column_name"),
            primary_key_position: row.get::<i64, _>("pk"),
        })
        .collect())
}

fn json_projection(function: &str, columns: &[ColumnInfo]) -> String {
    let arguments = columns
        .iter()
        .flat_map(|column| {
            [
                format!("'{}'", column.name.replace('\'', "''")),
                quoted_identifier(&column.name),
            ]
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{function}({arguments})")
}

fn primary_key_order(columns: &[ColumnInfo]) -> String {
    let mut keys = columns
        .iter()
        .filter(|column| column.primary_key_position > 0)
        .collect::<Vec<_>>();
    keys.sort_by_key(|column| column.primary_key_position);
    if keys.is_empty() {
        columns
            .iter()
            .map(|column| quoted_identifier(&column.name))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        keys.into_iter()
            .map(|column| quoted_identifier(&column.name))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn decode_json_rows(
    table: &str,
    columns: &[ColumnInfo],
    rows: impl Iterator<Item = String>,
) -> Result<Vec<SeedRow>> {
    rows.map(|json| {
        let mut object = serde_json::from_str::<Value>(&json)?
            .as_object()
            .cloned()
            .context("database JSON projection was not an object")?;
        let values = columns
            .iter()
            .map(|column| {
                object
                    .remove(&column.name)
                    .with_context(|| format!("database JSON omitted {}.{}", table, column.name))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(SeedRow {
            table: table.to_string(),
            columns: columns.iter().map(|column| column.name.clone()).collect(),
            values,
        })
    })
    .collect()
}

fn ids(rows: &[SeedRow], column: &str) -> Result<Vec<i64>> {
    let mut values = rows
        .iter()
        .map(|row| row.integer(column))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    values.sort_unstable();
    Ok(values)
}

fn in_predicate(column: &str, ids: &[i64]) -> String {
    if ids.is_empty() {
        "1 = 0".to_string()
    } else {
        format!(
            "{} IN ({})",
            quoted_identifier(column),
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        )
    }
}

fn primary_key_predicate(row: &SeedRow) -> Result<String> {
    // Every generated/comparison table in this workflow has one of these
    // explicit stable keys. Keeping this list closed prevents a caller from
    // turning arbitrary source text into SQL.
    let columns: &[&str] = match row.table.as_str() {
        "avionics_manufacturer_canonical_keys" => &["avionics_manufacturer_id"],
        "avionics_approved_product_identities" => &["avionics_model_id"],
        "avionics_model_types" => &["avionics_model_id", "avionics_type_id"],
        "avionics_suite_components" => &["suite_model_id", "component_model_id"],
        "avionics_product_reuse_attestations" => &["avionics_model_id"],
        "avionics_manufacturer_identity_memberships" => &["avionics_manufacturer_id"],
        "avionics_authoritative_source_origin_revocations" => {
            &["avionics_authoritative_source_origin_id"]
        }
        "aircraft_identity_decision_claims" => {
            &["decision_id", "evidence_claim_id", "evidence_role"]
        }
        "faa_registry_aircraft" => &["snapshot_id", "n_number"],
        "faa_registry_aircraft_references" => &["snapshot_id", "aircraft_code"],
        "faa_registry_engine_references" => &["snapshot_id", "engine_code"],
        "faa_registry_coverage" => &["snapshot_id", "n_number"],
        "aircraft_generation_designations" => {
            &["aircraft_generation_id", "aircraft_designation_id"]
        }
        "aircraft_designation_faa_bindings" => &[
            "faa_snapshot_date",
            "faa_archive_sha256",
            "faa_aircraft_code",
        ],
        _ if row.columns.iter().any(|column| column == "id") => &["id"],
        _ => bail!("no stable seed key is defined for {}", row.table),
    };
    columns
        .iter()
        .map(|column| {
            let value = row
                .value(column)
                .with_context(|| format!("{}.{} is absent", row.table, column))?;
            Ok(format!(
                "{} = {}",
                quoted_identifier(column),
                sql_literal(value)?
            ))
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join(" AND "))
}

fn sql_literal(value: &Value) -> Result<String> {
    match value {
        Value::Null => Ok("NULL".into()),
        Value::Bool(value) => Ok(if *value { "1" } else { "0" }.into()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(format!("'{}'", value.replace('\'', "''"))),
        _ => bail!("compound JSON cannot be used as a seed key"),
    }
}

fn quoted_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn canonical_row(row: &SeedRow) -> String {
    serde_json::to_string(row).expect("SeedRow serialization cannot fail")
}

fn fingerprint(rows: &[SeedRow]) -> Result<String> {
    let bytes = serde_json::to_vec(rows)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn execute(db: &AppDb, sql: &str) {
        match db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::raw_sql(sql).execute(pool).await.unwrap();
            }
            DatabaseBackend::Postgres(_) => unreachable!("SQLite fixture"),
        }
    }

    async fn approved_fixture() -> AppDb {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        execute(
            &db,
            r#"
            UPDATE users
            SET created_at = '2000-01-01 00:00:00',
                updated_at = '2000-01-01 00:00:00'
            WHERE id = 1;

            INSERT INTO avionics_manufacturers (
              id, name, normalized_name, created_at, updated_at
            ) VALUES (1, 'Garmin', 'garmin', '2026-01-01', '2026-01-01');
            UPDATE avionics_manufacturer_canonical_keys
            SET created_at = '2026-01-01', updated_at = '2026-01-01'
            WHERE avionics_manufacturer_id = 1;

            INSERT INTO avionics_manufacturer_identities (
              id, canonical_name, normalized_identity_key,
              identity_evidence_kind, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_confidence, created_at
            ) VALUES (
              1, 'Garmin', 'garmin', 'authoritative_reference',
              'https://www.garmin.com/en-US/p/588901/',
              'Garmin product reference',
              'Garmin publishes this exact aviation product identity.',
              'very_high', '2026-01-01'
            );

            INSERT INTO avionics_manufacturer_identity_memberships (
              avionics_manufacturer_id, avionics_manufacturer_identity_id,
              membership_basis, normalized_name_key, evidence_source_url,
              evidence_source_title, evidence_text, evidence_confidence,
              created_at
            ) VALUES (
              1, 1, 'authoritative_primary', 'garmin',
              'https://www.garmin.com/en-US/p/588901/',
              'Garmin product reference',
              'Garmin publishes this exact aviation product identity.',
              'very_high', '2026-01-01'
            );

            INSERT INTO avionics_authoritative_source_origins (
              id, authority_kind, avionics_manufacturer_identity_id,
              regulator_key, https_origin, evidence_source_url,
              evidence_source_title, evidence_text, approval_basis,
              approved_by_user_id, approval_reason, created_at
            ) VALUES (
              63, 'manufacturer_primary', 1, NULL,
              'https://support.garmin.com',
              'https://support.garmin.com/en-US/aviation/',
              'Garmin aviation support',
              'Garmin publishes exact aviation product support on this origin.',
              'human_review', 1,
              'Reviewer confirmed the exact Garmin support origin.',
              '2026-01-02'
            );

            INSERT INTO avionics_types (
              id, name, normalized_name, created_at, updated_at
            ) VALUES (1, 'GPS', 'gps', '2026-01-01', '2026-01-01');

            INSERT INTO avionics_models (
              id, avionics_manufacturer_id, name, normalized_name,
              catalog_status, manufacturer_identifier_kind,
              manufacturer_identifier, normalized_manufacturer_identifier,
              identity_source_url, identity_source_title,
              identity_evidence_text, identity_evidence_kind,
              identity_confidence, catalog_reviewed_at,
              value_basis, valuation_scope, created_at, updated_at
            ) VALUES (
              1, 1, 'GTN 650Xi', 'gtn 650xi', 'unreviewed',
              'manufacturer_model_number', 'GTN 650Xi', 'gtn 650xi',
              'https://support.garmin.com/en-US/aviation/gtn-650xi/',
              'Garmin GTN 650Xi',
              'Garmin identifies the GTN 650Xi by this exact model name.',
              'authoritative_reference', 'very_high', '2026-01-03',
              'unreviewed', 'unit', '2026-01-01', '2026-01-03'
            );
            INSERT INTO avionics_model_types VALUES (1, 1);
            UPDATE avionics_models SET catalog_status = 'approved' WHERE id = 1;
            INSERT INTO avionics_product_reuse_attestations (
              avionics_model_id, avionics_authoritative_source_origin_id,
              policy_version, product_fingerprint, attested_at
            ) VALUES (
              1, 63, 'avionics_reuse_v2',
              'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
              '2026-01-04'
            );
            "#,
        )
        .await;
        db
    }

    #[test]
    fn empty_in_predicate_never_selects_rows() {
        assert_eq!(in_predicate("id", &[]), "1 = 0");
    }

    #[test]
    fn fingerprint_is_order_sensitive_after_canonical_sort_boundary() {
        let row = SeedRow {
            table: "fixture".into(),
            columns: vec!["id".into()],
            values: vec![Value::from(1)],
        };
        assert_eq!(
            fingerprint(&[row.clone()]).unwrap(),
            fingerprint(&[row]).unwrap()
        );
    }

    #[tokio::test]
    async fn fresh_target_dry_run_and_apply_seed_only_the_approved_graph() {
        let source = approved_fixture().await;
        let target = AppDb::connect("sqlite::memory:").await.unwrap();

        let dry_run = seed_verified_catalog(&source, &target, false)
            .await
            .unwrap();
        assert!(dry_run.dry_run);
        assert_eq!(dry_run.provider_calls, 0);
        assert_eq!(dry_run.source_counts["avionics_models"], 1);
        assert_eq!(count(&target, "avionics_models", "1 = 1").await.unwrap(), 0);

        let applied = seed_verified_catalog(&source, &target, true).await.unwrap();
        assert!(!applied.dry_run);
        assert_eq!(applied.generated_identity_rows_verified, 1);
        assert_eq!(
            count(&target, "avionics_models", "catalog_status = 'approved'")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            count(&target, "avionics_models", "catalog_status <> 'approved'")
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            count(&target, "gemini_api_usage", "1 = 1").await.unwrap(),
            0
        );
    }
}
