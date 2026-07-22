use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, PgPool, SqlitePool};

use crate::depreciation::default_avionics_profile;
use crate::models::User;

pub const DEFAULT_DATABASE_PATH: &str = "data/aircost.sqlite3";
pub const DEFAULT_DATABASE_URL: &str = "sqlite://data/aircost.sqlite3";
pub const DEVELOPER_EMAIL: &str = "developer@localhost";
const DEVELOPER_AUTH_SUBJECT: &str = "developer";
const SQLITE_SCHEMA_SQL: &str = include_str!("../schema/sqlite.sql");
const POSTGRES_SCHEMA_SQL: &str = include_str!("../schema/postgres.sql");
const VALUATION_DATA_HARDENING_MIGRATION: &str = "20260720_valuation_data_hardening";
const AVIONICS_CATALOG_CURATION_MIGRATION: &str = "20260721_avionics_catalog_curation";
const AVIONICS_MULTI_TYPE_MIGRATION: &str = "20260721_avionics_multi_type";
const AIRCRAFT_REFERENCE_CATALOG_MIGRATION: &str = "20260722_aircraft_reference_catalog";

#[derive(Clone)]
pub struct AppDb {
    backend: DatabaseBackend,
}

#[derive(Clone)]
pub(crate) enum DatabaseBackend {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DatabaseKind {
    Sqlite,
    Postgres,
}

impl AppDb {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let database_url = normalize_database_url(database_url);
        if is_postgres_url(&database_url) {
            let pool = PgPoolOptions::new()
                .max_connections(5)
                .connect(&database_url)
                .await
                .with_context(|| {
                    format!("could not connect to Postgres database {database_url}")
                })?;
            let db = Self {
                backend: DatabaseBackend::Postgres(pool),
            };
            db.ensure_required_migrations().await?;
            db.initialize().await?;
            Ok(db)
        } else {
            ensure_sqlite_parent_directory(&database_url)?;
            let options = SqliteConnectOptions::from_str(&database_url)
                .with_context(|| format!("invalid SQLite database URL {database_url}"))?
                .create_if_missing(true)
                .foreign_keys(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(5)
                .connect_with(options)
                .await
                .with_context(|| format!("could not connect to SQLite database {database_url}"))?;
            let db = Self {
                backend: DatabaseBackend::Sqlite(pool),
            };
            db.ensure_required_migrations().await?;
            db.initialize().await?;
            Ok(db)
        }
    }

    pub(crate) fn backend(&self) -> &DatabaseBackend {
        &self.backend
    }

    pub(crate) fn kind(&self) -> DatabaseKind {
        match self.backend {
            DatabaseBackend::Sqlite(_) => DatabaseKind::Sqlite,
            DatabaseBackend::Postgres(_) => DatabaseKind::Postgres,
        }
    }

