"""Persistence helpers for aircraft sale listings."""

from __future__ import annotations

import sqlite3
from dataclasses import asdict
from typing import Any

from .database import current_timestamp_sql, row_to_dict
from .listing_parser import ListingPreview, normalize_name


class ListingStoreError(Exception):
    """Base error for expected listing persistence failures."""


class ListingValidationError(ListingStoreError):
    """Raised when listing input cannot satisfy database constraints."""


class ListingNotFoundError(ListingStoreError):
    """Raised when a listing is not visible to the current user."""


class ListingPermissionError(ListingStoreError):
    """Raised when a user cannot mutate a visible listing."""


class ListingStateError(ListingStoreError):
    """Raised when a listing state prevents a requested mutation."""


_REQUIRED_LISTING_FIELDS = (
    "manufacturer",
    "model",
    "variant",
    "model_year",
    "asking_price_usd",
    "airframe_hours",
    "engine_hours",
    "propeller_hours",
)

_LISTING_UPDATE_FIELDS = {
    "manufacturer",
    "model",
    "variant",
    "model_year",
    "asking_price_usd",
    "currency",
    "airframe_hours",
    "engine_hours",
    "propeller_hours",
    "listing_title",
    "registration_number",
    "serial_number",
    "status",
    "source_url",
    "damage_history_notes",
    "logbook_notes",
    "notes",
}

_MATCHED_LISTING_FIELDS = (
    "model_year",
    "asking_price_usd",
    "currency",
    "airframe_hours",
    "engine_hours",
    "propeller_hours",
    "status",
    "registration_number",
    "serial_number",
    "damage_history_notes",
    "logbook_notes",
    "notes",
)


def create_listing(
    connection: sqlite3.Connection,
    *,
    user_id: int,
    preview: ListingPreview,
    original_listing: dict[str, Any] | None = None,
) -> dict:
    """Create a sale listing from a preview result."""

    values = _values_from_preview(preview, original_listing=original_listing)
    _validate_listing_values(values)
    registration_number = _optional_string(values.get("registration_number"))

    if registration_number:
        unverified_listing_id = _unverified_listing_id_for_tail(
            connection,
            user_id=user_id,
            registration_number=registration_number,
        )
        if unverified_listing_id is not None:
            _update_listing_values(
                connection,
                listing_id=unverified_listing_id,
                values=values,
                update_added_at=True,
            )
            connection.commit()
            return get_listing(
                connection,
                user_id=user_id,
                listing_id=unverified_listing_id,
            )

    verified_listing_id = _matching_verified_listing_id(connection, values=values)
    if verified_listing_id is not None:
        _refresh_listing_timestamp(
            connection,
            listing_id=verified_listing_id,
            source_url=_optional_string(values.get("source_url")),
        )
        connection.commit()
        return get_listing(
            connection,
            user_id=user_id,
            listing_id=verified_listing_id,
        )

    listing_id = _insert_listing(connection, user_id=user_id, values=values)
    connection.commit()
    return get_listing(connection, user_id=user_id, listing_id=listing_id)


def list_listings(connection: sqlite3.Connection, *, user_id: int) -> list[dict]:
    """List sale listings visible to the current user."""

    rows = connection.execute(
        """
        SELECT
          l.*,
          mfr.name AS aircraft_manufacturer,
          model.name AS aircraft_model,
          variant.name AS aircraft_variant
        FROM aircraft_sale_listings l
        JOIN aircraft_variants variant
          ON variant.id = l.aircraft_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE l.is_verified = 1 OR l.created_by_user_id = ?
        ORDER BY l.added_at DESC, l.id DESC
        """,
        (user_id,),
    ).fetchall()
    return [_listing_from_row(connection, row) for row in rows]


def get_listing(
    connection: sqlite3.Connection,
    *,
    user_id: int,
    listing_id: int,
) -> dict:
    """Return a visible sale listing by id."""

    row = _visible_listing_row(connection, user_id=user_id, listing_id=listing_id)
    if row is None:
        raise ListingNotFoundError("listing not found")
    return _listing_from_row(connection, row)


