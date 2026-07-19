import http.client
import json
import tempfile
import threading
import unittest
from pathlib import Path

from aircost.webapp.database import connect_database
from aircost.webapp.server import create_server
from tests.test_listing_parser import SR20_HTML, sr20_model_output


class WebappListingApiTests(unittest.TestCase):
    def test_creates_lists_updates_and_deletes_manual_listing(self):
        with _running_server() as server:
            status, payload = _request(
                server,
                "POST",
                "/api/listings",
                {"listing": _manual_listing()},
            )
            listing_id = payload["listing"]["id"]

            self.assertEqual(status, 201)
            self.assertFalse(payload["listing"]["is_verified"])
            self.assertIsNone(payload["listing"]["source_url"])
            self.assertEqual(payload["listing"]["aircraft"]["manufacturer"], "Cirrus")

            status, payload = _request(server, "GET", "/api/listings")
            self.assertEqual(status, 200)
            self.assertEqual(len(payload["listings"]), 1)

            status, payload = _request(
                server,
                "PATCH",
                f"/api/listings/{listing_id}",
                {"listing": {"asking_price_usd": 585000}},
            )
            self.assertEqual(status, 200)
            self.assertEqual(payload["listing"]["asking_price_usd"], 585000)

            status, payload = _request(server, "DELETE", f"/api/listings/{listing_id}")
            self.assertEqual(status, 204)
            self.assertIsNone(payload)

            status, payload = _request(server, "GET", "/api/listings")
            self.assertEqual(status, 200)
            self.assertEqual(payload["listings"], [])

    def test_creates_listing_from_url_preview(self):
        def fetcher(source_url):
            self.assertEqual(source_url, "https://example.test/sr20")
            return SR20_HTML

        with _running_server(
            fetcher=fetcher,
            listing_extractor=lambda _: sr20_model_output(),
        ) as server:
            status, payload = _request(
                server,
                "POST",
                "/api/listings",
                {"source_url": "https://example.test/sr20"},
            )

        self.assertEqual(status, 201)
        self.assertEqual(payload["listing"]["source_url"], "https://example.test/sr20")
        self.assertFalse(payload["listing"]["is_verified"])
        self.assertEqual(payload["listing"]["aircraft"]["model"], "SR20")
        self.assertEqual(payload["listing"]["avionics"][0]["model"], "Perspective+")

    def test_add_overwrites_current_users_unverified_listing_for_same_tail(self):
        with _running_server() as server:
            status, first = _request(
                server,
                "POST",
                "/api/listings",
                {"listing": _manual_listing(registration_number="N123AB", price=579000)},
            )
            self.assertEqual(status, 201)

            status, second = _request(
                server,
                "POST",
                "/api/listings",
                {"listing": _manual_listing(registration_number="N123AB", price=585000)},
            )
            self.assertEqual(status, 201)

            status, listed = _request(server, "GET", "/api/listings")

        self.assertEqual(second["listing"]["id"], first["listing"]["id"])
        self.assertEqual(second["listing"]["asking_price_usd"], 585000)
        self.assertEqual(status, 200)
        self.assertEqual(len(listed["listings"]), 1)

    def test_add_refreshes_verified_duplicate_and_versions_changed_verified_listing(self):
        with _running_server() as server:
            status, first = _request(
                server,
                "POST",
                "/api/listings",
                {"listing": _manual_listing(registration_number="N456CD", price=579000)},
            )
            self.assertEqual(status, 201)
            listing_id = first["listing"]["id"]
            with connect_database(server.app_database_path) as connection:
                connection.execute(
                    """
                    UPDATE aircraft_sale_listings
                    SET
                      is_verified = 1,
                      source_url = 'https://example.test/n456cd',
                      added_at = '2024-01-01 00:00:00'
                    WHERE id = ?
                    """,
                    (listing_id,),
                )
                connection.commit()

            status, refreshed = _request(
                server,
                "POST",
                "/api/listings",
                {"listing": _manual_listing(registration_number="N456CD", price=579000)},
            )
            self.assertEqual(status, 201)

            status, changed = _request(
                server,
                "POST",
                "/api/listings",
                {"listing": _manual_listing(registration_number="N456CD", price=590000)},
            )
            self.assertEqual(status, 201)

            status, listed = _request(server, "GET", "/api/listings")

        self.assertEqual(refreshed["listing"]["id"], listing_id)
        self.assertTrue(refreshed["listing"]["is_verified"])
        self.assertNotEqual(refreshed["listing"]["added_at"], "2024-01-01 00:00:00")
        self.assertNotEqual(changed["listing"]["id"], listing_id)
        self.assertFalse(changed["listing"]["is_verified"])
        self.assertEqual(status, 200)
        self.assertEqual(len(listed["listings"]), 2)

    def test_rejects_updates_and_deletes_for_verified_listing(self):
        with _running_server() as server:
            status, created = _request(
                server,
                "POST",
                "/api/listings",
                {"listing": _manual_listing(registration_number="N789EF")},
            )
            self.assertEqual(status, 201)
            listing_id = created["listing"]["id"]
            with connect_database(server.app_database_path) as connection:
                connection.execute(
                    """
                    UPDATE aircraft_sale_listings
                    SET is_verified = 1, source_url = 'https://example.test/n789ef'
                    WHERE id = ?
                    """,
                    (listing_id,),
                )
                connection.commit()

            status, payload = _request(
                server,
                "PATCH",
                f"/api/listings/{listing_id}",
                {"listing": {"asking_price_usd": 590000}},
            )
            self.assertEqual(status, 409)
            self.assertIn("internally verified", payload["error"]["message"])

            status, payload = _request(server, "DELETE", f"/api/listings/{listing_id}")
            self.assertEqual(status, 409)
            self.assertIn("internally verified", payload["error"]["message"])

    def test_unverified_listing_is_only_visible_to_creator_until_verified(self):
        with _running_server() as server:
            status, created = _request(
                server,
                "POST",
                "/api/listings",
                {"listing": _manual_listing(registration_number="N222ME")},
            )
            self.assertEqual(status, 201)
            listing_id = created["listing"]["id"]
            with connect_database(server.app_database_path) as connection:
                connection.execute(
                    """
                    INSERT INTO users (
                      email,
                      display_name,
                      auth_provider,
                      auth_subject
                    )
                    VALUES ('other@localhost', 'Other', 'local', 'other')
                    """
                )
                connection.commit()

            status, payload = _request(
                server,
                "GET",
                "/api/listings",
                headers={"X-User-Email": "other@localhost"},
            )
            self.assertEqual(status, 200)
            self.assertEqual(payload["listings"], [])

            with connect_database(server.app_database_path) as connection:
                connection.execute(
                    """
                    UPDATE aircraft_sale_listings
                    SET is_verified = 1, source_url = 'https://example.test/n222me'
                    WHERE id = ?
                    """,
                    (listing_id,),
                )
                connection.commit()

            status, payload = _request(
                server,
                "GET",
                "/api/listings",
                headers={"X-User-Email": "other@localhost"},
            )

        self.assertEqual(status, 200)
        self.assertEqual(len(payload["listings"]), 1)