    pub(crate) fn sql<'a>(&self, sqlite_sql: &'a str) -> Cow<'a, str> {
        match self.kind() {
            DatabaseKind::Sqlite => Cow::Borrowed(sqlite_sql),
            DatabaseKind::Postgres => Cow::Owned(postgres_placeholders(sqlite_sql)),
        }
    }

    pub async fn current_user(&self, identity: Option<&str>) -> Result<User> {
        let identity = identity.unwrap_or(DEVELOPER_EMAIL);
        let sql = self.sql(
            r#"
            SELECT id, email, display_name, auth_provider, auth_subject
            FROM users
            WHERE email = ? OR auth_subject = ?
            "#,
        );
        let user = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, User>(&sql)
                    .bind(identity)
                    .bind(identity)
                    .fetch_optional(pool)
                    .await?
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, User>(&sql)
                    .bind(identity)
                    .bind(identity)
                    .fetch_optional(pool)
                    .await?
            }
        };
        user.with_context(|| format!("unknown user: {identity}"))
    }

    async fn ensure_required_migrations(&self) -> Result<()> {
        let missing_valuation_hardening = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                SELECT
                  EXISTS (
                    SELECT 1
                    FROM sqlite_schema
                    WHERE type = 'table' AND name = 'aircraft_sale_listings'
                  )
                  AND NOT EXISTS (
                    SELECT 1
                    FROM pragma_table_info('aircraft_sale_listings')
                    WHERE name = 'ingestion_state'
                  )
                "#,
                )
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                SELECT
                  to_regclass('aircraft_sale_listings') IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1
                    FROM pg_attribute
                    WHERE attrelid = to_regclass('aircraft_sale_listings')
                      AND attname = 'ingestion_state'
                      AND NOT attisdropped
                  )
                "#,
                )
                .fetch_one(pool)
                .await?
            }
        };
        if missing_valuation_hardening {
            bail!(migration_required_message(
                self.kind(),
                "aircraft_sale_listings",
                "ingestion_state",
                VALUATION_DATA_HARDENING_MIGRATION,
            ));
        }

        let missing_avionics_curation = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                SELECT
                  EXISTS (
                    SELECT 1
                    FROM sqlite_schema
                    WHERE type = 'table' AND name = 'avionics_models'
                  )
                  AND NOT EXISTS (
                    SELECT 1
                    FROM pragma_table_info('avionics_models')
                    WHERE name = 'catalog_status'
                  )
                "#,
                )
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                SELECT
                  to_regclass('avionics_models') IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1
                    FROM pg_attribute
                    WHERE attrelid = to_regclass('avionics_models')
                      AND attname = 'catalog_status'
                      AND NOT attisdropped
                  )
                "#,
                )
                .fetch_one(pool)
                .await?
            }
        };
        if missing_avionics_curation {
            bail!(migration_required_message(
                self.kind(),
                "avionics_models",
                "catalog_status",
                AVIONICS_CATALOG_CURATION_MIGRATION,
            ));
        }

        let missing_avionics_multi_type = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                SELECT
                  EXISTS (
                    SELECT 1
                    FROM sqlite_schema
                    WHERE type = 'table' AND name = 'avionics_models'
                  )
                  AND (
                    NOT EXISTS (
                      SELECT 1
                      FROM sqlite_schema
                      WHERE type = 'table' AND name = 'avionics_model_types'
                    )
                    OR EXISTS (
                      SELECT 1
                      FROM pragma_table_info('avionics_models')
                      WHERE name = 'avionics_type_id'
                    )
                  )
                "#,
                )
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                SELECT
                  to_regclass('avionics_models') IS NOT NULL
                  AND (
                    to_regclass('avionics_model_types') IS NULL
                    OR EXISTS (
                      SELECT 1
                      FROM pg_attribute
                      WHERE attrelid = to_regclass('avionics_models')
                        AND attname = 'avionics_type_id'
                        AND NOT attisdropped
                    )
                  )
                "#,
                )
                .fetch_one(pool)
                .await?
            }
        };
        if missing_avionics_multi_type {
            bail!(avionics_multi_type_migration_required_message(self.kind()));
        }

        let missing_aircraft_reference_catalog = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table' AND name = 'aircraft_sale_listings'
                      )
                      AND (
                        NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'aircraft_identity_observations'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'aircraft_engine_catalog_models'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'aircraft_propeller_catalog_models'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'faa_registry_snapshots'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'faa_registry_aircraft'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'faa_registry_aircraft_references'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'faa_registry_engine_references'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'faa_registry_coverage'
                        )
                      )
                    "#,
                )
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT
                      to_regclass('aircraft_sale_listings') IS NOT NULL
                      AND (
                        to_regclass('aircraft_identity_observations') IS NULL
                        OR to_regclass('aircraft_engine_catalog_models') IS NULL
                        OR to_regclass('aircraft_propeller_catalog_models') IS NULL
                        OR to_regclass('faa_registry_snapshots') IS NULL
                        OR to_regclass('faa_registry_aircraft') IS NULL
                        OR to_regclass('faa_registry_aircraft_references') IS NULL
                        OR to_regclass('faa_registry_engine_references') IS NULL
                        OR to_regclass('faa_registry_coverage') IS NULL
                      )
                    "#,
                )
                .fetch_one(pool)
                .await?
            }
        };
        if missing_aircraft_reference_catalog {
            bail!(aircraft_reference_catalog_migration_required_message(
                self.kind()
            ));
        }
        Ok(())
    }

    async fn initialize(&self) -> Result<()> {
        match self.backend() {
            DatabaseBackend::Sqlite(pool) => execute_statements(pool, SQLITE_SCHEMA_SQL).await?,
            DatabaseBackend::Postgres(pool) => {
                execute_statements(pool, POSTGRES_SCHEMA_SQL).await?
            }
        }
        self.seed_developer_user().await?;
        self.seed_depreciation_profile().await?;
        self.seed_component_depreciation_profiles().await?;
        Ok(())
    }

    async fn seed_developer_user(&self) -> Result<()> {
        let sql = self.sql(
            r#"
            INSERT INTO users (
              email,
              display_name,
              auth_provider,
              auth_subject
            )
            VALUES (?, ?, ?, ?)
            ON CONFLICT (auth_subject) DO NOTHING
            "#,
        );
        match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query(&sql)
                    .bind(DEVELOPER_EMAIL)
                    .bind("Developer")
                    .bind("local")
                    .bind(DEVELOPER_AUTH_SUBJECT)
                    .execute(pool)
                    .await?;
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query(&sql)
                    .bind(DEVELOPER_EMAIL)
                    .bind("Developer")
                    .bind("local")
                    .bind(DEVELOPER_AUTH_SUBJECT)
                    .execute(pool)
                    .await?;
            }
        }
        Ok(())
    }

    async fn seed_depreciation_profile(&self) -> Result<()> {
        let profile = crate::depreciation::AircraftProfile {
            name: "generic:all".to_string(),
            age_decay_rate: 0.05,
            long_run_residual_fraction: 0.28,
            new_to_used_discount_fraction: 0.08,
            new_to_used_discount_years: 1.0,
            airframe_doubling_discount: 0.15,
            max_airframe_premium: 0.12,
            max_airframe_discount: 0.30,
            replacement_floor_fraction: 0.0,
            minimum_value_fraction: 0.05,
            high_time_threshold_hours: Some(10_000.0),
            high_time_discount_at_double_threshold: 0.12,
        };
        for profile in [profile] {
            let sql = self.sql(
                r#"
                INSERT INTO depreciation_profiles (
                  name,
                  age_decay_rate,
                  long_run_residual_fraction,
                  new_to_used_discount_fraction,
                  new_to_used_discount_years,
                  airframe_doubling_discount,
                  max_airframe_premium,
                  max_airframe_discount,
                  replacement_floor_fraction,
                  minimum_value_fraction,
                  high_time_threshold_hours,
                  high_time_discount_at_double_threshold,
                  is_system_profile
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (name) DO NOTHING
                "#,
            );
            match self.backend() {
                DatabaseBackend::Sqlite(pool) => {
                    sqlx::query(&sql)
                        .bind(profile.name.as_str())
                        .bind(profile.age_decay_rate)
                        .bind(profile.long_run_residual_fraction)
                        .bind(profile.new_to_used_discount_fraction)
                        .bind(profile.new_to_used_discount_years)
                        .bind(profile.airframe_doubling_discount)
                        .bind(profile.max_airframe_premium)
                        .bind(profile.max_airframe_discount)
                        .bind(profile.replacement_floor_fraction)
                        .bind(profile.minimum_value_fraction)
                        .bind(profile.high_time_threshold_hours)
                        .bind(profile.high_time_discount_at_double_threshold)
                        .bind(true)
                        .execute(pool)
                        .await?;
                }
                DatabaseBackend::Postgres(pool) => {
                    sqlx::query(&sql)
                        .bind(profile.name.as_str())
                        .bind(profile.age_decay_rate)
                        .bind(profile.long_run_residual_fraction)
                        .bind(profile.new_to_used_discount_fraction)
                        .bind(profile.new_to_used_discount_years)
                        .bind(profile.airframe_doubling_discount)
                        .bind(profile.max_airframe_premium)
                        .bind(profile.max_airframe_discount)
                        .bind(profile.replacement_floor_fraction)
                        .bind(profile.minimum_value_fraction)
                        .bind(profile.high_time_threshold_hours)
                        .bind(profile.high_time_discount_at_double_threshold)
                        .bind(true)
                        .execute(pool)
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn seed_component_depreciation_profiles(&self) -> Result<()> {
        let avionics = default_avionics_profile();
        let rows = [
            ("engine", None, None, Some(0.5)),
            ("propeller", None, None, Some(0.5)),
            (
                "avionics",
                Some(avionics.age_decay_rate),
                Some(avionics.long_run_residual_fraction),
                None,
            ),
        ];

        for (component_type, age_decay_rate, long_run_residual_fraction, baseline_life_fraction) in
            rows
        {
            let sql = self.sql(
                r#"
                INSERT INTO component_depreciation_profiles (
                  component_type,
                  age_decay_rate,
                  long_run_residual_fraction,
                  baseline_life_fraction
                )
                VALUES (?, ?, ?, ?)
                ON CONFLICT (component_type) DO NOTHING
                "#,
            );
            match self.backend() {
                DatabaseBackend::Sqlite(pool) => {
                    sqlx::query(&sql)
                        .bind(component_type)
                        .bind(age_decay_rate)
                        .bind(long_run_residual_fraction)
                        .bind(baseline_life_fraction)
                        .execute(pool)
                        .await?;
                }
                DatabaseBackend::Postgres(pool) => {
                    sqlx::query(&sql)
                        .bind(component_type)
                        .bind(age_decay_rate)
                        .bind(long_run_residual_fraction)
                        .bind(baseline_life_fraction)
                        .execute(pool)
                        .await?;
                }
            }
        }
        Ok(())
    }
}

pub fn database_url_from_arg(value: Option<String>) -> String {
    value
        .map(|value| {
            if is_database_url(&value) {
                value
            } else {
                sqlite_url_for_path(PathBuf::from(value))
            }
        })
        .unwrap_or_else(|| {
            std::env::var("AIRCOST_DATABASE_URL")
                .unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
        })
}

fn normalize_database_url(value: &str) -> String {
    if is_database_url(value) {
        value.to_string()
    } else {
        sqlite_url_for_path(PathBuf::from(value))
    }
}

fn sqlite_url_for_path(path: PathBuf) -> String {
    if path == Path::new(":memory:") {
        "sqlite::memory:".to_string()
    } else {
        format!("sqlite://{}", path.to_string_lossy())
    }
}

fn is_database_url(value: &str) -> bool {
    value.starts_with("sqlite:")
        || value.starts_with("postgres:")
        || value.starts_with("postgresql:")
}

fn is_postgres_url(value: &str) -> bool {
    value.starts_with("postgres:") || value.starts_with("postgresql:")
}

fn ensure_sqlite_parent_directory(database_url: &str) -> Result<()> {
    if database_url == "sqlite::memory:" {
        return Ok(());
    }
    let Some(path) = database_url.strip_prefix("sqlite://") else {
        return Ok(());
    };
    let path = Path::new(path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create database directory {parent:?}"))?;
    }
    Ok(())
}

async fn execute_statements<'a, E>(executor: E, sql: &str) -> Result<()>
where
    E: Executor<'a> + Copy,
{
    for statement in split_sql_statements(sql) {
        executor.execute(statement).await?;
    }
    Ok(())
}

/// Split the checked-in schema files without breaking trigger bodies, quoted
/// strings, or PostgreSQL dollar-quoted function definitions.
fn split_sql_statements(sql: &str) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut index = 0;
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut dollar_quote: Option<String> = None;

    while index < bytes.len() {
        if line_comment {
            if bytes[index] == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(delimiter) = dollar_quote.as_deref() {
            if bytes[index..].starts_with(delimiter.as_bytes()) {
                index += delimiter.len();
                dollar_quote = None;
            } else {
                index += 1;
            }
            continue;
        }
        if single_quoted {
            if bytes[index] == b'\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                } else {
                    single_quoted = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }
        if double_quoted {
            if bytes[index] == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    index += 2;
                } else {
                    double_quoted = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }

        if bytes[index] == b'-' && bytes.get(index + 1) == Some(&b'-') {
            line_comment = true;
            index += 2;
            continue;
        }
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            block_comment = true;
            index += 2;
            continue;
        }
        if bytes[index] == b'\'' {
            single_quoted = true;
            index += 1;
            continue;
        }
        if bytes[index] == b'"' {
            double_quoted = true;
            index += 1;
            continue;
        }
        if bytes[index] == b'$' {
            if let Some(delimiter) = dollar_quote_delimiter(&sql[index..]) {
                index += delimiter.len();
                dollar_quote = Some(delimiter.to_string());
                continue;
            }
        }
        if bytes[index] == b';' {
            let candidate = sql[start..index].trim();
            if !candidate.is_empty() && !sqlite_trigger_body_is_open(candidate) {
                statements.push(candidate);
                start = index + 1;
            }
        }
        index += 1;
    }

    let trailing = sql[start..].trim();
    if !trailing.is_empty() {
        statements.push(trailing);
    }
    statements
}

fn dollar_quote_delimiter(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'$') {
        return None;
    }
    let end = bytes[1..].iter().position(|byte| *byte == b'$')? + 1;
    if bytes[1..end]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        Some(&value[..=end])
    } else {
        None
    }
}

fn sqlite_trigger_body_is_open(statement: &str) -> bool {
    let statement = strip_leading_sql_comments(statement);
    let uppercase = statement.to_ascii_uppercase();
    let mut words = uppercase
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|word| !word.is_empty());
    if words.next() != Some("CREATE") {
        return false;
    }
    let second = words.next();
    let trigger = if second == Some("TEMP") || second == Some("TEMPORARY") {
        words.next()
    } else {
        second
    };
    trigger == Some("TRIGGER")
        && words.any(|word| word == "BEGIN")
        && !uppercase.trim_end().ends_with("END")
}