def update_listing(
    connection: sqlite3.Connection,
    *,
    user_id: int,
    listing_id: int,
    listing: dict[str, Any],
) -> dict:
    """Update a user-owned listing while it has not been internally verified."""

    if not isinstance(listing, dict):
        raise ListingValidationError("listing must be a JSON object")

    row = _listing_row(connection, listing_id)
    if row is None:
        raise ListingNotFoundError("listing not found")
    _assert_user_can_mutate(row, user_id=user_id, action="update")

    current = get_listing(connection, user_id=user_id, listing_id=listing_id)
    values = _flatten_listing(current)
    for key, value in listing.items():
        if key in _LISTING_UPDATE_FIELDS:
            values[key] = value
        elif key != "avionics":
            raise ListingValidationError(f"unsupported listing field: {key}")

    _update_listing_values(
        connection,
        listing_id=listing_id,
        values=values,
        update_added_at=False,
    )

    connection.commit()
    return get_listing(connection, user_id=user_id, listing_id=listing_id)


def delete_listing(
    connection: sqlite3.Connection,
    *,
    user_id: int,
    listing_id: int,
) -> None:
    """Delete a user-owned listing while it has not been internally verified."""

    row = _listing_row(connection, listing_id)
    if row is None:
        raise ListingNotFoundError("listing not found")
    _assert_user_can_mutate(row, user_id=user_id, action="delete")

    connection.execute("DELETE FROM aircraft_sale_listings WHERE id = ?", (listing_id,))
    connection.commit()


def _insert_listing(
    connection: sqlite3.Connection,
    *,
    user_id: int,
    values: dict[str, Any],
) -> int:
    _validate_listing_values(values)
    aircraft_variant_id = _ensure_aircraft_variant(
        connection,
        manufacturer=str(values["manufacturer"]),
        model=str(values["model"]),
        variant=str(values["variant"]),
    )
    source_url = _optional_string(values.get("source_url"))
    columns = [
        "aircraft_variant_id",
        "created_by_user_id",
        "is_verified",
        "source_url",
        "listing_title",
        "model_year",
        "asking_price_usd",
        "currency",
        "added_at",
        "status",
        "registration_number",
        "serial_number",
        "airframe_hours",
        "engine_hours",
        "propeller_hours",
        "damage_history_notes",
        "logbook_notes",
        "notes",
    ]
    value_expressions = [
        "?",
        "?",
        "0",
        "?",
        "?",
        "?",
        "?",
        "?",
        current_timestamp_sql(),
        "?",
        "?",
        "?",
        "?",
        "?",
        "?",
        "?",
        "?",
        "?",
    ]
    params: list[Any] = [
        aircraft_variant_id,
        user_id,
        source_url,
        _optional_string(values.get("listing_title")),
        int(values["model_year"]),
        float(values["asking_price_usd"]),
        _optional_string(values.get("currency")) or "USD",
        _optional_string(values.get("status")) or "active",
        _optional_string(values.get("registration_number")),
        _optional_string(values.get("serial_number")),
        float(values["airframe_hours"]),
        float(values["engine_hours"]),
        float(values["propeller_hours"]),
        _optional_string(values.get("damage_history_notes")),
        _optional_string(values.get("logbook_notes")),
        _optional_string(values.get("notes")),
    ]
    if _table_has_column(connection, "aircraft_sale_listings", "observed_at"):
        columns.insert(9, "observed_at")
        value_expressions.insert(9, current_timestamp_sql())

    cursor = connection.execute(
        f"""
        INSERT INTO aircraft_sale_listings ({", ".join(columns)})
        VALUES ({", ".join(value_expressions)})
        """,
        params,
    )
    listing_id = int(cursor.lastrowid)
    _replace_listing_avionics(connection, listing_id, values.get("avionics"))
    return listing_id