def _manual_listing(
    *,
    registration_number: str = "N12345",
    price: float = 579000,
) -> dict:
    return {
        "manufacturer": "Cirrus",
        "model": "SR20",
        "variant": "SR20 G6",
        "model_year": 2023,
        "asking_price_usd": price,
        "currency": "USD",
        "airframe_hours": 75,
        "engine_hours": 75,
        "propeller_hours": 75,
        "listing_title": "2023 Cirrus SR20 G6",
        "registration_number": registration_number,
        "serial_number": "1234",
        "status": "active",
        "avionics": [
            {
                "manufacturer": "Garmin",
                "model": "Perspective+",
                "type": "Integrated Flight Deck",
                "quantity": 1,
            }
        ],
    }


class _running_server:
    def __init__(self, *, fetcher=None, listing_extractor=None):
        self._directory = tempfile.TemporaryDirectory()
        self._fetcher = fetcher
        self._listing_extractor = listing_extractor
        self.server = None
        self._thread = None

    def __enter__(self):
        database_path = Path(self._directory.name) / "aircost.sqlite3"
        self.server = create_server(
            host="127.0.0.1",
            port=0,
            database_path=database_path,
            fetcher=self._fetcher,
            listing_extractor=self._listing_extractor,
        )
        self.server.app_database_path = database_path
        self._thread = threading.Thread(
            target=lambda: self.server.serve_forever(poll_interval=0.05),
            daemon=True,
        )
        self._thread.start()
        return self.server

    def __exit__(self, exc_type, exc, traceback):
        self.server.shutdown()
        self._thread.join(timeout=5)
        self.server.server_close()
        self._directory.cleanup()


def _request(server, method, path, payload=None, headers=None):
    request_headers = headers.copy() if headers else {}
    body = None
    if payload is not None:
        body = json.dumps(payload)
        request_headers["Content-Type"] = "application/json"
    connection = http.client.HTTPConnection(
        "127.0.0.1",
        server.server_port,
        timeout=5,
    )
    connection.request(method, path, body=body, headers=request_headers)
    response = connection.getresponse()
    raw_body = response.read().decode("utf-8")
    connection.close()
    if not raw_body:
        return response.status, None
    return response.status, json.loads(raw_body)


if __name__ == "__main__":
    unittest.main()
