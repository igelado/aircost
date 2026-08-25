//! Deterministic projection and transactional seeding of current verified
//! catalog truth.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, SqliteConnection};

pub(crate) mod current;
pub(crate) mod seed;

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