def _update_listing_values(
    connection: sqlite3.Connection,
    *,
    listing_id: int,
    values: dict[str, Any],
    update_added_at: bool,
) -> None:
    _validate_listing_values(values)
    aircraft_variant_id = _ensure_aircraft_variant(
        connection,
        manufacturer=str(values["manufacturer"]),
        model=str(values["model"]),
        variant=str(values["variant"]),
    )
    source_url = _optional_string(values.get("source_url"))
    added_at_assignment = ""
    if update_added_at:
        added_at_assignment = f", added_at = {current_timestamp_sql()}"
    observed_at_assignment = ""
    if update_added_at and _table_has_column(connection, "aircraft_sale_listings", "observed_at"):
        observed_at_assignment = f", observed_at = {current_timestamp_sql()}"
    connection.execute(
        f"""
        UPDATE aircraft_sale_listings
        SET
          aircraft_variant_id = ?,
          source_url = ?,
          listing_title = ?,
          model_year = ?,
          asking_price_usd = ?,
          currency = ?,
          status = ?,
          registration_number = ?,
          serial_number = ?,
          airframe_hours = ?,
          engine_hours = ?,
          propeller_hours = ?,
          damage_history_notes = ?,
          logbook_notes = ?,
          notes = ?,
          updated_at = {current_timestamp_sql()}
          {added_at_assignment}
          {observed_at_assignment}
        WHERE id = ?
        """,
        (
            aircraft_variant_id,
            source_url,
            _optional_string(values.get("listing_title")),
            int(values["model_year"]),
            float(values["asking_price_usd"]),
            _optional_string(values.get("currency")) or "USD",
            _optional_string(values.get("status")) or "active",
            _optional_string(values.get("registration_number")),
            _optional_string(values.get("serial_number")),
            float(values["airframe_hours"]),
            float(values["engine_hours"]),
            float(values["propeller_hours"]),
            _optional_string(values.get("damage_history_notes")),
            _optional_string(values.get("logbook_notes")),
            _optional_string(values.get("notes")),
            listing_id,
        ),
    )
    _replace_listing_avionics(connection, listing_id, values.get("avionics"))


def _values_from_preview(
    preview: ListingPreview,
    *,
    original_listing: dict[str, Any] | None,
) -> dict[str, Any]:
    parsed = asdict(preview.parsed_listing)
    parsed["source_url"] = preview.source_url
    if original_listing:
        for key in ("damage_history_notes", "logbook_notes", "notes"):
            if key in original_listing:
                parsed[key] = original_listing[key]
    return parsed


def _unverified_listing_id_for_tail(
    connection: sqlite3.Connection,
    *,
    user_id: int,
    registration_number: str,
) -> int | None:
    row = connection.execute(
        """
        SELECT id
        FROM aircraft_sale_listings
        WHERE created_by_user_id = ?
          AND is_verified = 0
          AND UPPER(registration_number) = UPPER(?)
        ORDER BY added_at DESC, id DESC
        LIMIT 1
        """,
        (user_id, registration_number),
    ).fetchone()
    return int(row["id"]) if row else None


def _matching_verified_listing_id(
    connection: sqlite3.Connection,
    *,
    values: dict[str, Any],
) -> int | None:
    registration_number = _optional_string(values.get("registration_number"))
    if not registration_number:
        return None
    rows = connection.execute(
        """
        SELECT
          l.*,
          mfr.name AS aircraft_manufacturer,
          model.name AS aircraft_model,
          variant.name AS aircraft_variant
        FROM aircraft_sale_listings l
        JOIN aircraft_variants variant
          ON variant.id = l.aircraft_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE UPPER(l.registration_number) = UPPER(?)
          AND l.is_verified = 1
        ORDER BY l.added_at DESC, l.id DESC
        """,
        (registration_number,),
    ).fetchall()
    for row in rows:
        listing = _listing_from_row(connection, row)
        if _listing_matches_values(listing, values):
            return int(listing["id"])
    return None


