//! Provider-free projection of the reusable verified catalog closure.
//!
//! The projection deliberately excludes listings, assignments, reviews,
//! corrections, valuation artifacts, provider usage, and raw provider data.
//! Listing-derived observations and decisions are rewritten into bounded
//! catalog-provenance records before they enter the clean replay source.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::aircraft::faa::bridge::{FaaBridgeOutcome, LegacyFaaRepresentative};
use crate::db::{AppDb, DatabaseBackend};

const ROOT_TABLES: &[&str] = &[
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
];

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct CatalogProjectionReport {
    pub fingerprint_sha256: String,
    pub source_counts: BTreeMap<String, usize>,
    pub applied_rows: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ProjectionRow {
    table: String,
    columns: Vec<String>,
    values: Vec<Value>,
}

impl ProjectionRow {
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
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_i64()
                .map(Some)
                .with_context(|| format!("{}.{} is not an integer", self.table, column)),
        }
    }

    fn string(&self, column: &str) -> Result<&str> {
        self.value(column)
            .and_then(Value::as_str)
            .with_context(|| format!("{}.{} is not text", self.table, column))
    }

    fn set(&mut self, column: &str, value: Value) -> Result<()> {
        let index = self
            .columns
            .iter()
            .position(|candidate| candidate == column)
            .with_context(|| format!("{}.{} is missing", self.table, column))?;
        self.values[index] = value;
        Ok(())
    }
}

struct ProjectionBundle {
    groups: Vec<Vec<ProjectionRow>>,
    generated_keys: Vec<ProjectionRow>,
    generated_products: Vec<ProjectionRow>,
    required_users: Vec<ProjectionRow>,
    counts: BTreeMap<String, usize>,
    fingerprint_sha256: String,
}

impl ProjectionBundle {
    fn fingerprint_rows(&self) -> Result<Vec<ProjectionRow>> {
        let mut rows = self.groups.iter().flatten().cloned().collect::<Vec<_>>();
        for row in &mut rows {
            if row.table == "avionics_models" {
                row.set("catalog_status", Value::String("approved".into()))?;
            }
        }
        rows.extend(self.generated_keys.clone());
        rows.extend(self.generated_products.clone());
        rows.sort_by_key(canonical_row);
        Ok(rows)
    }
}

pub(crate) async fn required_faa_representatives(
    source: &mut SqliteConnection,
) -> Result<Vec<LegacyFaaRepresentative>> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        r#"SELECT representative_faa_registry_snapshot_id,
                  representative_faa_n_number
           FROM aircraft_tcds_make_lineage_bindings
           UNION
           SELECT binding.representative_faa_registry_snapshot_id,
                  claim.subject_text
           FROM aircraft_designation_faa_bindings binding
           JOIN curation_evidence_claims claim
             ON claim.id = binding.identity_evidence_claim_id
           ORDER BY 1, 2"#,
    )
    .fetch_all(&mut *source)
    .await?;
    let mut representatives = rows
        .into_iter()
        .map(|(snapshot_id, n_number)| {
            let normalized =
                crate::aircraft::faa::normalize_n_number(&n_number).with_context(|| {
                    format!("catalog representative has invalid N-number {n_number:?}")
                })?;
            if normalized != n_number {
                bail!("catalog representative N-number is not canonical: {n_number:?}");
            }
            Ok(LegacyFaaRepresentative {
                snapshot_id,
                n_number,
            })
        })
        .collect::<Result<BTreeSet<_>>>()?;

    let roots = selected_aircraft_roots(source).await?;
    let decision_ids = roots
        .values()
        .flatten()
        .filter_map(|row| row.nullable_integer("approval_decision_id").transpose())
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    if decision_ids.is_empty() {
        return Ok(representatives.into_iter().collect());
    }
    let hierarchy_claims: Vec<(i64, String, String)> = sqlx::query_as(&format!(
        r#"SELECT DISTINCT claim.id, claim.object_text, claim.quoted_evidence
           FROM aircraft_identity_decision_claims decision_claim
           JOIN curation_evidence_claims claim
             ON claim.id = decision_claim.evidence_claim_id
           WHERE decision_claim.decision_id IN ({})
             AND claim.evidence_source_id IN (
               SELECT evidence_source_id FROM faa_registry_snapshots
             )
           ORDER BY claim.id"#,
        decision_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    ))
    .fetch_all(&mut *source)
    .await?;
    for (claim_id, object_text, quoted_evidence) in hierarchy_claims {
        let Ok(object) = serde_json::from_str::<ServerFaaHierarchyObject>(&object_text) else {
            continue;
        };
        let fact_kind = ServerFaaHierarchyFactKind::from_evidence_id(&object.evidence_id)
            .with_context(|| format!("selected FAA hierarchy claim {claim_id} is invalid"))?;
        if object.supports != ["hierarchy_identity"] {
            bail!("selected FAA hierarchy claim {claim_id} has unsupported authority scope");
        }
        let parsed = parse_server_faa_hierarchy_claim(fact_kind, &quoted_evidence, claim_id)?;
        let facts = load_legacy_hierarchy_facts(source, &parsed).await?;
        validate_legacy_hierarchy_facts(&parsed, &facts, claim_id)?;
        validate_server_faa_hierarchy_evidence_id(&object.evidence_id, &parsed, &facts, claim_id)?;
        representatives.extend(facts.into_iter().map(|row| LegacyFaaRepresentative {
            snapshot_id: row.snapshot_id,
            n_number: row.n_number,
        }));
    }
    Ok(representatives.into_iter().collect())
}

pub(crate) async fn project_reusable_catalog(
    source: &mut SqliteConnection,
    target: &AppDb,
    faa: &FaaBridgeOutcome,
) -> Result<CatalogProjectionReport> {
    let mut bundle = build_bundle(source, faa).await?;
    bundle.fingerprint_sha256 = fingerprint(&bundle.fingerprint_rows()?)?;
    validate_required_users(target, &bundle.required_users).await?;
    let applied_rows = apply_bundle(target, &bundle).await?;
    validate_projected_target(target, &bundle).await?;
    Ok(CatalogProjectionReport {
        fingerprint_sha256: bundle.fingerprint_sha256,
        source_counts: bundle.counts,
        applied_rows,
    })
}

