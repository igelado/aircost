"""Minimal REST server for the aircraft listings web application."""

from __future__ import annotations

import argparse
import json
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from .database import (
    DEFAULT_DATABASE_PATH,
    DEVELOPER_EMAIL,
    connect_database,
    initialize_database,
    row_to_dict,
)
from .listing_parser import (
    ExtractListing,
    FetchUrl,
    ListingExtractionError,
    ListingExtractorUnavailable,
    ListingFetchError,
    preview_listing_url,
    preview_manual_listing,
)
from .listings import (
    ListingNotFoundError,
    ListingPermissionError,
    ListingStateError,
    ListingValidationError,
    create_listing,
    delete_listing,
    get_listing,
    list_listings,
    update_listing,
)


class AircostHttpServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class ApiError(Exception):
    """Expected API error that should be returned as JSON."""

    def __init__(self, status: HTTPStatus, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.message = message


def create_server(
    *,
    host: str,
    port: int,
    database_path: str | Path = DEFAULT_DATABASE_PATH,
    fetcher: FetchUrl | None = None,
    listing_extractor: ExtractListing | None = None,
) -> AircostHttpServer:
    """Create a configured HTTP server."""

    initialize_database(database_path)

    class AircostRequestHandler(_AircostRequestHandler):
        app_database_path = Path(database_path)
        app_fetcher = staticmethod(fetcher) if fetcher is not None else None
        app_listing_extractor = (
            staticmethod(listing_extractor) if listing_extractor is not None else None
        )

    return AircostHttpServer((host, port), AircostRequestHandler)


class _AircostRequestHandler(BaseHTTPRequestHandler):
    app_database_path = DEFAULT_DATABASE_PATH
    app_fetcher: FetchUrl | None = None
    app_listing_extractor: ExtractListing | None = None

    def do_GET(self) -> None:
        try:
            parsed = urlparse(self.path)
            if parsed.path == "/health":
                self._send_json({"ok": True})
                return
            if parsed.path == "/api/users/current":
                with connect_database(self.app_database_path) as connection:
                    user = _current_user(connection, self.headers)
                self._send_json({"user": user})
                return
            if parsed.path == "/api/listings":
                with connect_database(self.app_database_path) as connection:
                    user = _current_user(connection, self.headers)
                    listings = list_listings(connection, user_id=user["id"])
                self._send_json({"current_user": user, "listings": listings})
                return
            listing_id = _listing_id_from_path(parsed.path)
            if listing_id is not None:
                with connect_database(self.app_database_path) as connection:
                    user = _current_user(connection, self.headers)
                    listing = get_listing(
                        connection,
                        user_id=user["id"],
                        listing_id=listing_id,
                    )
                self._send_json({"current_user": user, "listing": listing})
                return
            if parsed.path == "/":
                self._send_html(_index_html())
                return
            raise ApiError(HTTPStatus.NOT_FOUND, "endpoint not found")
        except ApiError as exc:
            self._send_error(exc)
        except ListingNotFoundError as exc:
            self._send_error(ApiError(HTTPStatus.NOT_FOUND, str(exc)))

    def do_POST(self) -> None:
        try:
            parsed = urlparse(self.path)
            if parsed.path == "/api/listings/preview":
                payload = self._read_json_body()
                with connect_database(self.app_database_path) as connection:
                    user = _current_user(connection, self.headers)
                preview = _preview_listing_payload(
                    payload,
                    self.app_fetcher,
                    self.app_listing_extractor,
                ).to_dict()
                self._send_json(
                    {
                        "current_user": user,
                        "preview": preview,
                    }
                )
                return
            if parsed.path == "/api/listings":
                payload = self._read_json_body()
                with connect_database(self.app_database_path) as connection:
                    user = _current_user(connection, self.headers)
                preview = _preview_listing_payload(
                    payload,
                    self.app_fetcher,
                    self.app_listing_extractor,
                )
                original_listing = payload.get("listing")
                if not isinstance(original_listing, dict):
                    original_listing = None
                with connect_database(self.app_database_path) as connection:
                    listing = create_listing(
                        connection,
                        user_id=user["id"],
                        preview=preview,
                        original_listing=original_listing,
                    )
                self._send_json(
                    {"current_user": user, "listing": listing},
                    HTTPStatus.CREATED,
                )
                return
            raise ApiError(HTTPStatus.NOT_FOUND, "endpoint not found")
        except ApiError as exc:
            self._send_error(exc)
        except ListingFetchError as exc:
            self._send_error(ApiError(HTTPStatus.BAD_GATEWAY, str(exc)))
        except ListingExtractorUnavailable as exc:
            self._send_error(ApiError(HTTPStatus.SERVICE_UNAVAILABLE, str(exc)))
        except ListingExtractionError as exc:
            self._send_error(ApiError(HTTPStatus.BAD_GATEWAY, str(exc)))
        except ValueError as exc:
            self._send_error(ApiError(HTTPStatus.BAD_REQUEST, str(exc)))
        except ListingValidationError as exc:
            self._send_error(ApiError(HTTPStatus.BAD_REQUEST, str(exc)))
        except ListingNotFoundError as exc:
            self._send_error(ApiError(HTTPStatus.NOT_FOUND, str(exc)))
        except ListingPermissionError as exc:
            self._send_error(ApiError(HTTPStatus.FORBIDDEN, str(exc)))
        except ListingStateError as exc:
            self._send_error(ApiError(HTTPStatus.CONFLICT, str(exc)))

    def do_PATCH(self) -> None:
        try:
            parsed = urlparse(self.path)
            listing_id = _listing_id_from_path(parsed.path)
            if listing_id is None:
                raise ApiError(HTTPStatus.NOT_FOUND, "endpoint not found")
            payload = self._read_json_body()
            listing_payload = payload.get("listing")
            if not isinstance(listing_payload, dict):
                raise ApiError(HTTPStatus.BAD_REQUEST, "listing must be a JSON object")
            with connect_database(self.app_database_path) as connection:
                user = _current_user(connection, self.headers)
                listing = update_listing(
                    connection,
                    user_id=user["id"],
                    listing_id=listing_id,
                    listing=listing_payload,
                )
            self._send_json({"current_user": user, "listing": listing})
        except ApiError as exc:
            self._send_error(exc)
        except ListingValidationError as exc:
            self._send_error(ApiError(HTTPStatus.BAD_REQUEST, str(exc)))
        except ListingNotFoundError as exc:
            self._send_error(ApiError(HTTPStatus.NOT_FOUND, str(exc)))
        except ListingPermissionError as exc:
            self._send_error(ApiError(HTTPStatus.FORBIDDEN, str(exc)))
        except ListingStateError as exc:
            self._send_error(ApiError(HTTPStatus.CONFLICT, str(exc)))
        except ValueError as exc:
            self._send_error(ApiError(HTTPStatus.BAD_REQUEST, str(exc)))

    def do_DELETE(self) -> None:
        try:
            parsed = urlparse(self.path)
            listing_id = _listing_id_from_path(parsed.path)
            if listing_id is None:
                raise ApiError(HTTPStatus.NOT_FOUND, "endpoint not found")
            with connect_database(self.app_database_path) as connection:
                user = _current_user(connection, self.headers)
                delete_listing(connection, user_id=user["id"], listing_id=listing_id)
            self._send_empty(HTTPStatus.NO_CONTENT)
        except ApiError as exc:
            self._send_error(exc)
        except ListingValidationError as exc:
            self._send_error(ApiError(HTTPStatus.BAD_REQUEST, str(exc)))
        except ListingNotFoundError as exc:
            self._send_error(ApiError(HTTPStatus.NOT_FOUND, str(exc)))
        except ListingPermissionError as exc:
            self._send_error(ApiError(HTTPStatus.FORBIDDEN, str(exc)))
        except ListingStateError as exc:
            self._send_error(ApiError(HTTPStatus.CONFLICT, str(exc)))

    def log_message(self, format: str, *args: Any) -> None:
        return

    def _read_json_body(self) -> dict[str, Any]:
        content_length = int(self.headers.get("Content-Length", "0"))
        if content_length <= 0:
            raise ApiError(HTTPStatus.BAD_REQUEST, "request body is required")
        raw_body = self.rfile.read(content_length)
        try:
            payload = json.loads(raw_body.decode("utf-8"))
        except json.JSONDecodeError as exc:
            raise ApiError(HTTPStatus.BAD_REQUEST, f"invalid JSON: {exc}") from exc
        if not isinstance(payload, dict):
            raise ApiError(HTTPStatus.BAD_REQUEST, "request body must be a JSON object")
        return payload

    def _send_json(self, payload: dict[str, Any], status: HTTPStatus = HTTPStatus.OK) -> None:
        body = json.dumps(payload, indent=2, sort_keys=True).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_html(self, html: str) -> None:
        body = html.encode("utf-8")
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_empty(self, status: HTTPStatus) -> None:
        self.send_response(status)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def _send_error(self, error: ApiError) -> None:
        self._send_json(
            {
                "error": {
                    "message": error.message,
                    "status": error.status.value,
                }
            },
            error.status,
        )


def _preview_listing_payload(
    payload: dict[str, Any],
    fetcher: FetchUrl | None,
    listing_extractor: ExtractListing | None,
):
    source_url = payload.get("source_url")
    listing = payload.get("listing")
    has_source_url = source_url is not None
    has_listing = listing is not None
    if has_source_url and has_listing:
        raise ApiError(
            HTTPStatus.BAD_REQUEST,
            "provide either source_url or listing, not both",
        )
    if has_source_url:
        return preview_listing_url(
            str(source_url),
            fetcher=fetcher,
            extractor=listing_extractor,
        )
    if has_listing:
        if not isinstance(listing, dict):
            raise ApiError(HTTPStatus.BAD_REQUEST, "listing must be a JSON object")
        return preview_manual_listing(listing)
    raise ApiError(HTTPStatus.BAD_REQUEST, "provide source_url or listing")


def _listing_id_from_path(path: str) -> int | None:
    parts = path.strip("/").split("/")
    if len(parts) != 3 or parts[:2] != ["api", "listings"]:
        return None
    try:
        listing_id = int(parts[2])
    except ValueError as exc:
        raise ApiError(HTTPStatus.NOT_FOUND, "endpoint not found") from exc
    if listing_id <= 0:
        raise ApiError(HTTPStatus.NOT_FOUND, "endpoint not found")
    return listing_id


def _current_user(connection, headers) -> dict:
    email = headers.get("X-User-Email", DEVELOPER_EMAIL)
    row = connection.execute(
        """
        SELECT id, email, display_name, auth_provider, auth_subject
        FROM users
        WHERE email = ?
        """,
        (email,),
    ).fetchone()
    if row is None:
        raise ApiError(HTTPStatus.UNAUTHORIZED, "unknown user")
    return row_to_dict(row)


def _index_html() -> str:
    return """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>aircost listing preview</title>
  <style>
    body {
      font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      margin: 0;
      color: #1f2933;
      background: #f5f7fa;
    }
    main {
      max-width: 960px;
      margin: 0 auto;
      padding: 32px 20px;
    }
    h1 {
      font-size: 28px;
      margin: 0 0 20px;
    }
    form {
      display: grid;
      gap: 12px;
      margin-bottom: 16px;
    }
    input, button, textarea {
      font: inherit;
    }
    input {
      padding: 10px 12px;
      border: 1px solid #c8d1dc;
      border-radius: 6px;
      background: white;
    }
    button {
      width: fit-content;
      padding: 10px 14px;
      border: 0;
      border-radius: 6px;
      color: white;
      background: #22577a;
      cursor: pointer;
    }
    pre {
      min-height: 320px;
      padding: 16px;
      overflow: auto;
      border: 1px solid #d8dee8;
      border-radius: 6px;
      background: white;
    }
  </style>
</head>
<body>
  <main>
    <h1>Listing Preview</h1>
    <form id="preview-form">
      <input id="source-url" name="source_url" type="url" placeholder="Listing URL" required>
      <button type="submit">Preview</button>
    </form>
    <pre id="output">{}</pre>
  </main>
  <script>
    const form = document.querySelector("#preview-form");
    const output = document.querySelector("#output");
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      output.textContent = "Loading...";
      const sourceUrl = document.querySelector("#source-url").value;
      const response = await fetch("/api/listings/preview", {
        method: "POST",
        headers: {"Content-Type": "application/json"},
        body: JSON.stringify({source_url: sourceUrl})
      });
      const payload = await response.json();
      output.textContent = JSON.stringify(payload, null, 2);
    });
  </script>
</body>
</html>
"""


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Run the aircost web application.")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument(
        "--database",
        type=Path,
        default=DEFAULT_DATABASE_PATH,
        help="SQLite database path.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    server = create_server(
        host=args.host,
        port=args.port,
        database_path=args.database,
    )
    print(f"Serving aircost web app on http://{args.host}:{args.port}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
