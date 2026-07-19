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
const SQLITE_SCHEMA_SQL: &str = include_str!("../aircost/webapp/schema.sql");
const POSTGRES_SCHEMA_SQL: &str = include_str!("../aircost/webapp/schema.postgres.sql");

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
    for statement in sql.split(';').map(str::trim).filter(|sql| !sql.is_empty()) {
        executor.execute(statement).await?;
    }
    Ok(())
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

pub fn ensure_supported_database_url(database_url: &str) -> Result<()> {
    if is_database_url(database_url) || !database_url.trim().is_empty() {
        Ok(())
    } else {
        bail!("database URL cannot be empty")
    }
}
