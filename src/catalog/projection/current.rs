//! Read-only projection of verified catalog truth in a current schema.
//!
//! Evidence, observation, case, decision, and claim rows are fingerprinted
//! byte-for-byte as stored in the current schema.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Acquire, Connection, PgConnection, SqliteConnection};

use super::{canonical_row, ids, in_predicate, ProjectionRow, ROOT_TABLES};
use crate::db::{AppDb, DatabaseBackend};

const CURRENT_CATALOG_PROJECTION_DOMAIN: &[u8] = b"aircost:current-verified-catalog-projection\0";

const CURRENT_TABLES: &[&str] = &[
    "curation_evidence_sources",
    "curation_evidence_claims",
    "faa_registry_snapshots",
    "faa_registry_aircraft",
    "faa_registry_aircraft_references",
    "faa_registry_engine_references",
    "faa_registry_coverage",
    "aircraft_identity_observations",
    "aircraft_identity_resolution_cases",
    "aircraft_identity_decisions",
    "aircraft_identity_decision_claims",
    "aircraft_markets",
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
    "avionics_manufacturers",
    "avionics_manufacturer_canonical_keys",
    "avionics_manufacturer_identities",
    "avionics_manufacturer_identity_memberships",
    "avionics_authoritative_source_origins",
    "avionics_types",
    "avionics_models",
    "avionics_approved_product_identities",
    "avionics_model_types",
    "avionics_suite_components",
    "avionics_product_reuse_attestations",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct CurrentCatalogProjectionSummary {
    pub fingerprint_sha256: String,
    pub table_counts: BTreeMap<String, usize>,
    pub required_users: Vec<RequiredCatalogUser>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct RequiredCatalogUser {
    pub id: i64,
    pub email: String,
    pub display_name: String,
    pub auth_provider: String,
    pub auth_subject: String,
}

/// Opaque current-schema catalog snapshot. Consumers compare or report it; the
/// sibling seed writer consumes this boundary without coupling the writer to
/// current row selection or canonicalization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CurrentCatalogProjection {
    summary: CurrentCatalogProjectionSummary,
    rows: Vec<ProjectionRow>,
    canonical_rows: Vec<String>,
}

enum ProjectionReader<'connection> {
    Sqlite(&'connection mut SqliteConnection),
    Postgres(&'connection mut PgConnection),
}

#[derive(Serialize)]
struct FingerprintEnvelope<'rows> {
    table_counts: Vec<FingerprintTableCount<'rows>>,
    rows: &'rows [String],
}

#[derive(Serialize)]
struct FingerprintTableCount<'table> {
    table: &'table str,
    count: usize,
}

impl CurrentCatalogProjection {
    pub(crate) async fn load(source: &AppDb) -> Result<Self> {
        match source.backend() {
            DatabaseBackend::Sqlite(pool) => {
                let mut connection = pool.acquire().await?;
                let mut snapshot = connection.begin().await?;
                let projection = {
                    let mut reader = ProjectionReader::Sqlite(&mut snapshot);
                    load_current(&mut reader).await
                };
                snapshot.rollback().await?;
                projection
            }
            DatabaseBackend::Postgres(pool) => {
                let mut connection = pool.acquire().await?;
                let mut snapshot = connection
                    .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
                    .await?;
                let projection = {
                    let mut reader = ProjectionReader::Postgres(&mut snapshot);
                    load_current(&mut reader).await
                };
                snapshot.rollback().await?;
                projection
            }
        }
    }

    pub(crate) fn summary(&self) -> &CurrentCatalogProjectionSummary {
        &self.summary
    }

    pub(crate) fn fingerprint_sha256(&self) -> &str {
        &self.summary.fingerprint_sha256
    }

    pub(super) fn rows(&self) -> &[ProjectionRow] {
        &self.rows
    }

    pub(super) async fn load_sqlite_connection(connection: &mut SqliteConnection) -> Result<Self> {
        let mut reader = ProjectionReader::Sqlite(connection);
        load_current(&mut reader).await
    }

    pub(super) async fn load_postgres_connection(connection: &mut PgConnection) -> Result<Self> {
        let mut reader = ProjectionReader::Postgres(connection);
        load_current(&mut reader).await
    }

    pub(super) fn require_exact_match(&self, reloaded: Self, scope: &str) -> Result<()> {
        if reloaded != *self {
            bail!(
                "{scope} current catalog fingerprint {} differs from source fingerprint {}",
                reloaded.fingerprint_sha256(),
                self.fingerprint_sha256()
            );
        }
        Ok(())
    }

    pub(crate) async fn require_reloaded_match(&self, source: &AppDb) -> Result<()> {
        let reloaded = Self::load(source).await?;
        self.require_exact_match(reloaded, "reopened")
    }
}