def _refresh_listing_timestamp(
    connection: sqlite3.Connection,
    *,
    listing_id: int,
    source_url: str | None,
) -> None:
    observed_at_assignment = ""
    if _table_has_column(connection, "aircraft_sale_listings", "observed_at"):
        observed_at_assignment = f", observed_at = {current_timestamp_sql()}"
    connection.execute(
        f"""
        UPDATE aircraft_sale_listings
        SET
          added_at = {current_timestamp_sql()},
          source_url = COALESCE(source_url, ?),
          updated_at = {current_timestamp_sql()}
          {observed_at_assignment}
        WHERE id = ?
        """,
        (source_url, listing_id),
    )


def _listing_matches_values(listing: dict[str, Any], values: dict[str, Any]) -> bool:
    aircraft = listing["aircraft"]
    for field_name, current_value in (
        ("manufacturer", aircraft["manufacturer"]),
        ("model", aircraft["model"]),
        ("variant", aircraft["variant"]),
    ):
        if normalize_name(str(current_value)) != normalize_name(str(values.get(field_name))):
            return False

    for field_name in _MATCHED_LISTING_FIELDS:
        if not _values_match(listing.get(field_name), values.get(field_name)):
            return False

    return _canonical_avionics(listing.get("avionics")) == _canonical_avionics(
        values.get("avionics")
    )


def _canonical_avionics(value: Any) -> list[tuple[str, str, str, int]]:
    if not isinstance(value, list):
        return []
    canonical: set[tuple[str, str, str, int]] = set()
    for item in value:
        if not isinstance(item, dict):
            continue
        manufacturer = _optional_string(item.get("manufacturer"))
        model = _optional_string(item.get("model"))
        avionics_type = _optional_string(item.get("type")) or "Unknown"
        if not manufacturer or not model:
            continue
        canonical.add(
            (
                normalize_name(manufacturer),
                normalize_name(model),
                normalize_name(avionics_type),
                _optional_int_min(item.get("quantity"), 1) or 1,
            )
        )
    return sorted(canonical)


def _values_match(left: Any, right: Any) -> bool:
    left_number = _optional_float_or_none(left)
    right_number = _optional_float_or_none(right)
    if left_number is not None or right_number is not None:
        return (
            left_number is not None
            and right_number is not None
            and abs(left_number - right_number) <= 0.01
        )
    left_text = _optional_string(left)
    right_text = _optional_string(right)
    return (left_text or "") == (right_text or "")


def _flatten_listing(listing: dict[str, Any]) -> dict[str, Any]:
    aircraft = listing["aircraft"]
    values = {
        "manufacturer": aircraft["manufacturer"],
        "model": aircraft["model"],
        "variant": aircraft["variant"],
        "source_url": listing["source_url"],
        "listing_title": listing["listing_title"],
        "model_year": listing["model_year"],
        "asking_price_usd": listing["asking_price_usd"],
        "currency": listing["currency"],
        "status": listing["status"],
        "registration_number": listing["registration_number"],
        "serial_number": listing["serial_number"],
        "airframe_hours": listing["airframe_hours"],
        "engine_hours": listing["engine_hours"],
        "propeller_hours": listing["propeller_hours"],
        "damage_history_notes": listing["damage_history_notes"],
        "logbook_notes": listing["logbook_notes"],
        "notes": listing["notes"],
        "avionics": listing["avionics"],
    }
    return values


def _validate_listing_values(values: dict[str, Any]) -> None:
    missing = [
        field_name
        for field_name in _REQUIRED_LISTING_FIELDS
        if _is_missing(values.get(field_name))
    ]
    if missing:
        raise ListingValidationError(
            "cannot save listing; missing fields: " + ", ".join(missing)
        )
    try:
        int(values["model_year"])
        float(values["asking_price_usd"])
        float(values["airframe_hours"])
        float(values["engine_hours"])
        float(values["propeller_hours"])
    except (TypeError, ValueError) as exc:
        raise ListingValidationError(
            "model_year, asking_price_usd, airframe_hours, engine_hours, and "
            "propeller_hours must be numeric"
        ) from exc


