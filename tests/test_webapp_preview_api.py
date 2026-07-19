import http.client
import json
import tempfile
import threading
import unittest
from pathlib import Path

from aircost.webapp.listing_parser import ListingFetchError
from aircost.webapp.server import create_server
from tests.test_listing_parser import SR20_HTML, sr20_model_output


class WebappPreviewApiTests(unittest.TestCase):
    def test_preview_endpoint_parses_url_payload(self):
        def fetcher(source_url):
            self.assertEqual(source_url, "https://example.test/listing")
            return SR20_HTML

        with tempfile.TemporaryDirectory() as directory:
            server = create_server(
                host="127.0.0.1",
                port=0,
                database_path=Path(directory) / "aircost.sqlite3",
                fetcher=fetcher,
                listing_extractor=lambda _: sr20_model_output(),
            )
            thread = threading.Thread(
                target=lambda: server.serve_forever(poll_interval=0.05),
                daemon=True,
            )
            thread.start()
            try:
                connection = http.client.HTTPConnection(
                    "127.0.0.1",
                    server.server_port,
                    timeout=5,
                )
                connection.request(
                    "POST",
                    "/api/listings/preview",
                    body=json.dumps({"source_url": "https://example.test/listing"}),
                    headers={"Content-Type": "application/json"},
                )
                response = connection.getresponse()
                payload = json.loads(response.read().decode("utf-8"))
                connection.close()
            finally:
                server.shutdown()
                thread.join(timeout=5)
                server.server_close()

        self.assertEqual(response.status, 200)
        self.assertEqual(
            payload["current_user"]["email"],
            "developer@localhost",
        )
        self.assertEqual(
            payload["preview"]["parsed_listing"]["manufacturer"],
            "Cirrus",
        )
        self.assertEqual(
            payload["preview"]["parsed_listing"]["asking_price_usd"],
            579000,
        )

    def test_preview_endpoint_rejects_ambiguous_payload(self):
        with tempfile.TemporaryDirectory() as directory:
            server = create_server(
                host="127.0.0.1",
                port=0,
                database_path=Path(directory) / "aircost.sqlite3",
                fetcher=lambda _: "",
            )
            thread = threading.Thread(
                target=lambda: server.serve_forever(poll_interval=0.05),
                daemon=True,
            )
            thread.start()
            try:
                connection = http.client.HTTPConnection(
                    "127.0.0.1",
                    server.server_port,
                    timeout=5,
                )
                connection.request(
                    "POST",
                    "/api/listings/preview",
                    body=json.dumps(
                        {
                            "source_url": "https://example.test/listing",
                            "listing": {},
                        }
                    ),
                    headers={"Content-Type": "application/json"},
                )
                response = connection.getresponse()
                payload = json.loads(response.read().decode("utf-8"))
                connection.close()
            finally:
                server.shutdown()
                thread.join(timeout=5)
                server.server_close()

        self.assertEqual(response.status, 400)
        self.assertEqual(
            payload["error"]["message"],
            "provide either source_url or listing, not both",
        )

    def test_preview_endpoint_returns_bad_gateway_for_fetch_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            server = create_server(
                host="127.0.0.1",
                port=0,
                database_path=Path(directory) / "aircost.sqlite3",
                fetcher=lambda _: (_ for _ in ()).throw(
                    ListingFetchError("could not fetch source_url: blocked")
                ),
            )
            thread = threading.Thread(
                target=lambda: server.serve_forever(poll_interval=0.05),
                daemon=True,
            )
            thread.start()
            try:
                connection = http.client.HTTPConnection(
                    "127.0.0.1",
                    server.server_port,
                    timeout=5,
                )
                connection.request(
                    "POST",
                    "/api/listings/preview",
                    body=json.dumps({"source_url": "https://example.test/listing"}),
                    headers={"Content-Type": "application/json"},
                )
                response = connection.getresponse()
                payload = json.loads(response.read().decode("utf-8"))
                connection.close()
            finally:
                server.shutdown()
                thread.join(timeout=5)
                server.server_close()

        self.assertEqual(response.status, 502)
        self.assertEqual(
            payload["error"]["message"],
            "could not fetch source_url: blocked",
        )


if __name__ == "__main__":
    unittest.main()