async fn load_current(source: &mut ProjectionReader<'_>) -> Result<CurrentCatalogProjection> {
    let mut roots = selected_aircraft_roots(source).await?;
    let decision_ids = required_aircraft_decision_ids(&roots)?;
    let decisions = fetch(
        source,
        "aircraft_identity_decisions",
        &in_predicate("id", &decision_ids),
    )
    .await?;
    validate_aircraft_approval_decisions(&decision_ids, &decisions)?;
    let cases = fetch(
        source,
        "aircraft_identity_resolution_cases",
        &in_predicate("id", &ids(&decisions, "resolution_case_id")?),
    )
    .await?;
    let observations = fetch(
        source,
        "aircraft_identity_observations",
        &in_predicate("id", &ids(&cases, "observation_id")?),
    )
    .await?;
    let decision_claims = fetch(
        source,
        "aircraft_identity_decision_claims",
        &in_predicate("decision_id", &decision_ids),
    )
    .await?;
    let mut claim_ids = ids(&decision_claims, "evidence_claim_id")?
        .into_iter()
        .collect::<BTreeSet<_>>();
    for row in roots.values().flatten() {
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
    let claims = fetch(
        source,
        "curation_evidence_claims",
        &in_predicate("id", &claim_ids.into_iter().collect::<Vec<_>>()),
    )
    .await?;

    let snapshots = fetch(source, "faa_registry_snapshots", "1 = 1").await?;
    if snapshots.is_empty() {
        bail!("current catalog projection requires a current-domain FAA cache");
    }
    let faa_snapshot_ids = ids(&snapshots, "id")?;
    // A retained FAA snapshot authenticates one complete target set. Keep its
    // full target-scoped cache, including targets not used as representatives;
    // subsetting would make target_set_sha256 lie about the retained rows.
    let aircraft_references = fetch(
        source,
        "faa_registry_aircraft_references",
        &in_predicate("snapshot_id", &faa_snapshot_ids),
    )
    .await?;
    let engine_references = fetch(
        source,
        "faa_registry_engine_references",
        &in_predicate("snapshot_id", &faa_snapshot_ids),
    )
    .await?;
    let faa_aircraft = fetch(
        source,
        "faa_registry_aircraft",
        &in_predicate("snapshot_id", &faa_snapshot_ids),
    )
    .await?;
    let coverage = fetch(
        source,
        "faa_registry_coverage",
        &in_predicate("snapshot_id", &faa_snapshot_ids),
    )
    .await?;
    validate_faa_closure(
        &snapshots,
        &coverage,
        &faa_aircraft,
        &aircraft_references,
        &engine_references,
    )?;

    let selected_snapshot_ids = faa_snapshot_ids.iter().copied().collect::<BTreeSet<_>>();
    for row in roots
        .get("aircraft_designation_faa_bindings")
        .into_iter()
        .flatten()
        .chain(
            roots
                .get("aircraft_tcds_make_lineage_bindings")
                .into_iter()
                .flatten(),
        )
    {
        if !selected_snapshot_ids.contains(&row.integer("representative_faa_registry_snapshot_id")?)
        {
            bail!("verified aircraft catalog references a missing FAA snapshot");
        }
    }

    let approved_models = fetch(source, "avionics_models", "catalog_status = 'approved'").await?;
    if approved_models.is_empty() {
        bail!("current catalog has no approved avionics models");
    }
    let model_ids = ids(&approved_models, "id")?;
    let manufacturers = fetch(
        source,
        "avionics_manufacturers",
        &in_predicate("id", &ids(&approved_models, "avionics_manufacturer_id")?),
    )
    .await?;
    let manufacturer_ids = ids(&manufacturers, "id")?;
    let generated_keys = fetch(
        source,
        "avionics_manufacturer_canonical_keys",
        &in_predicate("avionics_manufacturer_id", &manufacturer_ids),
    )
    .await?;
    let generated_products = fetch(
        source,
        "avionics_approved_product_identities",
        &in_predicate("avionics_model_id", &model_ids),
    )
    .await?;
    if generated_keys.len() != manufacturers.len()
        || generated_products.len() != approved_models.len()
    {
        bail!("approved avionics closure lacks schema-generated identity rows");
    }
    let identity_ids = ids(&generated_products, "avionics_manufacturer_identity_id")?;
    let identities = fetch(
        source,
        "avionics_manufacturer_identities",
        &in_predicate("id", &identity_ids),
    )
    .await?;
    let memberships = fetch(
        source,
        "avionics_manufacturer_identity_memberships",
        &format!(
            "{} AND {}",
            in_predicate("avionics_manufacturer_id", &manufacturer_ids),
            in_predicate("avionics_manufacturer_identity_id", &identity_ids)
        ),
    )
    .await?;
    if memberships.len() != manufacturers.len() {
        bail!("approved avionics manufacturers lack exact identity memberships");
    }
    let merges = fetch(
        source,
        "avionics_manufacturer_identity_merges",
        &format!(
            "{} OR {}",
            in_predicate("merged_identity_id", &identity_ids),
            in_predicate("survivor_identity_id", &identity_ids)
        ),
    )
    .await?;
    if !merges.is_empty() {
        bail!("approved avionics closure depends on excluded manufacturer merge history");
    }
    let model_types = fetch(
        source,
        "avionics_model_types",
        &in_predicate("avionics_model_id", &model_ids),
    )
    .await?;
    let types = fetch(
        source,
        "avionics_types",
        &in_predicate("id", &ids(&model_types, "avionics_type_id")?),
    )
    .await?;
    let suite_components = fetch(
        source,
        "avionics_suite_components",
        &format!(
            "{} AND {}",
            in_predicate("suite_model_id", &model_ids),
            in_predicate("component_model_id", &model_ids)
        ),
    )
    .await?;
    let reuse = fetch(
        source,
        "avionics_product_reuse_attestations",
        &in_predicate("avionics_model_id", &model_ids),
    )
    .await?;
    let origins = fetch(
        source,
        "avionics_authoritative_source_origins",
        &selected_origin_predicate(
            &identity_ids,
            &ids(&reuse, "avionics_authoritative_source_origin_id")?,
        ),
    )
    .await?;
    let revocations = fetch(
        source,
        "avionics_authoritative_source_origin_revocations",
        &in_predicate(
            "avionics_authoritative_source_origin_id",
            &ids(&origins, "id")?,
        ),
    )
    .await?;
    reject_selected_origin_revocations(&revocations)?;

    validate_catalog_safe_aircraft_provenance(&observations, &cases, &decisions, &claims)?;

    let mut required_user_ids = decisions
        .iter()
        .filter_map(|row| row.nullable_integer("decided_by_user_id").transpose())
        .collect::<Result<BTreeSet<_>>>()?;
    for row in &origins {
        if let Some(id) = row.nullable_integer("approved_by_user_id")? {
            required_user_ids.insert(id);
        }
    }
    let required_user_rows = fetch(
        source,
        "users",
        &in_predicate("id", &required_user_ids.iter().copied().collect::<Vec<_>>()),
    )
    .await?;
    if required_user_rows.len() != required_user_ids.len() {
        bail!("verified catalog references a missing reviewer user");
    }
    let required_users = required_user_rows
        .iter()
        .map(|row| {
            Ok(RequiredCatalogUser {
                id: row.integer("id")?,
                email: row.string("email")?.to_string(),
                display_name: row.string("display_name")?.to_string(),
                auth_provider: row.string("auth_provider")?.to_string(),
                auth_subject: row.string("auth_subject")?.to_string(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let markets = selected_markets(source, &roots).await?;
    let mut evidence_source_ids = ids(&claims, "evidence_source_id")?
        .into_iter()
        .collect::<BTreeSet<_>>();
    evidence_source_ids.extend(ids(&snapshots, "evidence_source_id")?);
    let evidence_sources = fetch(
        source,
        "curation_evidence_sources",
        &in_predicate("id", &evidence_source_ids.into_iter().collect::<Vec<_>>()),
    )
    .await?;

    let mut groups = vec![
        evidence_sources,
        claims,
        snapshots,
        aircraft_references,
        engine_references,
        faa_aircraft,
        coverage,
        observations,
        cases,
        decisions,
        decision_claims,
        markets,
    ];
    for table in ROOT_TABLES {
        groups.push(roots.remove(*table).expect("root table was selected"));
    }
    groups.extend([
        manufacturers,
        generated_keys,
        identities,
        memberships,
        origins,
        types,
        approved_models,
        generated_products,
        model_types,
        suite_components,
        reuse,
    ]);
    assemble(groups, required_users)
}

fn assemble(
    groups: Vec<Vec<ProjectionRow>>,
    required_users: Vec<RequiredCatalogUser>,
) -> Result<CurrentCatalogProjection> {
    let mut table_counts = CURRENT_TABLES
        .iter()
        .map(|table| ((*table).to_string(), 0usize))
        .collect::<BTreeMap<_, _>>();
    let mut rows = groups.into_iter().flatten().collect::<Vec<_>>();
    for row in &mut rows {
        let count = table_counts
            .get_mut(&row.table)
            .with_context(|| format!("unversioned current projection table {}", row.table))?;
        *count += 1;
        if row.table == "avionics_authoritative_source_origins"
            && row.value("approval_basis").and_then(Value::as_str) == Some("curated_bootstrap")
        {
            omit_column(row, "created_at")?;
        } else if matches!(
            row.table.as_str(),
            "avionics_manufacturer_canonical_keys" | "avionics_approved_product_identities"
        ) {
            omit_column_if_present(row, "created_at")?;
            omit_column_if_present(row, "updated_at")?;
        } else if row.table == "aircraft_markets" {
            omit_column_if_present(row, "created_at")?;
        } else if row.table == "faa_registry_snapshots" {
            omit_column_if_present(row, "imported_at")?;
        } else if row.table == "curation_evidence_sources"
            && row.value("source_domain").and_then(Value::as_str) == Some("faa.gov")
            && row.value("source_tier").and_then(Value::as_str) == Some("regulator_primary")
        {
            omit_column_if_present(row, "retrieved_at")?;
            omit_column_if_present(row, "created_at")?;
        }
    }
    rows.sort_by_key(canonical_row);
    let canonical_rows = rows.iter().map(canonical_row).collect::<Vec<_>>();
    let fingerprint_table_counts = CURRENT_TABLES
        .iter()
        .map(|table| FingerprintTableCount {
            table,
            count: table_counts[*table],
        })
        .collect();
    let envelope = FingerprintEnvelope {
        table_counts: fingerprint_table_counts,
        rows: &canonical_rows,
    };
    let mut fingerprint = Sha256::new();
    fingerprint.update(CURRENT_CATALOG_PROJECTION_DOMAIN);
    fingerprint.update(serde_json::to_vec(&envelope)?);
    let fingerprint_sha256 = format!("{:x}", fingerprint.finalize());
    Ok(CurrentCatalogProjection {
        summary: CurrentCatalogProjectionSummary {
            fingerprint_sha256,
            table_counts,
            required_users,
        },
        rows,
        canonical_rows,
    })
}

fn omit_column(row: &mut ProjectionRow, column: &str) -> Result<()> {
    let index = row
        .columns
        .iter()
        .position(|candidate| candidate == column)
        .with_context(|| format!("{}.{} is absent", row.table, column))?;
    row.columns.remove(index);
    row.values.remove(index);
    Ok(())
}

fn omit_column_if_present(row: &mut ProjectionRow, column: &str) -> Result<()> {
    if row.columns.iter().any(|candidate| candidate == column) {
        omit_column(row, column)?;
    }
    Ok(())
}

fn selected_origin_predicate(identity_ids: &[i64], referenced_origin_ids: &[i64]) -> String {
    format!(
        "{} OR {}",
        in_predicate("id", referenced_origin_ids),
        in_predicate("avionics_manufacturer_identity_id", identity_ids)
    )
}

fn reject_selected_origin_revocations(rows: &[ProjectionRow]) -> Result<()> {
    if !rows.is_empty() {
        bail!("selected avionics closure contains a revoked authoritative origin");
    }
    Ok(())
}

fn validate_faa_closure(
    snapshots: &[ProjectionRow],
    coverage: &[ProjectionRow],
    aircraft: &[ProjectionRow],
    aircraft_references: &[ProjectionRow],
    engine_references: &[ProjectionRow],
) -> Result<()> {
    let snapshot_ids = ids(snapshots, "id")?.into_iter().collect::<BTreeSet<_>>();
    let covered_snapshots = coverage
        .iter()
        .map(|row| row.integer("snapshot_id"))
        .collect::<Result<BTreeSet<_>>>()?;
    if coverage.is_empty() || covered_snapshots != snapshot_ids {
        bail!("each current FAA snapshot must retain a nonempty coverage set");
    }
    let matched = coverage
        .iter()
        .filter(|row| row.value("lookup_status").and_then(Value::as_str) == Some("matched"))
        .map(|row| {
            Ok((
                row.integer("snapshot_id")?,
                row.string("n_number")?.to_string(),
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let retained_aircraft = aircraft
        .iter()
        .map(|row| {
            Ok((
                row.integer("snapshot_id")?,
                row.string("n_number")?.to_string(),
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if matched != retained_aircraft {
        bail!("FAA matched coverage and retained aircraft N-numbers differ");
    }
    let aircraft_codes = aircraft
        .iter()
        .map(|row| {
            Ok((
                row.integer("snapshot_id")?,
                row.string("aircraft_code")?.to_string(),
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let reference_codes = aircraft_references
        .iter()
        .map(|row| {
            Ok((
                row.integer("snapshot_id")?,
                row.string("aircraft_code")?.to_string(),
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if aircraft_codes != reference_codes {
        bail!("FAA retained aircraft and ACFTREF code sets differ");
    }
    let aircraft_engine_codes = aircraft
        .iter()
        .filter_map(|row| {
            row.value("engine_code")
                .filter(|value| !value.is_null())
                .map(|value| {
                    Ok((
                        row.integer("snapshot_id")?,
                        value
                            .as_str()
                            .context("faa_registry_aircraft.engine_code is not text")?
                            .to_string(),
                    ))
                })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let reference_engine_codes = engine_references
        .iter()
        .map(|row| {
            Ok((
                row.integer("snapshot_id")?,
                row.string("engine_code")?.to_string(),
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if aircraft_engine_codes != reference_engine_codes {
        bail!("FAA retained aircraft and ENGINE code sets differ");
    }
    Ok(())
}

fn validate_catalog_safe_aircraft_provenance(
    observations: &[ProjectionRow],
    cases: &[ProjectionRow],
    decisions: &[ProjectionRow],
    claims: &[ProjectionRow],
) -> Result<()> {
    const RAW_OBSERVATION_COLUMNS: &[&str] = &[
        "aircraft_sale_listing_id",
        "source_url",
        "observed_make",
        "observed_family",
        "observed_designation",
        "observed_generation",
        "observed_package",
        "model_year",
        "serial_number",
        "registration_number",
        "market_code",
        "legacy_hint_json",
    ];
    const OBSERVATION_RECEIPT_PREFIX: &str = "catalog-provenance-source-observation-sha256:";
    for row in observations {
        if RAW_OBSERVATION_COLUMNS
            .iter()
            .any(|column| row.value(column).is_some_and(|value| !value.is_null()))
        {
            bail!("current catalog contains a listing-derived aircraft observation");
        }
        let source_digest = row
            .string("exact_source_evidence")?
            .strip_prefix(OBSERVATION_RECEIPT_PREFIX)
            .context("catalog aircraft observation lacks its bounded source digest receipt")?;
        require_lower_sha256(source_digest, "catalog observation source digest")?;
        require_lower_sha256(
            row.string("observation_sha256")?,
            "catalog observation digest",
        )?;
    }
    for row in cases {
        if row.string("case_status")? != "resolved" {
            bail!("current catalog contains an unresolved aircraft identity case");
        }
        require_lower_sha256(row.string("job_fingerprint")?, "catalog case fingerprint")?;
        let revision = row
            .string("catalog_revision")?
            .strip_prefix("sha256:")
            .context("catalog case revision is not a projected SHA-256")?;
        require_lower_sha256(revision, "catalog revision")?;
    }
    for row in decisions {
        if row.string("decision_status")? != "approved"
            || row.value("deterministic_validation_passed") != Some(&Value::Bool(true))
        {
            bail!("current catalog contains a non-approved aircraft identity decision");
        }
        let payload: Value = serde_json::from_str(row.string("decision_payload_json")?)
            .context("catalog decision payload is not JSON")?;
        if payload.get("projection_domain").and_then(Value::as_str)
            != Some("aircost:catalog-decision-projection:v2")
        {
            bail!("catalog decision payload is not the current bounded projection");
        }
        let validation: Value = serde_json::from_str(row.string("deterministic_validation_json")?)
            .context("catalog decision validation is not JSON")?;
        if validation.get("projection_domain").and_then(Value::as_str)
            != Some("aircost:catalog-decision-validation-projection:v1")
            || validation.get("passed").and_then(Value::as_bool) != Some(true)
        {
            bail!("catalog decision validation is not the current bounded projection");
        }
    }
    if claims
        .iter()
        .any(|row| row.value("validation_status").and_then(Value::as_str) != Some("validated"))
    {
        bail!("current catalog contains a non-validated evidence claim");
    }
    Ok(())
}

fn require_lower_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || value != value.to_ascii_lowercase()
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("{label} is not one lowercase SHA-256 digest");
    }
    Ok(())
}

async fn fetch(
    source: &mut ProjectionReader<'_>,
    table: &str,
    predicate: &str,
) -> Result<Vec<ProjectionRow>> {
    match source {
        ProjectionReader::Sqlite(connection) => {
            super::fetch_rows(&mut **connection, table, predicate).await
        }
        ProjectionReader::Postgres(connection) => {
            fetch_rows_postgres(&mut **connection, table, predicate).await
        }
    }
}

async fn selected_aircraft_roots(
    source: &mut ProjectionReader<'_>,
) -> Result<BTreeMap<String, Vec<ProjectionRow>>> {
    let mut roots = BTreeMap::new();
    let makes = fetch(source, "aircraft_makes", "1 = 1").await?;
    let make_ids = ids(&makes, "id")?;
    roots.insert("aircraft_makes".into(), makes);
    let families = fetch(
        source,
        "aircraft_model_families",
        &in_predicate("aircraft_make_id", &make_ids),
    )
    .await?;
    let family_ids = ids(&families, "id")?;
    roots.insert("aircraft_model_families".into(), families);
    let designations = fetch(
        source,
        "aircraft_designations",
        &in_predicate("aircraft_model_family_id", &family_ids),
    )
    .await?;
    let designation_ids = ids(&designations, "id")?;
    roots.insert("aircraft_designations".into(), designations);
    for (table, column, selected) in [
        ("aircraft_make_aliases", "aircraft_make_id", &make_ids),
        (
            "aircraft_family_aliases",
            "aircraft_model_family_id",
            &family_ids,
        ),
        (
            "aircraft_designation_aliases",
            "aircraft_designation_id",
            &designation_ids,
        ),
        (
            "aircraft_designation_identifiers",
            "aircraft_designation_id",
            &designation_ids,
        ),
        (
            "aircraft_serial_number_schemes",
            "aircraft_make_id",
            &make_ids,
        ),
    ] {
        roots.insert(
            table.into(),
            fetch(source, table, &in_predicate(column, selected)).await?,
        );
    }
    let generations = fetch(
        source,
        "aircraft_generations",
        &in_predicate("aircraft_model_family_id", &family_ids),
    )
    .await?;
    let generation_ids = ids(&generations, "id")?;
    roots.insert("aircraft_generations".into(), generations);
    roots.insert(
        "aircraft_generation_designations".into(),
        fetch(
            source,
            "aircraft_generation_designations",
            &format!(
                "{} AND {}",
                in_predicate("aircraft_generation_id", &generation_ids),
                in_predicate("aircraft_designation_id", &designation_ids)
            ),
        )
        .await?,
    );
    let packages = fetch(
        source,
        "aircraft_factory_packages",
        &in_predicate("aircraft_model_family_id", &family_ids),
    )
    .await?;
    let package_ids = ids(&packages, "id")?;
    roots.insert("aircraft_factory_packages".into(), packages);
    roots.insert(
        "aircraft_package_applicability".into(),
        fetch(
            source,
            "aircraft_package_applicability",
            &format!(
                "{} AND (aircraft_designation_id IS NULL OR {}) AND (aircraft_generation_id IS NULL OR {})",
                in_predicate("aircraft_factory_package_id", &package_ids),
                in_predicate("aircraft_designation_id", &designation_ids),
                in_predicate("aircraft_generation_id", &generation_ids)
            ),
        )
        .await?,
    );
    for table in [
        "aircraft_engine_catalog_models",
        "aircraft_propeller_catalog_models",
        "aircraft_feature_definitions",
    ] {
        roots.insert(table.into(), fetch(source, table, "1 = 0").await?);
    }
    roots.insert(
        "aircraft_designation_faa_bindings".into(),
        fetch(
            source,
            "aircraft_designation_faa_bindings",
            &in_predicate("aircraft_designation_id", &designation_ids),
        )
        .await?,
    );
    roots.insert(
        "aircraft_tcds_make_lineage_bindings".into(),
        fetch(
            source,
            "aircraft_tcds_make_lineage_bindings",
            &format!(
                "{} AND {}",
                in_predicate("aircraft_make_id", &make_ids),
                in_predicate("aircraft_designation_id", &designation_ids)
            ),
        )
        .await?,
    );
    Ok(roots)
}

fn required_aircraft_decision_ids(
    roots: &BTreeMap<String, Vec<ProjectionRow>>,
) -> Result<Vec<i64>> {
    let mut decision_ids = BTreeSet::new();
    for row in roots.values().flatten() {
        if row.value("approval_decision_id").is_some() {
            decision_ids.insert(
                row.nullable_integer("approval_decision_id")?
                    .with_context(|| {
                        format!("selected {} row has no approval decision", row.table)
                    })?,
            );
        }
    }
    Ok(decision_ids.into_iter().collect())
}

fn validate_aircraft_approval_decisions(
    required_ids: &[i64],
    decisions: &[ProjectionRow],
) -> Result<()> {
    if ids(decisions, "id")? != required_ids
        || decisions
            .iter()
            .any(|row| row.value("decision_status").and_then(Value::as_str) != Some("approved"))
    {
        bail!("aircraft catalog closure has a missing or non-approved decision");
    }
    Ok(())
}

async fn selected_markets(
    source: &mut ProjectionReader<'_>,
    roots: &BTreeMap<String, Vec<ProjectionRow>>,
) -> Result<Vec<ProjectionRow>> {
    let mut selected = roots
        .values()
        .flatten()
        .filter_map(|row| row.nullable_integer("aircraft_market_id").transpose())
        .collect::<Result<BTreeSet<_>>>()?;
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    loop {
        let rows = fetch(
            source,
            "aircraft_markets",
            &in_predicate("id", &selected.iter().copied().collect::<Vec<_>>()),
        )
        .await?;
        let before = selected.len();
        for row in &rows {
            if let Some(parent) = row.nullable_integer("parent_market_id")? {
                selected.insert(parent);
            }
        }
        if selected.len() == before {
            return Ok(rows);
        }
    }
}

async fn fetch_rows_postgres(
    connection: &mut PgConnection,
    table: &str,
    predicate: &str,
) -> Result<Vec<ProjectionRow>> {
    let columns: Vec<(String, i64)> = sqlx::query_as(
        r#"SELECT attribute.attname,
                  COALESCE(
                    pg_catalog.array_position(
                      primary_index.indkey::smallint[],
                      attribute.attnum::smallint
                    ),
                    0
                  )::bigint AS primary_key_position
           FROM pg_catalog.pg_attribute attribute
           JOIN pg_catalog.pg_class relation ON relation.oid = attribute.attrelid
           JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace
           LEFT JOIN pg_catalog.pg_index primary_index
             ON primary_index.indrelid = relation.oid
            AND primary_index.indisprimary
           WHERE namespace.nspname = 'public'
             AND relation.relname = $1
             AND relation.relkind IN ('r', 'p')
             AND attribute.attnum > 0
             AND NOT attribute.attisdropped
           ORDER BY attribute.attnum"#,
    )
    .bind(table)
    .fetch_all(&mut *connection)
    .await?;
    if columns.is_empty() {
        bail!("required source/target table {table} is missing");
    }
    let arguments = columns
        .iter()
        .flat_map(|(column, _)| {
            [
                format!("'{}'", column.replace('\'', "''")),
                format!("{}", super::quoted_identifier(column)),
            ]
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut keys = columns
        .iter()
        .filter(|(_, position)| *position > 0)
        .collect::<Vec<_>>();
    keys.sort_by_key(|(_, position)| *position);
    let order = if keys.is_empty() {
        columns.iter().collect::<Vec<_>>()
    } else {
        keys
    }
    .into_iter()
    .map(|(column, _)| super::quoted_identifier(column))
    .collect::<Vec<_>>()
    .join(", ");
    let sql = format!(
        "SELECT pg_catalog.json_build_object({arguments})::text \
         FROM public.{} WHERE {predicate} ORDER BY {order}",
        super::quoted_identifier(table)
    );
    let rows = sqlx::query_scalar::<_, String>(&sql)
        .fetch_all(&mut *connection)
        .await?;
    rows.into_iter()
        .map(|json| {
            let mut object = serde_json::from_str::<Value>(&json)?
                .as_object()
                .cloned()
                .context("database JSON projection was not an object")?;
            let values = columns
                .iter()
                .map(|(column, _)| {
                    let value = object
                        .remove(column)
                        .with_context(|| format!("database JSON omitted {table}.{column}"))?;
                    super::canonicalize_value(table, column, value)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(ProjectionRow {
                table: table.to_string(),
                columns: columns.iter().map(|(column, _)| column.clone()).collect(),
                values,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(table: &str, id: i64, value: &str) -> ProjectionRow {
        ProjectionRow {
            table: table.into(),
            columns: vec!["id".into(), "value".into()],
            values: vec![Value::from(id), Value::from(value)],
        }
    }

    fn fields(table: &str, fields: Vec<(&str, Value)>) -> ProjectionRow {
        ProjectionRow {
            table: table.into(),
            columns: fields.iter().map(|(name, _)| (*name).into()).collect(),
            values: fields.into_iter().map(|(_, value)| value).collect(),
        }
    }

    #[test]
    fn fingerprint_is_order_stable_and_inventory_sensitive() {
        let first = assemble(
            vec![vec![
                row("faa_registry_coverage", 2, "N2"),
                row("faa_registry_coverage", 1, "N1"),
            ]],
            Vec::new(),
        )
        .unwrap();
        let reordered = assemble(
            vec![vec![
                row("faa_registry_coverage", 1, "N1"),
                row("faa_registry_coverage", 2, "N2"),
            ]],
            Vec::new(),
        )
        .unwrap();
        assert_eq!(first, reordered);
        assert_eq!(first.summary.table_counts["faa_registry_coverage"], 2);
        assert_eq!(first.summary.table_counts["avionics_models"], 0);
        assert!(
            serde_json::to_value(&first.summary)
                .unwrap()
                .get("version")
                .is_none(),
            "current catalog summaries must remain unversioned"
        );
        let envelope = FingerprintEnvelope {
            table_counts: CURRENT_TABLES
                .iter()
                .map(|table| FingerprintTableCount {
                    table,
                    count: first.summary.table_counts[*table],
                })
                .collect(),
            rows: &first.canonical_rows,
        };
        let mut expected = Sha256::new();
        expected.update(b"aircost:current-verified-catalog-projection\0");
        expected.update(serde_json::to_vec(&envelope).unwrap());
        assert_eq!(
            first.fingerprint_sha256(),
            format!("{:x}", expected.finalize())
        );

        let changed = assemble(
            vec![vec![
                row("faa_registry_coverage", 1, "N1"),
                row("faa_registry_coverage", 2, "changed non-representative"),
            ]],
            Vec::new(),
        )
        .unwrap();
        assert_ne!(first.fingerprint_sha256(), changed.fingerprint_sha256());
    }

    #[test]
    fn only_schema_generated_timestamps_are_omitted() {
        let mut origin = ProjectionRow {
            table: "avionics_authoritative_source_origins".into(),
            columns: vec!["id".into(), "https_origin".into(), "created_at".into()],
            values: vec![
                Value::from(1),
                Value::from("https://example.test"),
                Value::from("now"),
            ],
        };
        omit_column(&mut origin, "created_at").unwrap();
        assert_eq!(origin.columns, ["id", "https_origin"]);

        let durable = row("aircraft_identity_observations", 1, "exact bytes");
        let projection = assemble(vec![vec![durable.clone()]], Vec::new()).unwrap();
        assert!(projection
            .canonical_rows
            .iter()
            .any(|encoded| encoded == &canonical_row(&durable)));
    }

    #[test]
    fn full_faa_snapshot_closure_is_required() {
        let snapshots = [fields("faa_registry_snapshots", vec![("id", 1.into())])];
        let coverage = [
            fields(
                "faa_registry_coverage",
                vec![
                    ("snapshot_id", 1.into()),
                    ("n_number", "N1".into()),
                    ("lookup_status", "matched".into()),
                ],
            ),
            fields(
                "faa_registry_coverage",
                vec![
                    ("snapshot_id", 1.into()),
                    ("n_number", "N2".into()),
                    ("lookup_status", "absent".into()),
                ],
            ),
        ];
        let aircraft = [fields(
            "faa_registry_aircraft",
            vec![
                ("snapshot_id", 1.into()),
                ("n_number", "N1".into()),
                ("aircraft_code", "A1".into()),
                ("engine_code", "E1".into()),
            ],
        )];
        let aircraft_references = [fields(
            "faa_registry_aircraft_references",
            vec![("snapshot_id", 1.into()), ("aircraft_code", "A1".into())],
        )];
        let engine_references = [fields(
            "faa_registry_engine_references",
            vec![("snapshot_id", 1.into()), ("engine_code", "E1".into())],
        )];
        validate_faa_closure(
            &snapshots,
            &coverage,
            &aircraft,
            &aircraft_references,
            &engine_references,
        )
        .unwrap();
        assert!(
            validate_faa_closure(&snapshots, &coverage, &aircraft, &aircraft_references, &[])
                .unwrap_err()
                .to_string()
                .contains("ENGINE code sets differ")
        );
        assert!(validate_faa_closure(
            &snapshots,
            &coverage[..1],
            &[],
            &aircraft_references,
            &engine_references
        )
        .unwrap_err()
        .to_string()
        .contains("N-numbers differ"));
    }

    #[test]
    fn listing_derived_aircraft_provenance_is_rejected_without_rewriting() {
        let digest = "a".repeat(64);
        let mut observation_fields = vec![
            ("aircraft_sale_listing_id", Value::Null),
            ("source_url", Value::Null),
            ("observed_make", Value::Null),
            ("observed_family", Value::Null),
            ("observed_designation", Value::Null),
            ("observed_generation", Value::Null),
            ("observed_package", Value::Null),
            ("model_year", Value::Null),
            ("serial_number", Value::Null),
            ("registration_number", Value::Null),
            ("market_code", Value::Null),
            ("legacy_hint_json", Value::Null),
            (
                "exact_source_evidence",
                format!("catalog-provenance-source-observation-sha256:{digest}").into(),
            ),
            ("observation_sha256", digest.clone().into()),
        ];
        let observation = fields("aircraft_identity_observations", observation_fields.clone());
        let case = fields(
            "aircraft_identity_resolution_cases",
            vec![
                ("case_status", "resolved".into()),
                ("job_fingerprint", digest.clone().into()),
                ("catalog_revision", format!("sha256:{digest}").into()),
            ],
        );
        let decision = fields(
            "aircraft_identity_decisions",
            vec![
                ("decision_status", "approved".into()),
                ("deterministic_validation_passed", true.into()),
                (
                    "decision_payload_json",
                    serde_json::json!({
                        "projection_domain": "aircost:catalog-decision-projection:v2"
                    })
                    .to_string()
                    .into(),
                ),
                (
                    "deterministic_validation_json",
                    serde_json::json!({
                        "projection_domain": "aircost:catalog-decision-validation-projection:v1",
                        "passed": true
                    })
                    .to_string()
                    .into(),
                ),
            ],
        );
        let claim = fields(
            "curation_evidence_claims",
            vec![("validation_status", "validated".into())],
        );
        validate_catalog_safe_aircraft_provenance(
            &[observation],
            &[case.clone()],
            &[decision.clone()],
            &[claim.clone()],
        )
        .unwrap();

        observation_fields[1].1 = "https://listing.example/raw".into();
        let error = validate_catalog_safe_aircraft_provenance(
            &[fields("aircraft_identity_observations", observation_fields)],
            &[case],
            &[decision],
            &[claim],
        )
        .unwrap_err();
        assert!(error.to_string().contains("listing-derived"));
    }

    #[test]
    fn origins_are_generic_revocations_fail_and_users_are_not_fingerprinted() {
        let predicate = selected_origin_predicate(&[7], &[11]);
        assert!(predicate.contains("\"avionics_manufacturer_identity_id\" IN (7)"));
        assert!(predicate.contains("\"id\" IN (11)"));
        assert!(!predicate.to_ascii_lowercase().contains("garmin"));
        assert!(reject_selected_origin_revocations(&[]).is_ok());
        assert!(reject_selected_origin_revocations(&[row(
            "avionics_authoritative_source_origin_revocations",
            1,
            "revoked"
        )])
        .is_err());

        let first = assemble(
            vec![vec![row("avionics_models", 1, "unit")]],
            vec![RequiredCatalogUser {
                id: 1,
                email: "first@example.test".into(),
                display_name: "First".into(),
                auth_provider: "local".into(),
                auth_subject: "first".into(),
            }],
        )
        .unwrap();
        let second = assemble(
            vec![vec![row("avionics_models", 1, "unit")]],
            vec![RequiredCatalogUser {
                id: 1,
                email: "changed@example.test".into(),
                display_name: "Changed".into(),
                auth_provider: "local".into(),
                auth_subject: "changed".into(),
            }],
        )
        .unwrap();
        assert_eq!(first.fingerprint_sha256(), second.fingerprint_sha256());
        assert_ne!(first.summary.required_users, second.summary.required_users);
        assert!(assemble(
            vec![vec![row("aircraft_sale_listings", 1, "excluded")]],
            Vec::new()
        )
        .is_err());
        assert!(assemble(
            vec![vec![row("gemini_api_usage", 1, "excluded")]],
            Vec::new()
        )
        .is_err());
    }
}