fn strip_leading_sql_comments(mut value: &str) -> &str {
    loop {
        value = value.trim_start();
        if let Some(line_comment) = value.strip_prefix("--") {
            value = line_comment
                .find('\n')
                .map(|newline| &line_comment[newline + 1..])
                .unwrap_or("");
            continue;
        }
        if let Some(block_comment) = value.strip_prefix("/*") {
            value = block_comment
                .find("*/")
                .map(|end| &block_comment[end + 2..])
                .unwrap_or("");
            continue;
        }
        return value;
    }
}

fn postgres_placeholders(sql: &str) -> String {
    let mut next_placeholder = 1_usize;
    let mut converted = String::with_capacity(sql.len());
    for character in sql.chars() {
        if character == '?' {
            converted.push('$');
            converted.push_str(&next_placeholder.to_string());
            next_placeholder += 1;
        } else {
            converted.push(character);
        }
    }
    converted
}

fn migration_required_message(
    kind: DatabaseKind,
    table: &str,
    column: &str,
    migration: &str,
) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing `{table}` is missing `{column}`; \
         back up the database, apply `migrations/{migration}.{backend}.sql`, then restart aircost"
    )
}

fn avionics_multi_type_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing avionics catalog must use the \
         `avionics_model_types` capability table without scalar `avionics_models.avionics_type_id`; \
         back up the database, apply `migrations/{AVIONICS_MULTI_TYPE_MIGRATION}.{backend}.sql`, \
         then restart aircost"
    )
}