def _ensure_aircraft_variant(
    connection: sqlite3.Connection,
    *,
    manufacturer: str,
    model: str,
    variant: str,
) -> int:
    manufacturer_id = _ensure_named_row(
        connection,
        table="aircraft_manufacturers",
        name=manufacturer,
    )
    model_normalized = normalize_name(model)
    connection.execute(
        """
        INSERT OR IGNORE INTO aircraft_models (
          aircraft_manufacturer_id,
          name,
          normalized_name
        )
        VALUES (?, ?, ?)
        """,
        (manufacturer_id, model, model_normalized),
    )
    model_id = connection.execute(
        """
        SELECT id
        FROM aircraft_models
        WHERE aircraft_manufacturer_id = ? AND normalized_name = ?
        """,
        (manufacturer_id, model_normalized),
    ).fetchone()["id"]

    variant_normalized = normalize_name(variant)
    connection.execute(
        """
        INSERT OR IGNORE INTO aircraft_variants (
          aircraft_model_id,
          name,
          normalized_name
        )
        VALUES (?, ?, ?)
        """,
        (model_id, variant, variant_normalized),
    )
    return int(
        connection.execute(
            """
            SELECT id
            FROM aircraft_variants
            WHERE aircraft_model_id = ? AND normalized_name = ?
            """,
            (model_id, variant_normalized),
        ).fetchone()["id"]
    )


def _ensure_avionics_model(
    connection: sqlite3.Connection,
    *,
    manufacturer: str,
    model: str,
    avionics_type: str,
) -> int:
    manufacturer_id = _ensure_named_row(
        connection,
        table="avionics_manufacturers",
        name=manufacturer,
    )
    type_id = _ensure_named_row(
        connection,
        table="avionics_types",
        name=avionics_type,
    )
    normalized_model = normalize_name(model)
    connection.execute(
        """
        INSERT OR IGNORE INTO avionics_models (
          avionics_manufacturer_id,
          avionics_type_id,
          name,
          normalized_name
        )
        VALUES (?, ?, ?, ?)
        """,
        (manufacturer_id, type_id, model, normalized_model),
    )
    return int(
        connection.execute(
            """
            SELECT id
            FROM avionics_models
            WHERE avionics_manufacturer_id = ? AND normalized_name = ?
            """,
            (manufacturer_id, normalized_model),
        ).fetchone()["id"]
    )


def _ensure_named_row(
    connection: sqlite3.Connection,
    *,
    table: str,
    name: str,
) -> int:
    normalized_name = normalize_name(name)
    connection.execute(
        f"""
        INSERT OR IGNORE INTO {table} (name, normalized_name)
        VALUES (?, ?)
        """,
        (name, normalized_name),
    )
    row = connection.execute(
        f"""
        SELECT id
        FROM {table}
        WHERE normalized_name = ?
        """,
        (normalized_name,),
    ).fetchone()
    return int(row["id"])


def _replace_listing_avionics(
    connection: sqlite3.Connection,
    listing_id: int,
    value: Any,
) -> None:
    connection.execute(
        "DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        (listing_id,),
    )
    if not isinstance(value, list):
        return

    for item in value:
        if not isinstance(item, dict):
            continue
        manufacturer = _optional_string(item.get("manufacturer"))
        model = _optional_string(item.get("model"))
        avionics_type = _optional_string(item.get("type")) or "Unknown"
        if not manufacturer or not model:
            continue
        avionics_model_id = _ensure_avionics_model(
            connection,
            manufacturer=manufacturer,
            model=model,
            avionics_type=avionics_type,
        )
        connection.execute(
            """
            INSERT OR REPLACE INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id,
              avionics_model_id,
              quantity,
              notes
            )
            VALUES (?, ?, ?, ?)
            """,
            (
                listing_id,
                avionics_model_id,
                _optional_int_min(item.get("quantity"), 1) or 1,
                _optional_string(item.get("notes")),
            ),
        )


