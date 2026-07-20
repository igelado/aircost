"""Database setup for the aircraft listings web application."""

from __future__ import annotations

import sqlite3
from pathlib import Path

from aircost.depreciation import PROFILES


DEFAULT_DATABASE_PATH = Path("data/aircost.sqlite3")
DEVELOPER_EMAIL = "developer@localhost"
DEVELOPER_AUTH_SUBJECT = "developer"


def connect_database(path: str | Path = DEFAULT_DATABASE_PATH) -> sqlite3.Connection:
    """Open a SQLite connection configured for the web app."""

    database_path = Path(path)
    if str(database_path) != ":memory:":
        database_path.parent.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(database_path)
    connection.row_factory = sqlite3.Row
    connection.execute("PRAGMA foreign_keys = ON")
    return connection


def initialize_database(path: str | Path = DEFAULT_DATABASE_PATH) -> None:
    """Create tables and seed base records."""

    with connect_database(path) as connection:
        initialize_connection(connection)


def initialize_connection(connection: sqlite3.Connection) -> None:
    """Create tables and seed base records on an existing connection."""

    schema_path = Path(__file__).with_name("schema.sql")
    _migrate_aircraft_sale_listings(connection)
    connection.executescript(schema_path.read_text(encoding="utf-8"))
    _migrate_aircraft_sale_listings(connection)
    _seed_developer_user(connection)
    _seed_depreciation_profiles(connection)
    connection.commit()


def row_to_dict(row: sqlite3.Row) -> dict:
    """Convert a SQLite row into a plain JSON-serializable dictionary."""

    return {key: row[key] for key in row.keys()}


def current_timestamp_sql() -> str:
    """Return the SQL expression used for application-managed update timestamps."""

    return "CURRENT_TIMESTAMP"


def _migrate_aircraft_sale_listings(connection: sqlite3.Connection) -> None:
    """Apply small additive/rename migrations for pre-CRUD local databases."""

    columns = _table_columns(connection, "aircraft_sale_listings")
    if not columns:
        return

    if "is_valid" in columns and "is_verified" not in columns:
        connection.execute(
            "ALTER TABLE aircraft_sale_listings RENAME COLUMN is_valid TO is_verified"
        )
        columns = _table_columns(connection, "aircraft_sale_listings")

    if "added_at" not in columns:
        connection.execute("ALTER TABLE aircraft_sale_listings ADD COLUMN added_at TEXT")
        columns = _table_columns(connection, "aircraft_sale_listings")

    timestamp_source = "CURRENT_TIMESTAMP"
    if "observed_at" in columns:
        timestamp_source = "observed_at"
    elif "verified_at" in columns:
        timestamp_source = "verified_at"
    elif "created_at" in columns:
        timestamp_source = "created_at"
    connection.execute(
        f"""
        UPDATE aircraft_sale_listings
        SET added_at = COALESCE(added_at, {timestamp_source}, CURRENT_TIMESTAMP)
        WHERE added_at IS NULL
        """
    )

    if "verified_at" in columns and "is_verified" in columns:
        connection.execute(
            """
            UPDATE aircraft_sale_listings
            SET is_verified = CASE
              WHEN verified_at IS NOT NULL THEN 1
              ELSE 0
            END
            """
        )

    connection.execute(
        """
        CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_variant
          ON aircraft_sale_listings (aircraft_variant_id, is_verified, added_at)
        """
    )


def _table_columns(connection: sqlite3.Connection, table_name: str) -> set[str]:
    rows = connection.execute(f"PRAGMA table_info({table_name})").fetchall()
    return {row["name"] for row in rows}


def _seed_developer_user(connection: sqlite3.Connection) -> None:
    connection.execute(
        """
        INSERT OR IGNORE INTO users (
          email,
          display_name,
          auth_provider,
          auth_subject
        )
        VALUES (?, ?, ?, ?)
        """,
        (
            DEVELOPER_EMAIL,
            "Developer",
            "local",
            DEVELOPER_AUTH_SUBJECT,
        ),
    )


def _seed_depreciation_profiles(connection: sqlite3.Connection) -> None:
    for profile in PROFILES.values():
        connection.execute(
            """
            INSERT OR IGNORE INTO depreciation_profiles (
              name,
              age_decay_rate,
              long_run_residual_fraction,
              new_to_used_discount_fraction,
              new_to_used_discount_years,
              annual_airframe_hours,
              airframe_doubling_discount,
              max_airframe_premium,
              max_airframe_discount,
              minimum_value_fraction,
              high_time_threshold_hours,
              high_time_discount_at_double_threshold,
              is_system_profile
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)
            """,
            (
                profile.name,
                profile.age_decay_rate,
                profile.long_run_residual_fraction,
                profile.new_to_used_discount_fraction,
                profile.new_to_used_discount_years,
                profile.annual_airframe_hours,
                profile.airframe_doubling_discount,
                profile.max_airframe_premium,
                profile.max_airframe_discount,
                profile.minimum_value_fraction,
                profile.high_time_threshold_hours,
                profile.high_time_discount_at_double_threshold,
            ),
        )