fn aircraft_reference_catalog_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing aircraft data is missing the clean \
         aircraft identity/reference catalogs or FAA registry projection; back up the \
         database, apply `migrations/{AIRCRAFT_REFERENCE_CATALOG_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

pub fn ensure_supported_database_url(database_url: &str) -> Result<()> {
    if is_database_url(database_url) || !database_url.trim().is_empty() {
        Ok(())
    } else {
        bail!("database URL cannot be empty")
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::Executor;

    use super::{
        aircraft_reference_catalog_migration_required_message,
        avionics_multi_type_migration_required_message, migration_required_message,
        split_sql_statements, AppDb, DatabaseBackend, DatabaseKind,
        AVIONICS_CATALOG_CURATION_MIGRATION, VALUATION_DATA_HARDENING_MIGRATION,
    };

    async fn sqlite_db_with_statements(statements: &[&str]) -> AppDb {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("SQLite test database should connect");
        for statement in statements {
            pool.execute(*statement)
                .await
                .expect("legacy test schema should be created");
        }
        AppDb {
            backend: DatabaseBackend::Sqlite(pool),
        }
    }

    #[test]
    fn schema_splitter_preserves_sqlite_trigger_bodies() {
        let sql = "CREATE TABLE example (id INTEGER);\n\
                   CREATE TRIGGER example_guard BEFORE INSERT ON example\n\
                   BEGIN\n\
                     SELECT RAISE(ABORT, 'invalid; value');\n\
                   END;\n\
                   CREATE INDEX example_id ON example (id);";
        let statements = split_sql_statements(sql);
        assert_eq!(statements.len(), 3);
        assert!(statements[1].contains("SELECT RAISE"));
        assert!(statements[1].ends_with("END"));
    }

    #[test]
    fn schema_splitter_preserves_commented_sqlite_trigger_bodies() {
        let sql = "CREATE TABLE example (id INTEGER);\n\
                   -- Approval is staged before this trigger.\n\
                   /* Keep the trigger body in one statement. */\n\
                   CREATE TRIGGER example_guard BEFORE INSERT ON example\n\
                   BEGIN\n\
                     SELECT RAISE(ABORT, 'invalid; value');\n\
                   END;\n\
                   CREATE INDEX example_id ON example (id);";
        let statements = split_sql_statements(sql);
        assert_eq!(statements.len(), 3);
        assert!(statements[1].contains("CREATE TRIGGER"));
        assert!(statements[1].contains("SELECT RAISE"));
        assert!(statements[1].ends_with("END"));
    }

    #[test]
    fn schema_splitter_preserves_postgres_function_bodies() {
        let sql = "CREATE OR REPLACE FUNCTION guard() RETURNS TRIGGER\n\
                   LANGUAGE plpgsql AS $function$\n\
                   BEGIN\n\
                     RAISE EXCEPTION 'invalid; value';\n\
                     RETURN NEW;\n\
                   END;\n\
                   $function$;\n\
                   CREATE TRIGGER guard_insert BEFORE INSERT ON example\n\
                   FOR EACH ROW EXECUTE FUNCTION guard();";
        let statements = split_sql_statements(sql);
        assert_eq!(statements.len(), 2);
        assert!(statements[0].contains("RETURN NEW;"));
        assert!(statements[1].starts_with("CREATE TRIGGER"));
    }

    #[tokio::test]
    async fn legacy_listing_schema_requires_valuation_hardening_first() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY)",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("legacy listing schema must fail preflight")
            .to_string();
        assert!(error.contains("`aircraft_sale_listings` is missing `ingestion_state`"));
        assert!(error.contains("migrations/20260720_valuation_data_hardening.sqlite.sql"));
    }

    #[tokio::test]
    async fn hardened_listing_with_legacy_avionics_requires_catalog_migration() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY, ingestion_state TEXT)",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("legacy avionics schema must fail preflight")
            .to_string();
        assert!(error.contains("`avionics_models` is missing `catalog_status`"));
        assert!(error.contains("migrations/20260721_avionics_catalog_curation.sqlite.sql"));
    }

    #[tokio::test]
    async fn curated_catalog_requires_join_only_multi_type_migration() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY, ingestion_state TEXT)",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY, catalog_status TEXT, avionics_type_id INTEGER)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("scalar avionics catalog must fail preflight")
            .to_string();
        assert!(error.contains("`avionics_model_types` capability table"));
        assert!(error.contains("without scalar `avionics_models.avionics_type_id`"));
        assert!(error.contains("migrations/20260721_avionics_multi_type.sqlite.sql"));
    }

    #[tokio::test]
    async fn join_only_avionics_catalog_passes_migration_preflight() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY, ingestion_state TEXT)",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY, catalog_status TEXT)",
            "CREATE TABLE avionics_model_types (avionics_model_id INTEGER, avionics_type_id INTEGER)",
            "CREATE TABLE aircraft_identity_observations (id INTEGER PRIMARY KEY)",
            "CREATE TABLE aircraft_engine_catalog_models (id INTEGER PRIMARY KEY)",
            "CREATE TABLE aircraft_propeller_catalog_models (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_snapshots (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_aircraft (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_aircraft_references (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_engine_references (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_coverage (id INTEGER PRIMARY KEY)",
        ])
        .await;
        db.ensure_required_migrations()
            .await
            .expect("join-only catalog should pass preflight");
    }

    #[tokio::test]
    async fn existing_database_requires_clean_aircraft_reference_catalog() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY, ingestion_state TEXT)",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY, catalog_status TEXT)",
            "CREATE TABLE avionics_model_types (avionics_model_id INTEGER, avionics_type_id INTEGER)",
            "CREATE TABLE engine_models (id INTEGER PRIMARY KEY)",
            "CREATE TABLE propeller_models (id INTEGER PRIMARY KEY)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("legacy aircraft reference storage must fail preflight")
            .to_string();
        assert!(error.contains("clean aircraft identity/reference catalog"));
        assert!(error.contains("20260722_aircraft_reference_catalog.sqlite.sql"));
    }

    #[tokio::test]
    async fn empty_database_passes_preflight_and_initializes_fresh_schema() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("fresh database should initialize");
        db.ensure_required_migrations()
            .await
            .expect("fresh schema should pass subsequent preflight");
    }

    #[test]
    fn migration_messages_select_the_backend_specific_script() {
        let sqlite = migration_required_message(
            DatabaseKind::Sqlite,
            "aircraft_sale_listings",
            "ingestion_state",
            VALUATION_DATA_HARDENING_MIGRATION,
        );
        assert!(sqlite.contains("20260720_valuation_data_hardening.sqlite.sql"));

        let postgres = migration_required_message(
            DatabaseKind::Postgres,
            "avionics_models",
            "catalog_status",
            AVIONICS_CATALOG_CURATION_MIGRATION,
        );
        assert!(postgres.contains("20260721_avionics_catalog_curation.postgres.sql"));

        let multi_type = avionics_multi_type_migration_required_message(DatabaseKind::Postgres);
        assert!(multi_type.contains("20260721_avionics_multi_type.postgres.sql"));

        let aircraft_reference =
            aircraft_reference_catalog_migration_required_message(DatabaseKind::Sqlite);
        assert!(aircraft_reference.contains("20260722_aircraft_reference_catalog.sqlite.sql"));
    }
}