def _listing_from_row(connection: sqlite3.Connection, row: sqlite3.Row) -> dict:
    listing = row_to_dict(row)
    listing["is_verified"] = bool(listing["is_verified"])
    for legacy_key in ("is_valid", "verified_at", "observed_at", "invalidated_at", "validation_notes"):
        listing.pop(legacy_key, None)
    listing["aircraft"] = {
        "manufacturer": listing.pop("aircraft_manufacturer"),
        "model": listing.pop("aircraft_model"),
        "variant": listing.pop("aircraft_variant"),
        "aircraft_variant_id": listing["aircraft_variant_id"],
    }
    listing["avionics"] = _listing_avionics(connection, listing_id=listing["id"])
    return listing


def _listing_avionics(connection: sqlite3.Connection, *, listing_id: int) -> list[dict]:
    rows = connection.execute(
        """
        SELECT
          mfr.name AS manufacturer,
          model.name AS model,
          type.name AS type,
          link.quantity,
          link.notes
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model
          ON model.id = link.avionics_model_id
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        JOIN avionics_types type
          ON type.id = model.avionics_type_id
        WHERE link.aircraft_sale_listing_id = ?
        ORDER BY link.id
        """,
        (listing_id,),
    ).fetchall()
    return [row_to_dict(row) for row in rows]


def _visible_listing_row(
    connection: sqlite3.Connection,
    *,
    user_id: int,
    listing_id: int,
) -> sqlite3.Row | None:
    return connection.execute(
        """
        SELECT
          l.*,
          mfr.name AS aircraft_manufacturer,
          model.name AS aircraft_model,
          variant.name AS aircraft_variant
        FROM aircraft_sale_listings l
        JOIN aircraft_variants variant
          ON variant.id = l.aircraft_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE l.id = ? AND (l.is_verified = 1 OR l.created_by_user_id = ?)
        """,
        (listing_id, user_id),
    ).fetchone()


def _listing_row(connection: sqlite3.Connection, listing_id: int) -> sqlite3.Row | None:
    return connection.execute(
        """
        SELECT id, created_by_user_id, is_verified
        FROM aircraft_sale_listings
        WHERE id = ?
        """,
        (listing_id,),
    ).fetchone()


def _assert_user_can_mutate(row: sqlite3.Row, *, user_id: int, action: str) -> None:
    if int(row["created_by_user_id"]) != user_id:
        raise ListingPermissionError(f"cannot {action} a listing owned by another user")
    if bool(row["is_verified"]):
        raise ListingStateError(f"cannot {action} an internally verified listing")


def _optional_string(value: Any) -> str | None:
    if value is None:
        return None
    if isinstance(value, str):
        stripped = value.strip()
        return stripped or None
    return str(value)


def _optional_int_min(value: Any, minimum: int) -> int | None:
    if value is None or value == "":
        return None
    try:
        number = int(float(str(value).replace(",", "").strip()))
    except (TypeError, ValueError):
        return None
    return number if number >= minimum else None


def _optional_float_or_none(value: Any) -> float | None:
    if value is None or value == "":
        return None
    try:
        return float(str(value).replace(",", "").replace("$", "").strip())
    except (TypeError, ValueError):
        return None


def _is_missing(value: Any) -> bool:
    if value is None:
        return True
    return isinstance(value, str) and not value.strip()


def _table_has_column(
    connection: sqlite3.Connection,
    table_name: str,
    column_name: str,
) -> bool:
    rows = connection.execute(f"PRAGMA table_info({table_name})").fetchall()
    return any(row["name"] == column_name for row in rows)