async fn build_bundle(
    source: &mut SqliteConnection,
    faa: &FaaBridgeOutcome,
) -> Result<ProjectionBundle> {
    let approved_models = fetch(source, "avionics_models", "catalog_status = 'approved'").await?;
    if approved_models.is_empty() {
        bail!("frozen source has no approved avionics models");
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
    if generated_products.len() != approved_models.len() {
        bail!("every approved avionics model must have one approved product identity");
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
        bail!("approved avionics manufacturers lack one exact identity membership");
    }
    let merges = fetch(
        source,
        "avionics_manufacturer_identity_merges",
        &format!(
            "{} OR {}",
            in_predicate("merged_identity_id", &identity_ids),
            in_predicate("canonical_identity_id", &identity_ids)
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
    let origins =
        fetch(
            source,
            "avionics_authoritative_source_origins",
            &format!(
            "{} OR ({} AND https_origin IN ('https://www.garmin.com','https://static.garmin.com'))",
            in_predicate("id", &ids(&reuse, "avionics_authoritative_source_origin_id")?),
            in_predicate("avionics_manufacturer_identity_id", &identity_ids)
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
    if !revocations.is_empty() {
        bail!("selected avionics closure contains a revoked authoritative origin");
    }

    let mut roots = selected_aircraft_roots(source).await?;
    let decision_ids = roots
        .values()
        .flatten()
        .filter_map(|row| row.nullable_integer("approval_decision_id").transpose())
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    let mut decisions = fetch(
        source,
        "aircraft_identity_decisions",
        &in_predicate("id", &decision_ids),
    )
    .await?;
    if decisions.len() != decision_ids.len()
        || decisions
            .iter()
            .any(|row| row.value("decision_status").and_then(Value::as_str) != Some("approved"))
    {
        bail!("aircraft catalog closure has a missing or non-approved decision");
    }
    let mut cases = fetch(
        source,
        "aircraft_identity_resolution_cases",
        &in_predicate("id", &ids(&decisions, "resolution_case_id")?),
    )
    .await?;
    let mut observations = fetch(
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
    let mut claims = fetch(
        source,
        "curation_evidence_claims",
        &in_predicate("id", &claim_ids.into_iter().collect::<Vec<_>>()),
    )
    .await?;
    let mut sources = fetch(
        source,
        "curation_evidence_sources",
        &in_predicate("id", &ids(&claims, "evidence_source_id")?),
    )
    .await?;
    let markets = selected_markets(source, &roots).await?;
    let mut user_ids = decisions
        .iter()
        .filter_map(|row| row.nullable_integer("decided_by_user_id").transpose())
        .collect::<Result<BTreeSet<_>>>()?;
    for row in &origins {
        if let Some(id) = row.nullable_integer("approved_by_user_id")? {
            user_ids.insert(id);
        }
    }
    let required_users = fetch(
        source,
        "users",
        &in_predicate("id", &user_ids.into_iter().collect::<Vec<_>>()),
    )
    .await?;

    remap_evidence_sources_and_faa_claims(source, &roots, &mut claims, &mut sources, faa).await?;
    remap_faa_roots(&mut roots, faa)?;
    project_observations(&mut observations)?;
    let catalog_revision = projected_catalog_revision(&roots)?;
    project_cases(&mut cases, &observations, &catalog_revision)?;
    project_decisions(&mut decisions, &roots)?;
    let mut staged_models = approved_models;
    for model in &mut staged_models {
        model.set("catalog_status", Value::String("unreviewed".into()))?;
    }

    let mut groups = vec![
        sources,
        claims,
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
        origins,
        identities,
        memberships,
        types,
        staged_models,
        model_types,
        suite_components,
        reuse,
    ]);
    let mut counts = BTreeMap::new();
    for rows in &groups {
        if let Some(row) = rows.first() {
            counts.insert(row.table.clone(), rows.len());
        }
    }
    counts.insert(
        "avionics_manufacturer_canonical_keys".into(),
        generated_keys.len(),
    );
    counts.insert(
        "avionics_approved_product_identities".into(),
        generated_products.len(),
    );
    Ok(ProjectionBundle {
        groups,
        generated_keys,
        generated_products,
        required_users,
        counts,
        fingerprint_sha256: String::new(),
    })
}

async fn selected_aircraft_roots(
    source: &mut SqliteConnection,
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
    roots.insert(
        "aircraft_make_aliases".into(),
        fetch(
            source,
            "aircraft_make_aliases",
            &in_predicate("aircraft_make_id", &make_ids),
        )
        .await?,
    );
    roots.insert(
        "aircraft_family_aliases".into(),
        fetch(
            source,
            "aircraft_family_aliases",
            &in_predicate("aircraft_model_family_id", &family_ids),
        )
        .await?,
    );
    roots.insert(
        "aircraft_designation_aliases".into(),
        fetch(
            source,
            "aircraft_designation_aliases",
            &in_predicate("aircraft_designation_id", &designation_ids),
        )
        .await?,
    );
    roots.insert(
        "aircraft_designation_identifiers".into(),
        fetch(
            source,
            "aircraft_designation_identifiers",
            &in_predicate("aircraft_designation_id", &designation_ids),
        )
        .await?,
    );
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
    roots.insert(
        "aircraft_serial_number_schemes".into(),
        fetch(
            source,
            "aircraft_serial_number_schemes",
            &in_predicate("aircraft_make_id", &make_ids),
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

async fn selected_markets(
    source: &mut SqliteConnection,
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

fn project_observations(rows: &mut [ProjectionRow]) -> Result<()> {
    for row in rows {
        let source_hash = row.string("observation_sha256")?.to_string();
        for column in [
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
        ] {
            row.set(column, Value::Null)?;
        }
        row.set(
            "exact_source_evidence",
            Value::String(format!(
                "catalog-provenance-source-observation-sha256:{source_hash}"
            )),
        )?;
        let material = serde_json::json!({
            "projection_domain": "aircost:catalog-observation-projection:v2",
            "source_observation_sha256": source_hash,
            "exact_source_evidence": row.string("exact_source_evidence")?,
        });
        row.set("observation_sha256", Value::String(sha256_json(&material)?))?;
    }
    Ok(())
}

fn projected_catalog_revision(roots: &BTreeMap<String, Vec<ProjectionRow>>) -> Result<String> {
    let mut material = roots.values().flatten().cloned().collect::<Vec<_>>();
    material.sort_by_key(canonical_row);
    Ok(format!(
        "sha256:{}",
        sha256_json(&serde_json::json!({
            "projection_domain": "aircost:catalog-revision-projection:v1",
            "rows": material,
        }))?
    ))
}

fn project_cases(
    cases: &mut [ProjectionRow],
    observations: &[ProjectionRow],
    catalog_revision: &str,
) -> Result<()> {
    let observation_hashes = observations
        .iter()
        .map(|row| {
            Ok((
                row.integer("id")?,
                row.string("observation_sha256")?.to_string(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    for row in cases {
        let observation_id = row.integer("observation_id")?;
        let material = serde_json::json!({
            "projection_domain": "aircost:catalog-resolution-case-projection:v1",
            "source_case_id": row.integer("id")?,
            "observation_sha256": observation_hashes.get(&observation_id).context("case references an unselected observation")?,
            "resolution_scope": row.string("resolution_scope")?,
            "catalog_revision": catalog_revision,
        });
        row.set("job_fingerprint", Value::String(sha256_json(&material)?))?;
        row.set(
            "catalog_revision",
            Value::String(catalog_revision.to_string()),
        )?;
        row.set("case_status", Value::String("resolved".into()))?;
    }
    Ok(())
}

fn project_decisions(
    decisions: &mut [ProjectionRow],
    roots: &BTreeMap<String, Vec<ProjectionRow>>,
) -> Result<()> {
    for row in decisions {
        let id = row.integer("id")?;
        let mut approved_rows = roots
            .values()
            .flatten()
            .filter(|candidate| {
                candidate
                    .nullable_integer("approval_decision_id")
                    .is_ok_and(|candidate_id| candidate_id == Some(id))
            })
            .map(|candidate| {
                serde_json::json!({
                    "table": candidate.table,
                    "row_sha256": sha256_text(&canonical_row(candidate)),
                })
            })
            .collect::<Vec<_>>();
        approved_rows.sort_by_key(Value::to_string);
        if approved_rows.is_empty() {
            bail!("selected aircraft decision {id} approves no retained catalog row");
        }
        row.set(
            "decision_payload_json",
            Value::String(
                serde_json::json!({
                    "projection_domain": "aircost:catalog-decision-projection:v2",
                    "entity_kind": row.string("entity_kind")?,
                    "decision_action": row.string("decision_action")?,
                    "selected_entity_id": row.value("selected_entity_id"),
                    "catalog_rows": approved_rows,
                })
                .to_string(),
            ),
        )?;
        row.set(
            "deterministic_validation_json",
            Value::String(
                serde_json::json!({
                    "projection_domain": "aircost:catalog-decision-validation-projection:v1",
                    "passed": true,
                })
                .to_string(),
            ),
        )?;
        row.set(
            "rationale",
            Value::String("Approved reusable catalog provenance projection.".into()),
        )?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FaaIdentityObject {
    aircraft_code: String,
    manufacturer: String,
    model: String,
    source_record_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerFaaHierarchyObject {
    evidence_id: String,
    supports: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServerFaaHierarchyFactKind {
    Make,
    Designation,
}

impl ServerFaaHierarchyFactKind {
    fn from_evidence_id(value: &str) -> Result<Self> {
        let (kind, digest) = if let Some(digest) = value.strip_prefix("server_faa_registry.make.") {
            (Self::Make, digest)
        } else if let Some(digest) = value.strip_prefix("server_faa_registry.designation.") {
            (Self::Designation, digest)
        } else {
            bail!("server FAA hierarchy evidence ID has an unsupported kind");
        };
        require_lower_sha256(digest, "server FAA hierarchy evidence ID")?;
        Ok(kind)
    }

    fn quote_prefix(self) -> &'static str {
        match self {
            Self::Make => "Imported FAA ACFTREF reports manufacturer_name=",
            Self::Designation => "Imported FAA ACFTREF reports model_name=",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Make => "make",
            Self::Designation => "designation",
        }
    }
}

struct ParsedServerFaaHierarchyClaim {
    fact_kind: ServerFaaHierarchyFactKind,
    reported_fact_debug: String,
    snapshot_ids: BTreeSet<i64>,
    snapshot_date: String,
    archive_sha256: String,
    source_manifest_sha256: String,
    source_record_sha256s: BTreeSet<String>,
    n_numbers: BTreeSet<String>,
    aircraft_code: String,
    provenance: String,
}

#[derive(sqlx::FromRow)]
struct LegacyHierarchyFactRow {
    snapshot_id: i64,
    n_number: String,
    aircraft_code: String,
    manufacturer_name: Option<String>,
    model_name: Option<String>,
    source_record_sha256: String,
}

async fn remap_evidence_sources_and_faa_claims(
    source: &mut SqliteConnection,
    roots: &BTreeMap<String, Vec<ProjectionRow>>,
    claims: &mut [ProjectionRow],
    sources: &mut Vec<ProjectionRow>,
    faa: &FaaBridgeOutcome,
) -> Result<()> {
    let claim_indexes = claims
        .iter()
        .enumerate()
        .map(|(index, row)| Ok((row.integer("id")?, index)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut faa_claim_representatives = BTreeMap::new();
    for row in &roots["aircraft_designation_faa_bindings"] {
        let claim_id = row.integer("identity_evidence_claim_id")?;
        let claim = claims
            .get(
                *claim_indexes
                    .get(&claim_id)
                    .context("designation FAA binding references an unselected evidence claim")?,
            )
            .context("designation FAA evidence claim index is invalid")?;
        faa_claim_representatives.insert(
            claim_id,
            LegacyFaaRepresentative {
                snapshot_id: row.integer("representative_faa_registry_snapshot_id")?,
                n_number: claim.string("subject_text")?.to_string(),
            },
        );
    }

    let mut faa_claim_ids = faa_claim_representatives
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    for claim in claims.iter_mut() {
        let source_id = claim.integer("evidence_source_id")?;
        if !faa.legacy_evidence_source_ids.contains(&source_id) {
            continue;
        }
        let claim_id = claim.integer("id")?;
        if let Some(legacy) = faa_claim_representatives.get(&claim_id) {
            rebuild_faa_identity_claim(source, claim, claim_id, legacy, faa).await?;
        } else {
            rebuild_server_faa_hierarchy_claim(source, claim, claim_id, faa).await?;
        }
        faa_claim_ids.insert(claim_id);
    }

    sources.retain(|row| {
        row.integer("id")
            .is_ok_and(|id| !faa.legacy_evidence_source_ids.contains(&id))
    });
    sources.sort_by_key(|row| row.integer("id").unwrap_or(i64::MAX));
    let mut used = BTreeSet::from([faa.current_evidence_source_id]);
    let mut next_id = sources
        .iter()
        .filter_map(|row| row.integer("id").ok())
        .chain([faa.current_evidence_source_id])
        .max()
        .unwrap_or(0)
        + 1;
    let mut source_id_remap = BTreeMap::new();
    for row in sources.iter_mut() {
        let old_id = row.integer("id")?;
        let new_id = if used.insert(old_id) {
            old_id
        } else {
            while !used.insert(next_id) {
                next_id += 1;
            }
            let assigned = next_id;
            next_id += 1;
            row.set("id", Value::from(assigned))?;
            assigned
        };
        source_id_remap.insert(old_id, new_id);
    }
    for claim in claims {
        let claim_id = claim.integer("id")?;
        if faa_claim_ids.contains(&claim_id) {
            continue;
        }
        let old_id = claim.integer("evidence_source_id")?;
        claim.set(
            "evidence_source_id",
            Value::from(*source_id_remap.get(&old_id).with_context(|| {
                format!("selected evidence claim references excluded source {old_id}")
            })?),
        )?;
    }
    Ok(())
}

async fn rebuild_faa_identity_claim(
    source: &mut SqliteConnection,
    claim: &mut ProjectionRow,
    claim_id: i64,
    legacy: &LegacyFaaRepresentative,
    faa: &FaaBridgeOutcome,
) -> Result<()> {
    let current = faa
        .representative_remap
        .get(legacy)
        .context("typed FAA representative remap is missing")?;
    require_common_faa_claim_shape(claim, claim_id)?;
    if claim.string("predicate_text")? != "FAA registered aircraft identity"
        || claim.string("subject_text")? != legacy.n_number
    {
        bail!("legacy FAA claim {claim_id} does not have the exact typed identity shape");
    }
    let object: FaaIdentityObject = serde_json::from_str(claim.string("object_text")?)
        .with_context(|| {
            format!("legacy FAA claim {claim_id} object is not canonical identity JSON")
        })?;
    let legacy_fact: (String, String, Option<String>, Option<String>, String) = sqlx::query_as(
        r#"SELECT aircraft.n_number, aircraft.aircraft_code,
                  reference.manufacturer_name, reference.model_name,
                  aircraft.source_record_sha256
           FROM faa_registry_aircraft aircraft
           JOIN faa_registry_aircraft_references reference
             ON reference.snapshot_id = aircraft.snapshot_id
            AND reference.aircraft_code = aircraft.aircraft_code
           WHERE aircraft.snapshot_id = ? AND aircraft.n_number = ?"#,
    )
    .bind(legacy.snapshot_id)
    .bind(&legacy.n_number)
    .fetch_optional(&mut *source)
    .await?
    .context("legacy FAA claim representative has no exact retained registry fact")?;
    let manufacturer = legacy_fact
        .2
        .as_deref()
        .context("legacy FAA identity claim has no ACFTREF manufacturer")?;
    let model = legacy_fact
        .3
        .as_deref()
        .context("legacy FAA identity claim has no ACFTREF model")?;
    if object.aircraft_code != legacy_fact.1
        || object.manufacturer != manufacturer
        || object.model != model
        || object.source_record_sha256 != legacy_fact.4
        || faa
            .obsolete_hash_replacements
            .get(&legacy_fact.4)
            .is_none_or(|replacement| replacement != &current.source_record_sha256)
    {
        bail!("legacy FAA claim {claim_id} disagrees with its typed registry representative");
    }
    let expected_quote = format!(
        "FAA ACFTREF {}: {} {}; MASTER {} record sha256 {}",
        legacy_fact.1, manufacturer, model, legacy.n_number, legacy_fact.4
    );
    if claim.string("quoted_evidence")? != expected_quote {
        bail!("legacy FAA claim {claim_id} quote is not the exact registry identity receipt");
    }
    claim.set(
        "evidence_source_id",
        Value::from(faa.current_evidence_source_id),
    )?;
    claim.set(
        "object_text",
        Value::String(
            serde_json::json!({
                "aircraft_code": legacy_fact.1,
                "manufacturer": manufacturer,
                "model": model,
                "source_record_sha256": current.source_record_sha256,
            })
            .to_string(),
        ),
    )?;
    claim.set(
        "quoted_evidence",
        Value::String(format!(
            "FAA ACFTREF {}: {} {}; MASTER {} record sha256 {}",
            legacy_fact.1, manufacturer, model, legacy.n_number, current.source_record_sha256
        )),
    )?;
    Ok(())
}

async fn rebuild_server_faa_hierarchy_claim(
    source: &mut SqliteConnection,
    claim: &mut ProjectionRow,
    claim_id: i64,
    faa: &FaaBridgeOutcome,
) -> Result<()> {
    require_common_faa_claim_shape(claim, claim_id)?;
    if claim.string("subject_text")? != "aircraft_model_hierarchy"
        || claim.string("predicate_text")? != "supports verified aircraft hierarchy decision"
    {
        bail!("legacy FAA hierarchy claim {claim_id} has an unexpected subject or predicate");
    }
    let object: ServerFaaHierarchyObject = serde_json::from_str(claim.string("object_text")?)
        .with_context(|| format!("legacy FAA hierarchy claim {claim_id} object is invalid"))?;
    let fact_kind = ServerFaaHierarchyFactKind::from_evidence_id(&object.evidence_id)?;
    if object.supports != ["hierarchy_identity"] {
        bail!("legacy FAA hierarchy claim {claim_id} has unsupported authority scope");
    }
    let parsed =
        parse_server_faa_hierarchy_claim(fact_kind, claim.string("quoted_evidence")?, claim_id)?;
    if parsed.snapshot_date != faa.report.snapshot_date
        || parsed.archive_sha256 != faa.report.archive_sha256
        || !faa
            .obsolete_hash_replacements
            .contains_key(&parsed.source_manifest_sha256)
    {
        bail!("legacy FAA hierarchy claim {claim_id} references a different release");
    }
    let facts = load_legacy_hierarchy_facts(source, &parsed).await?;
    validate_legacy_hierarchy_facts(&parsed, &facts, claim_id)?;
    validate_server_faa_hierarchy_evidence_id(&object.evidence_id, &parsed, &facts, claim_id)?;
    let fact_values = facts
        .iter()
        .map(|row| match parsed.fact_kind {
            ServerFaaHierarchyFactKind::Make => row.manufacturer_name.as_deref(),
            ServerFaaHierarchyFactKind::Designation => row.model_name.as_deref(),
        })
        .collect::<BTreeSet<_>>();
    if fact_values.len() != 1 || fact_values.contains(&None) {
        bail!("legacy FAA hierarchy claim {claim_id} does not resolve to one exact ACFTREF fact");
    }
    let fact_value = fact_values
        .into_iter()
        .next()
        .flatten()
        .context("FAA hierarchy fact unexpectedly disappeared")?;
    if parsed.reported_fact_debug != format!("{fact_value:?}") {
        bail!("legacy FAA hierarchy claim {claim_id} quote disagrees with ACFTREF");
    }
    let current_record_hashes = parsed
        .source_record_sha256s
        .iter()
        .map(|old| {
            faa.obsolete_hash_replacements
                .get(old)
                .cloned()
                .with_context(|| {
                    format!(
                        "legacy FAA hierarchy claim {claim_id} has no parser-derived record remap"
                    )
                })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let current_manifest = faa
        .obsolete_hash_replacements
        .get(&parsed.source_manifest_sha256)
        .context("legacy FAA hierarchy claim has no parser-derived manifest remap")?;
    let current_record_hashes = current_record_hashes.into_iter().collect::<Vec<_>>();
    claim.set(
        "evidence_source_id",
        Value::from(faa.current_evidence_source_id),
    )?;
    claim.set(
        "object_text",
        Value::String(
            serde_json::json!({
                "projection_domain": "aircost:catalog-faa-hierarchy-claim-projection:v1",
                "fact_kind": parsed.fact_kind.as_str(),
                "fact_value": fact_value,
                "faa_snapshot_id": faa.report.stored_snapshot_id,
                "faa_snapshot_date": faa.report.snapshot_date,
                "faa_archive_sha256": faa.report.archive_sha256,
                "faa_source_manifest_sha256": current_manifest,
                "faa_aircraft_code": parsed.aircraft_code,
                "faa_source_record_sha256s": current_record_hashes,
                "supports": ["hierarchy_identity"],
            })
            .to_string(),
        ),
    )?;
    claim.set(
        "quoted_evidence",
        Value::String(format!(
            "Current FAA ACFTREF {} {}={:?}; retained MASTER record sha256s {}",
            parsed.aircraft_code,
            match parsed.fact_kind {
                ServerFaaHierarchyFactKind::Make => "manufacturer_name",
                ServerFaaHierarchyFactKind::Designation => "model_name",
            },
            fact_value,
            current_record_hashes.join(",")
        )),
    )?;
    Ok(())
}

fn require_common_faa_claim_shape(claim: &ProjectionRow, claim_id: i64) -> Result<()> {
    if claim.string("claim_kind")? != "identity"
        || claim.string("validation_status")? != "validated"
        || claim.value("validated_at").is_none_or(Value::is_null)
        || claim.value("citation_start") != Some(&Value::Null)
        || claim.value("citation_end") != Some(&Value::Null)
    {
        bail!("legacy FAA claim {claim_id} is not one exact validated identity claim");
    }
    Ok(())
}

fn parse_server_faa_hierarchy_claim(
    fact_kind: ServerFaaHierarchyFactKind,
    quote: &str,
    claim_id: i64,
) -> Result<ParsedServerFaaHierarchyClaim> {
    let rest = quote
        .strip_prefix(fact_kind.quote_prefix())
        .with_context(|| {
            format!("legacy FAA hierarchy claim {claim_id} quote has the wrong fact prefix")
        })?;
    let (reported_fact_debug, rest) = required_split(rest, "; snapshot_ids=", claim_id)?;
    let provenance = format!("snapshot_ids={rest}");
    let (snapshot_ids, rest) = required_split(rest, "; snapshot_date=", claim_id)?;
    let (snapshot_date, rest) = required_split(rest, "; archive_sha256=", claim_id)?;
    let (archive_sha256, rest) = required_split(rest, "; source_manifest_sha256=", claim_id)?;
    let (source_manifest_sha256, rest) = required_split(rest, "; source_record_sha256=", claim_id)?;
    let (source_record_sha256s, rest) = required_split(rest, "; n_numbers=", claim_id)?;
    let (n_numbers, rest) = required_split(rest, "; aircraft_code=", claim_id)?;
    let (aircraft_code, rest) = required_split(rest, "; observations=", claim_id)?;
    let (observations, case_token) = rest.rsplit_once("; case_token=").with_context(|| {
        format!("legacy FAA hierarchy claim {claim_id} quote omits case provenance")
    })?;
    if reported_fact_debug.is_empty()
        || observations.is_empty()
        || case_token.trim().is_empty()
        || aircraft_code.trim().is_empty()
        || snapshot_date.len() != 10
    {
        bail!("legacy FAA hierarchy claim {claim_id} has incomplete provenance");
    }
    require_lower_sha256(archive_sha256, "FAA hierarchy archive")?;
    require_lower_sha256(source_manifest_sha256, "FAA hierarchy source manifest")?;
    let snapshot_ids = parse_sorted_i64_set(snapshot_ids, "snapshot_ids", claim_id)?;
    let source_record_sha256s =
        parse_sorted_text_set(source_record_sha256s, "source_record_sha256", claim_id)?;
    for digest in &source_record_sha256s {
        require_lower_sha256(digest, "FAA hierarchy source record")?;
    }
    let n_numbers = parse_sorted_text_set(n_numbers, "n_numbers", claim_id)?;
    for n_number in &n_numbers {
        let normalized = crate::aircraft::faa::normalize_n_number(n_number).with_context(|| {
            format!("legacy FAA hierarchy claim {claim_id} has an invalid N-number")
        })?;
        if normalized != *n_number {
            bail!("legacy FAA hierarchy claim {claim_id} has a non-canonical N-number");
        }
    }
    Ok(ParsedServerFaaHierarchyClaim {
        fact_kind,
        reported_fact_debug: reported_fact_debug.to_string(),
        snapshot_ids,
        snapshot_date: snapshot_date.to_string(),
        archive_sha256: archive_sha256.to_string(),
        source_manifest_sha256: source_manifest_sha256.to_string(),
        source_record_sha256s,
        n_numbers,
        aircraft_code: aircraft_code.to_string(),
        provenance,
    })
}

async fn load_legacy_hierarchy_facts(
    source: &mut SqliteConnection,
    parsed: &ParsedServerFaaHierarchyClaim,
) -> Result<Vec<LegacyHierarchyFactRow>> {
    let mut query = QueryBuilder::<Sqlite>::new(
        r#"SELECT aircraft.snapshot_id, aircraft.n_number, aircraft.aircraft_code,
                  reference.manufacturer_name, reference.model_name,
                  aircraft.source_record_sha256
           FROM faa_registry_aircraft aircraft
           JOIN faa_registry_aircraft_references reference
             ON reference.snapshot_id = aircraft.snapshot_id
            AND reference.aircraft_code = aircraft.aircraft_code
           WHERE aircraft.aircraft_code = "#,
    );
    query.push_bind(&parsed.aircraft_code);
    query.push(" AND aircraft.snapshot_id IN (");
    {
        let mut separated = query.separated(", ");
        for snapshot_id in &parsed.snapshot_ids {
            separated.push_bind(snapshot_id);
        }
    }
    query.push(") AND aircraft.n_number IN (");
    {
        let mut separated = query.separated(", ");
        for n_number in &parsed.n_numbers {
            separated.push_bind(n_number);
        }
    }
    query.push(") AND aircraft.source_record_sha256 IN (");
    {
        let mut separated = query.separated(", ");
        for digest in &parsed.source_record_sha256s {
            separated.push_bind(digest);
        }
    }
    query.push(") ORDER BY aircraft.snapshot_id, aircraft.n_number");
    Ok(query.build_query_as().fetch_all(&mut *source).await?)
}

fn validate_legacy_hierarchy_facts(
    parsed: &ParsedServerFaaHierarchyClaim,
    facts: &[LegacyHierarchyFactRow],
    claim_id: i64,
) -> Result<()> {
    if facts.is_empty()
        || facts
            .iter()
            .map(|row| row.snapshot_id)
            .collect::<BTreeSet<_>>()
            != parsed.snapshot_ids
        || facts
            .iter()
            .map(|row| row.n_number.clone())
            .collect::<BTreeSet<_>>()
            != parsed.n_numbers
        || facts
            .iter()
            .map(|row| row.source_record_sha256.clone())
            .collect::<BTreeSet<_>>()
            != parsed.source_record_sha256s
        || facts
            .iter()
            .any(|row| row.aircraft_code != parsed.aircraft_code)
    {
        bail!("legacy FAA hierarchy claim {claim_id} does not match its retained FAA rows");
    }
    Ok(())
}

fn validate_server_faa_hierarchy_evidence_id(
    evidence_id: &str,
    parsed: &ParsedServerFaaHierarchyClaim,
    facts: &[LegacyHierarchyFactRow],
    claim_id: i64,
) -> Result<()> {
    let fact_values = facts
        .iter()
        .map(|row| match parsed.fact_kind {
            ServerFaaHierarchyFactKind::Make => row.manufacturer_name.as_deref(),
            ServerFaaHierarchyFactKind::Designation => row.model_name.as_deref(),
        })
        .collect::<BTreeSet<_>>();
    if fact_values.len() != 1 || fact_values.contains(&None) {
        bail!("legacy FAA hierarchy claim {claim_id} does not resolve to one exact ACFTREF fact");
    }
    let fact_value = fact_values
        .into_iter()
        .next()
        .flatten()
        .context("FAA hierarchy fact unexpectedly disappeared")?;
    let expected =
        server_faa_hierarchy_evidence_id(parsed.fact_kind, fact_value, &parsed.provenance)?;
    if evidence_id != expected {
        bail!(
            "legacy FAA hierarchy claim {claim_id} evidence ID does not authenticate its receipt"
        );
    }
    Ok(())
}

fn server_faa_hierarchy_evidence_id(
    fact_kind: ServerFaaHierarchyFactKind,
    fact_value: &str,
    provenance: &str,
) -> Result<String> {
    let case_token = provenance
        .rsplit_once("; case_token=")
        .map(|(_, case_token)| case_token)
        .filter(|value| !value.trim().is_empty())
        .context("FAA hierarchy provenance omits its case token")?;
    let mut hasher = Sha256::new();
    hasher.update(b"aircost-server-faa-evidence-v1\0");
    hasher.update(case_token.as_bytes());
    hasher.update(b"\0");
    hasher.update(fact_kind.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(fact_value.as_bytes());
    hasher.update(b"\0");
    hasher.update(provenance.as_bytes());
    Ok(format!(
        "server_faa_registry.{}.{:x}",
        fact_kind.as_str(),
        hasher.finalize()
    ))
}

fn required_split<'a>(value: &'a str, marker: &str, claim_id: i64) -> Result<(&'a str, &'a str)> {
    value
        .split_once(marker)
        .with_context(|| format!("legacy FAA hierarchy claim {claim_id} quote omits {marker:?}"))
}

fn parse_sorted_i64_set(value: &str, field: &str, claim_id: i64) -> Result<BTreeSet<i64>> {
    let values = value
        .split(',')
        .map(|part| part.parse::<i64>())
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .with_context(|| format!("legacy FAA hierarchy claim {claim_id} has invalid {field}"))?;
    if values.is_empty()
        || values.iter().any(|value| *value <= 0)
        || values
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",")
            != value
    {
        bail!("legacy FAA hierarchy claim {claim_id} has non-canonical {field}");
    }
    Ok(values)
}

fn parse_sorted_text_set(value: &str, field: &str, claim_id: i64) -> Result<BTreeSet<String>> {
    let values = value
        .split(',')
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if values.is_empty()
        || values.iter().any(|value| value.is_empty())
        || values
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(",")
            != value
    {
        bail!("legacy FAA hierarchy claim {claim_id} has non-canonical {field}");
    }
    Ok(values)
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

fn remap_faa_roots(
    roots: &mut BTreeMap<String, Vec<ProjectionRow>>,
    faa: &FaaBridgeOutcome,
) -> Result<()> {
    for row in roots
        .get_mut("aircraft_tcds_make_lineage_bindings")
        .context("TCDS lineage root selection is absent")?
    {
        let legacy = LegacyFaaRepresentative {
            snapshot_id: row.integer("representative_faa_registry_snapshot_id")?,
            n_number: row.string("representative_faa_n_number")?.to_string(),
        };
        let current = faa
            .representative_remap
            .get(&legacy)
            .context("TCDS lineage representative has no current FAA remap")?;
        require_same_faa_release(row, faa)?;
        row.set(
            "representative_faa_registry_snapshot_id",
            Value::from(current.snapshot_id),
        )?;
        row.set(
            "representative_faa_source_record_sha256",
            Value::String(current.source_record_sha256.clone()),
        )?;
    }
    for row in roots
        .get_mut("aircraft_designation_faa_bindings")
        .context("designation FAA binding root selection is absent")?
    {
        require_same_faa_release(row, faa)?;
        let old_snapshot = row.integer("representative_faa_registry_snapshot_id")?;
        let remaps = faa
            .representative_remap
            .iter()
            .filter(|(legacy, _)| legacy.snapshot_id == old_snapshot)
            .map(|(_, current)| current.snapshot_id)
            .collect::<BTreeSet<_>>();
        if remaps.len() != 1 {
            bail!("designation FAA binding snapshot has no unique current remap");
        }
        row.set(
            "representative_faa_registry_snapshot_id",
            Value::from(*remaps.iter().next().unwrap()),
        )?;
    }
    Ok(())
}

fn require_same_faa_release(row: &ProjectionRow, faa: &FaaBridgeOutcome) -> Result<()> {
    if row.string("faa_snapshot_date")? != faa.report.snapshot_date
        || row.string("faa_archive_sha256")? != faa.report.archive_sha256
    {
        bail!("{} references a different FAA release", row.table);
    }
    Ok(())
}

async fn fetch(
    source: &mut SqliteConnection,
    table: &str,
    predicate: &str,
) -> Result<Vec<ProjectionRow>> {
    fetch_rows(source, table, predicate).await
}

#[derive(Clone, Debug)]
struct ColumnInfo {
    name: String,
    primary_key_position: i64,
}

async fn fetch_rows(
    connection: &mut SqliteConnection,
    table: &str,
    predicate: &str,
) -> Result<Vec<ProjectionRow>> {
    let columns = sqlite_columns(connection, table).await?;
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
    let mut keys = columns
        .iter()
        .filter(|column| column.primary_key_position > 0)
        .collect::<Vec<_>>();
    keys.sort_by_key(|column| column.primary_key_position);
    let order = if keys.is_empty() {
        columns.iter().collect::<Vec<_>>()
    } else {
        keys
    }
    .into_iter()
    .map(|column| quoted_identifier(&column.name))
    .collect::<Vec<_>>()
    .join(", ");
    let sql = format!(
        "SELECT json_object({arguments}) FROM {} WHERE {predicate} ORDER BY {order}",
        quoted_identifier(table)
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
                .map(|column| {
                    let value = object.remove(&column.name).with_context(|| {
                        format!("database JSON omitted {table}.{}", column.name)
                    })?;
                    canonicalize_value(table, &column.name, value)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(ProjectionRow {
                table: table.to_string(),
                columns: columns.iter().map(|column| column.name.clone()).collect(),
                values,
            })
        })
        .collect()
}

async fn sqlite_columns(connection: &mut SqliteConnection, table: &str) -> Result<Vec<ColumnInfo>> {
    let rows = sqlx::query(&format!("PRAGMA table_info({})", quoted_identifier(table)))
        .fetch_all(&mut *connection)
        .await?;
    if rows.is_empty() {
        bail!("required source/target table {table} is missing");
    }
    Ok(rows
        .into_iter()
        .map(|row| ColumnInfo {
            name: row.get("name"),
            primary_key_position: row.get("pk"),
        })
        .collect())
}

fn canonicalize_value(table: &str, column: &str, value: Value) -> Result<Value> {
    match (table, column) {
        ("aircraft_identity_decisions", "deterministic_validation_passed") => match value {
            Value::Number(number) if number.as_i64() == Some(0) => Ok(Value::Bool(false)),
            Value::Number(number) if number.as_i64() == Some(1) => Ok(Value::Bool(true)),
            Value::Bool(value) => Ok(Value::Bool(value)),
            _ => bail!("{table}.{column} is not boolean"),
        },
        ("avionics_models", "estimated_unit_value_usd" | "replacement_cost_usd") => {
            let Value::Number(number) = value else {
                return if value.is_null() {
                    Ok(value)
                } else {
                    Err(anyhow::anyhow!("{table}.{column} is not numeric"))
                };
            };
            let number = number.as_f64().context("non-finite catalog value")?;
            if number.fract() == 0.0 {
                Ok(Value::from(number as i64))
            } else {
                Ok(Value::from(number))
            }
        }
        _ => Ok(value),
    }
}

fn ids(rows: &[ProjectionRow], column: &str) -> Result<Vec<i64>> {
    Ok(rows
        .iter()
        .map(|row| row.integer(column))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect())
}

fn in_predicate(column: &str, ids: &[i64]) -> String {
    if ids.is_empty() {
        "1 = 0".into()
    } else {
        format!(
            "{} IN ({})",
            quoted_identifier(column),
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        )
    }
}

fn primary_key_predicate(row: &ProjectionRow) -> Result<String> {
    let columns: &[&str] = match row.table.as_str() {
        "avionics_manufacturer_canonical_keys" => &["avionics_manufacturer_id"],
        "avionics_approved_product_identities" => &["avionics_model_id"],
        "avionics_model_types" => &["avionics_model_id", "avionics_type_id"],
        "avionics_suite_components" => &["suite_model_id", "component_model_id"],
        "avionics_product_reuse_attestations" => &["avionics_model_id"],
        "avionics_manufacturer_identity_memberships" => &["avionics_manufacturer_id"],
        "aircraft_identity_decision_claims" => {
            &["decision_id", "evidence_claim_id", "evidence_role"]
        }
        "aircraft_generation_designations" => {
            &["aircraft_generation_id", "aircraft_designation_id"]
        }
        "aircraft_designation_faa_bindings" => &[
            "faa_snapshot_date",
            "faa_archive_sha256",
            "faa_aircraft_code",
        ],
        _ if row.columns.iter().any(|column| column == "id") => &["id"],
        _ => bail!("no stable projection key is defined for {}", row.table),
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
        Value::Bool(value) => Ok(i64::from(*value).to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(format!("'{}'", value.replace('\'', "''"))),
        _ => bail!("projection key is not a scalar"),
    }
}

fn quoted_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn canonical_row(row: &ProjectionRow) -> String {
    serde_json::to_string(row).expect("projection row serialization cannot fail")
}

fn fingerprint(rows: &[ProjectionRow]) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(rows)?)))
}

fn sha256_json(value: &Value) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

async fn validate_required_users(target: &AppDb, required: &[ProjectionRow]) -> Result<()> {
    let DatabaseBackend::Sqlite(pool) = target.backend() else {
        bail!("catalog replay projection target must be SQLite");
    };
    let mut connection = pool.acquire().await?;
    for expected in required {
        let id = expected.integer("id")?;
        let actual = fetch_rows(&mut connection, "users", &format!("id = {id}"))
            .await?
            .into_iter()
            .next()
            .with_context(|| format!("required catalog reviewer user {id} is absent"))?;
        if ![
            "id",
            "email",
            "display_name",
            "auth_provider",
            "auth_subject",
        ]
        .into_iter()
        .all(|column| actual.value(column) == expected.value(column))
        {
            bail!("required catalog reviewer user {id} differs from the signed capture owner");
        }
    }
    Ok(())
}

async fn apply_bundle(target: &AppDb, bundle: &ProjectionBundle) -> Result<usize> {
    let DatabaseBackend::Sqlite(pool) = target.backend() else {
        bail!("catalog replay projection target must be SQLite");
    };
    let mut transaction = pool.begin().await?;
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(&mut *transaction)
        .await?;
    let mut inserted = 0;
    for rows in &bundle.groups {
        for row in rows {
            if row.table == "aircraft_markets"
                && ensure_existing_market(&mut transaction, row).await?
            {
                continue;
            }
            insert_row_sqlite(&mut transaction, row).await?;
            inserted += 1;
        }
        if rows
            .first()
            .is_some_and(|row| row.table == "avionics_manufacturers")
        {
            reconcile_generated_rows(&mut transaction, &bundle.generated_keys).await?;
        }
        if rows
            .first()
            .is_some_and(|row| row.table == "avionics_model_types")
        {
            for identity in &bundle.generated_products {
                let id = identity.integer("avionics_model_id")?;
                let changed = sqlx::query(
                    "UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ? AND catalog_status = 'unreviewed'",
                )
                .bind(id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
                if changed != 1 {
                    bail!("could not promote staged avionics model {id}");
                }
            }
            reconcile_generated_rows(&mut transaction, &bundle.generated_products).await?;
        }
    }
    let violations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
        .fetch_one(&mut *transaction)
        .await?;
    if violations != 0 {
        bail!("catalog replay projection produced {violations} foreign-key violations");
    }
    transaction.commit().await?;
    Ok(inserted)
}

async fn insert_row_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    row: &ProjectionRow,
) -> Result<()> {
    let mut query =
        QueryBuilder::<Sqlite>::new(format!("INSERT INTO {} (", quoted_identifier(&row.table)));
    {
        let mut separated = query.separated(", ");
        for column in &row.columns {
            separated.push(quoted_identifier(column));
        }
    }
    query.push(") VALUES (");
    {
        let mut separated = query.separated(", ");
        for value in &row.values {
            match value {
                Value::Null => separated.push("NULL"),
                Value::Bool(value) => separated.push_bind(i64::from(*value)),
                Value::Number(number) if number.is_i64() => {
                    separated.push_bind(number.as_i64().unwrap())
                }
                Value::Number(number) => separated.push_bind(number.as_f64().unwrap()),
                Value::String(value) => separated.push_bind(value.clone()),
                Value::Array(_) | Value::Object(_) => {
                    separated.push_bind(serde_json::to_string(value)?)
                }
            };
        }
    }
    query.push(")");
    query
        .build()
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("could not project {} row {}", row.table, canonical_row(row)))?;
    Ok(())
}

async fn ensure_existing_market(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    expected: &ProjectionRow,
) -> Result<bool> {
    let predicate = primary_key_predicate(expected)?;
    let Some(actual) = fetch_rows(&mut *transaction, &expected.table, &predicate)
        .await?
        .into_iter()
        .next()
    else {
        return Ok(false);
    };
    for column in &expected.columns {
        if !matches!(column.as_str(), "created_at" | "updated_at")
            && expected.value(column) != actual.value(column)
        {
            bail!("canonical target market differs from the selected source market");
        }
    }
    reconcile_timestamps(transaction, expected, &actual).await?;
    Ok(true)
}

async fn reconcile_generated_rows(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    expected: &[ProjectionRow],
) -> Result<()> {
    for row in expected {
        let predicate = primary_key_predicate(row)?;
        let actual = fetch_rows(&mut *transaction, &row.table, &predicate)
            .await?
            .into_iter()
            .next()
            .with_context(|| format!("schema did not generate expected {} row", row.table))?;
        reconcile_timestamps(transaction, row, &actual).await?;
        let actual = fetch_rows(&mut *transaction, &row.table, &predicate).await?;
        if actual.as_slice() != [row.clone()] {
            bail!("schema-generated {} row differs from source", row.table);
        }
    }
    Ok(())
}

async fn reconcile_timestamps(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    expected: &ProjectionRow,
    actual: &ProjectionRow,
) -> Result<()> {
    let columns = ["created_at", "updated_at"]
        .into_iter()
        .filter(|column| expected.value(column) != actual.value(column))
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Ok(());
    }
    let mut query = QueryBuilder::<Sqlite>::new(format!(
        "UPDATE {} SET ",
        quoted_identifier(&expected.table)
    ));
    for (index, column) in columns.into_iter().enumerate() {
        if index > 0 {
            query.push(", ");
        }
        query.push(quoted_identifier(column)).push(" = ").push_bind(
            expected
                .value(column)
                .and_then(Value::as_str)
                .context("generated timestamp is not text")?
                .to_string(),
        );
    }
    query.push(" WHERE ").push(primary_key_predicate(expected)?);
    query.build().execute(&mut **transaction).await?;
    Ok(())
}

async fn validate_projected_target(target: &AppDb, bundle: &ProjectionBundle) -> Result<()> {
    let DatabaseBackend::Sqlite(pool) = target.backend() else {
        bail!("catalog replay projection target must be SQLite");
    };
    let mut connection = pool.acquire().await?;
    let expected = bundle.fingerprint_rows()?;
    let mut actual = Vec::with_capacity(expected.len());
    for row in &expected {
        let rows = fetch_rows(&mut connection, &row.table, &primary_key_predicate(row)?).await?;
        if rows.len() != 1 {
            bail!(
                "projected target has {} rows for selected {} key",
                rows.len(),
                row.table
            );
        }
        actual.push(rows.into_iter().next().unwrap());
    }
    actual.sort_by_key(canonical_row);
    let actual_fingerprint = fingerprint(&actual)?;
    if actual_fingerprint != bundle.fingerprint_sha256 {
        bail!(
            "projected target fingerprint {actual_fingerprint} differs from planned fingerprint {}",
            bundle.fingerprint_sha256
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::faa::bridge::FaaBridgeReport;
    use crate::avionics::manufacturer::ensure_test_manufacturer_identity;

    const ARCHIVE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const MANIFEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const RECORD: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn hierarchy_quote(snapshot_ids: &str, n_numbers: &str) -> String {
        format!(
            "Imported FAA ACFTREF reports manufacturer_name=\"CESSNA AIRCRAFT COMPANY\"; snapshot_ids={snapshot_ids}; snapshot_date=2026-07-21; archive_sha256={ARCHIVE}; source_manifest_sha256={MANIFEST}; source_record_sha256={RECORD}; n_numbers={n_numbers}; aircraft_code=2072738; observations=11:dddd:\"Cessna\":\"182\":\"182T\":2004; case_token=case-11"
        )
    }

    fn hierarchy_fact() -> LegacyHierarchyFactRow {
        LegacyHierarchyFactRow {
            snapshot_id: 4,
            n_number: "N123AB".into(),
            aircraft_code: "2072738".into(),
            manufacturer_name: Some("CESSNA AIRCRAFT COMPANY".into()),
            model_name: Some("182T".into()),
            source_record_sha256: RECORD.into(),
        }
    }

    #[test]
    fn parses_and_authenticates_exact_server_faa_hierarchy_receipt() {
        let parsed = parse_server_faa_hierarchy_claim(
            ServerFaaHierarchyFactKind::Make,
            &hierarchy_quote("4", "N123AB"),
            19,
        )
        .unwrap();
        let facts = [hierarchy_fact()];
        validate_legacy_hierarchy_facts(&parsed, &facts, 19).unwrap();
        let evidence_id = server_faa_hierarchy_evidence_id(
            ServerFaaHierarchyFactKind::Make,
            "CESSNA AIRCRAFT COMPANY",
            &parsed.provenance,
        )
        .unwrap();
        validate_server_faa_hierarchy_evidence_id(&evidence_id, &parsed, &facts, 19).unwrap();
        assert_eq!(parsed.snapshot_ids, BTreeSet::from([4]));
        assert_eq!(parsed.n_numbers, BTreeSet::from(["N123AB".to_string()]));
        assert_eq!(
            parsed.source_record_sha256s,
            BTreeSet::from([RECORD.into()])
        );
    }

    #[test]
    fn rejects_mutated_or_noncanonical_server_faa_hierarchy_receipt() {
        let parsed = parse_server_faa_hierarchy_claim(
            ServerFaaHierarchyFactKind::Make,
            &hierarchy_quote("4", "N123AB"),
            20,
        )
        .unwrap();
        let facts = [hierarchy_fact()];
        let wrong_id = format!("server_faa_registry.make.{}", "d".repeat(64));
        assert!(
            validate_server_faa_hierarchy_evidence_id(&wrong_id, &parsed, &facts, 20)
                .unwrap_err()
                .to_string()
                .contains("does not authenticate")
        );

        let error = parse_server_faa_hierarchy_claim(
            ServerFaaHierarchyFactKind::Make,
            &hierarchy_quote("5,4", "N123AB"),
            21,
        )
        .err()
        .expect("unsorted snapshot set must fail");
        assert!(error.to_string().contains("non-canonical snapshot_ids"));
    }

    #[tokio::test]
    async fn projects_an_approved_avionics_product_into_a_fresh_current_schema() {
        let directory = tempfile::tempdir().unwrap();
        let source_url = format!(
            "sqlite://{}",
            directory.path().join("source.sqlite3").display()
        );
        let target_url = format!(
            "sqlite://{}",
            directory.path().join("target.sqlite3").display()
        );
        let source = AppDb::connect(&source_url).await.unwrap();
        let target = AppDb::connect(&target_url).await.unwrap();
        let DatabaseBackend::Sqlite(source_pool) = source.backend() else {
            panic!("test source must be SQLite")
        };
        let manufacturer_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Test Avionics', 'test avionics') RETURNING id",
        )
        .fetch_one(source_pool)
        .await
        .unwrap();
        ensure_test_manufacturer_identity(&source, manufacturer_id)
            .await
            .unwrap();
        let type_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Navigator', 'navigator') RETURNING id",
        )
        .fetch_one(source_pool)
        .await
        .unwrap();
        let model_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO avionics_models (
                 avionics_manufacturer_id, name, normalized_name,
                 manufacturer_identifier_kind, manufacturer_identifier,
                 normalized_manufacturer_identifier, identity_source_url,
                 identity_source_title, identity_evidence_text,
                 identity_evidence_kind, identity_confidence, catalog_reviewed_at
               ) VALUES (?, 'NAV 1', 'nav 1', 'manufacturer_model_number',
                         'NAV-1', 'nav1', 'https://manufacturer.example/nav-1',
                         'NAV 1 product page',
                         'The manufacturer identifies NAV 1 as its exact product.',
                         'authoritative_reference', 'very_high', CURRENT_TIMESTAMP)
               RETURNING id"#,
        )
        .bind(manufacturer_id)
        .fetch_one(source_pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(model_id)
        .bind(type_id)
        .execute(source_pool)
        .await
        .unwrap();
        sqlx::query("UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?")
            .bind(model_id)
            .execute(source_pool)
            .await
            .unwrap();

        let faa = FaaBridgeOutcome {
            report: FaaBridgeReport {
                archive_sha256: ARCHIVE.into(),
                snapshot_date: "2026-07-21".into(),
                target_count: 0,
                matched_count: 0,
                stored_snapshot_id: 1,
            },
            representative_remap: BTreeMap::new(),
            obsolete_hash_replacements: BTreeMap::new(),
            obsolete_hashes: BTreeSet::new(),
            legacy_evidence_source_ids: BTreeSet::new(),
            current_evidence_source_id: 1,
        };
        let mut source_connection = source_pool.acquire().await.unwrap();
        let report = project_reusable_catalog(&mut source_connection, &target, &faa)
            .await
            .unwrap();
        assert!(!report.fingerprint_sha256.is_empty());
        let DatabaseBackend::Sqlite(target_pool) = target.backend() else {
            panic!("test target must be SQLite")
        };
        let projected: (String, String, String) = sqlx::query_as(
            r#"SELECT manufacturer.name, model.name, model.catalog_status
               FROM avionics_models model
               JOIN avionics_manufacturers manufacturer
                 ON manufacturer.id = model.avionics_manufacturer_id
               WHERE model.id = ?"#,
        )
        .bind(model_id)
        .fetch_one(target_pool)
        .await
        .unwrap();
        assert_eq!(
            projected,
            ("Test Avionics".into(), "NAV 1".into(), "approved".into())
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM gemini_api_usage")
                .fetch_one(target_pool)
                .await
                .unwrap(),
            0
        );
        drop(source_connection);
        source.close().await;
        target.close().await;
    }
}
