import json
import unittest
from unittest.mock import Mock, patch

import certifi

from aircost.webapp import listing_parser
from aircost.webapp.listing_parser import (
    GeminiListingExtractor,
    ListingExtractorUnavailable,
    clean_listing_html,
    fetch_url,
    normalize_name,
    parse_listing_html,
    preview_manual_listing,
)


SR20_HTML = """
<!doctype html>
<html>
<head>
  <title>2023 Cirrus SR20-G6 Piston Single Aircraft</title>
  <meta name="description" content="Low time SR20 with Garmin Perspective+ avionics.">
  <script type="application/ld+json">
  {
    "@type": "Product",
    "name": "2023 Cirrus SR20-G6",
    "offers": {
      "price": "579000",
      "priceCurrency": "USD"
    }
  }
  </script>
</head>
<body>
  <h1>2023 Cirrus SR20-G6</h1>
  <dl>
    <dt>Total Time</dt><dd>75</dd>
    <dt>Engine Time</dt><dd>75 SNEW</dd>
    <dt>Propeller Time</dt><dd>75 SNEW</dd>
    <dt>Serial Number</dt><dd>1234</dd>
  </dl>
  <p>Equipped with Garmin Perspective+ and Garmin GFC 700 autopilot.</p>
</body>
</html>
"""

GLOBALAIR_HTML = """
<!doctype html>
<html>
<head>
  <title>2022 CIRRUS SR22T G6 SN: 8922 for Sale - Specs & Images</title>
  <meta name="description" content="2022 Cirrus SR22T G6 aircraft listing.">
  <script>window.analytics = {"engine": 22};</script>
</head>
<body>
  <nav>Single Engine Pistons</nav>
  <h1>2022 Cirrus SR22T G6 SN: 8922 for Sale</h1>
  <p>Price: $799,000</p>
  <section>
    <h3>2022 Cirrus SR22T G6</h3>
    <p>Year:</p><p>2022</p>
    <p>Manufacturer:</p><p>Cirrus Aircraft</p>
    <p>Serial Number:</p><p>8922</p>
    <p>Registration No:</p><p>N317JT</p>
    <p>Total Time:</p><p>810 hrs</p>
    <p>Price:</p><p>$799,000 USD</p>
  </section>
  <section>
    <h3>Airframe</h3>
    <p>TTSN: 810</p>
    <h3>Engine(s)</h3>
    <p>TSIO-550-K, 315 HP Turbocharged</p>
    <p>TTSN: 810</p>
    <p>TBO: 2,200</p>
    <h3>Prop Details</h3>
    <p>Hartzell Three-Blade Composite Propeller</p>
    <p>TTSN: 771</p>
  </section>
  <section>
    <h3>Avionics</h3>
    <p>Package: Cirrus Perspective+ by Garmin Avionics Suite</p>
    <p>Garmin GFC-700 Digital Autopilot</p>
    <p>Garmin GMA-350c All-Digital Audio Panel</p>
    <p>Garmin GTX 345 ADS-B In & Out Transponder</p>
  </section>
</body>
</html>
"""


def sr20_model_output():
    return {
        "manufacturer": "Cirrus Aircraft",
        "model": "SR20",
        "variant": "SR20 G6",
        "model_year": 2023,
        "asking_price_usd": "579000",
        "currency": "USD",
        "airframe_hours": 75,
        "engine_hours": 75,
        "propeller_hours": 75,
        "listing_title": "2023 Cirrus SR20-G6 Piston Single Aircraft",
        "registration_number": None,
        "serial_number": "1234",
        "status": "active",
        "avionics": [
            {
                "manufacturer": "Garmin",
                "model": "Perspective+",
                "type": "Integrated Flight Deck",
                "quantity": 1,
            },
            {
                "manufacturer": "Garmin",
                "model": "GFC 700",
                "type": "Autopilot",
                "quantity": 1,
            },
        ],
    }


def globalair_sr22t_model_output():
    return {
        "manufacturer": "Cirrus Aircraft",
        "model": "SR22T",
        "variant": "SR22T G6",
        "model_year": 2022,
        "asking_price_usd": "799,000",
        "currency": "USD",
        "airframe_hours": 810,
        "engine_hours": 810,
        "propeller_hours": 771,
        "listing_title": "2022 Cirrus SR22T G6 SN: 8922 for Sale",
        "registration_number": "N317JT",
        "serial_number": "8922",
        "status": "active",
        "avionics": [
            {
                "manufacturer": "Garmin",
                "model": "Perspective+",
                "type": "Integrated Flight Deck",
                "quantity": 1,
            },
            {
                "manufacturer": "Garmin",
                "model": "GFC 700",
                "type": "Autopilot",
                "quantity": 1,
            },
            {
                "manufacturer": "Garmin",
                "model": "GMA 350c",
                "type": "Audio Panel",
                "quantity": 1,
            },
            {
                "manufacturer": "Garmin",
                "model": "GTX 345",
                "type": "Transponder",
                "quantity": 1,
            },
        ],
    }


def globalair_sr22_model_output_without_registration():
    output = globalair_sr22t_model_output()
    output["model"] = "SR22"
    output["variant"] = "G6 GTS"
    output["registration_number"] = "8680"
    output["serial_number"] = "8680"
    return output


class ListingParserTests(unittest.TestCase):
    def test_normalizes_known_manufacturer_aliases(self):
        self.assertEqual(normalize_name("Cessna Aircraft Company"), "cessna")
        self.assertEqual(normalize_name("Cirrus Aircraft"), "cirrus")
        self.assertEqual(normalize_name("SR22T-G6"), "sr22t g6")

    def test_cleans_listing_html_to_text(self):
        text = clean_listing_html(GLOBALAIR_HTML)

        self.assertIn("2022 CIRRUS SR22T G6 SN: 8922", text)
        self.assertIn("Registration No:", text)
        self.assertIn("TTSN: 771", text)
        self.assertIn("Garmin GFC-700 Digital Autopilot", text)
        self.assertNotIn("window.analytics", text)
        self.assertNotIn("<h1>", text)

    def test_parses_sr20_listing_preview_with_injected_extractor_output(self):
        def extractor(text):
            self.assertIn("2023 Cirrus SR20-G6", text)
            self.assertIn("Garmin Perspective+", text)
            return sr20_model_output()

        preview = parse_listing_html(
            source_url="https://example.test/sr20",
            html=SR20_HTML,
            extractor=extractor,
        )
        listing = preview.parsed_listing

        self.assertEqual(listing.manufacturer, "Cirrus")
        self.assertEqual(listing.model, "SR20")
        self.assertEqual(listing.variant, "SR20 G6")
        self.assertEqual(listing.model_year, 2023)
        self.assertEqual(listing.asking_price_usd, 579000)
        self.assertEqual(listing.currency, "USD")
        self.assertEqual(listing.airframe_hours, 75)
        self.assertEqual(listing.engine_hours, 75)
        self.assertEqual(listing.propeller_hours, 75)
        self.assertEqual(listing.serial_number, "1234")
        self.assertEqual(
            {(item.manufacturer, item.model, item.type) for item in listing.avionics},
            {
                ("Garmin", "Perspective+", "Integrated Flight Deck"),
                ("Garmin", "GFC 700", "Autopilot"),
            },
        )
        self.assertEqual(preview.warnings, [])

    def test_parses_globalair_listing_preview_with_injected_extractor_output(self):
        def extractor(text):
            self.assertIn("Engine(s)", text)
            self.assertIn("TBO: 2,200", text)
            return globalair_sr22t_model_output()

        preview = parse_listing_html(
            source_url="https://www.globalair.com/aircraft-for-sale/listing-detail/2022-cirrus-sr22t-g6-singles/140549",
            html=GLOBALAIR_HTML,
            extractor=extractor,
        )
        listing = preview.parsed_listing

        self.assertEqual(listing.manufacturer, "Cirrus")
        self.assertEqual(listing.model, "SR22T")
        self.assertEqual(listing.variant, "SR22T G6")
        self.assertEqual(listing.model_year, 2022)
        self.assertEqual(listing.asking_price_usd, 799000)
        self.assertEqual(listing.airframe_hours, 810)
        self.assertEqual(listing.engine_hours, 810)
        self.assertEqual(listing.propeller_hours, 771)
        self.assertEqual(listing.registration_number, "N317JT")
        self.assertEqual(listing.serial_number, "8922")
        self.assertEqual(
            {(item.manufacturer, item.model, item.type) for item in listing.avionics},
            {
                ("Garmin", "Perspective+", "Integrated Flight Deck"),
                ("Garmin", "GFC 700", "Autopilot"),
                ("Garmin", "GMA 350c", "Audio Panel"),
                ("Garmin", "GTX 345", "Transponder"),
            },
        )
        self.assertEqual(preview.warnings, [])

    def test_does_not_reuse_serial_number_as_registration(self):
        preview = parse_listing_html(
            source_url="https://example.test/sr22",
            html=GLOBALAIR_HTML,
            extractor=lambda _: globalair_sr22_model_output_without_registration(),
        )

        self.assertIsNone(preview.parsed_listing.registration_number)
        self.assertEqual(preview.parsed_listing.serial_number, "8680")
        self.assertEqual(preview.parsed_listing.variant, "SR22 G6 GTS")

    def test_manual_preview_is_unsourced_and_warned(self):
        preview = preview_manual_listing(
            {
                "manufacturer": "Cirrus",
                "model": "SR22T",
                "variant": "SR22T G6",
                "model_year": 2023,
                "asking_price_usd": 950000,
                "airframe_hours": 170,
                "engine_hours": 170,
                "propeller_hours": 170,
                "avionics": [
                    {
                        "manufacturer": "Garmin",
                        "model": "Perspective+",
                        "type": "Integrated Flight Deck",
                    }
                ],
            }
        )

        self.assertIsNone(preview.source_url)
        self.assertIn(
            "manual listing has no source URL and will be created as invalid",
            preview.warnings,
        )
        self.assertEqual(preview.parsed_listing.manufacturer, "Cirrus")
        self.assertEqual(preview.parsed_listing.avionics[0].model, "Perspective+")

    def test_fetch_url_uses_requests_with_certifi(self):
        response = Mock()
        response.encoding = "utf-8"
        response.text = "<html></html>"

        with patch.object(listing_parser.requests, "get", return_value=response) as get:
            html = fetch_url("https://example.test/listing", timeout_seconds=3)

        self.assertEqual(html, "<html></html>")
        response.raise_for_status.assert_called_once_with()
        get.assert_called_once()
        self.assertEqual(get.call_args.kwargs["verify"], certifi.where())
        self.assertEqual(get.call_args.kwargs["timeout"], 3)

    def test_default_gemini_extractor_requires_api_key(self):
        with patch.dict("os.environ", {}, clear=True):
            with self.assertRaises(ListingExtractorUnavailable):
                listing_parser.GeminiListingExtractor.from_environment()

    def test_default_gemini_extractor_reads_api_key_from_environment(self):
        with patch.dict("os.environ", {"GEMINI_API_KEY": " test-key "}, clear=True):
            extractor = listing_parser.GeminiListingExtractor.from_environment()

        self.assertEqual(extractor._api_key, "test-key")

    def test_gemini_extractor_posts_structured_json_request(self):
        response = Mock()
        response.json.return_value = {
            "candidates": [
                {
                    "content": {
                        "parts": [
                            {"text": json.dumps(sr20_model_output())},
                        ]
                    }
                }
            ]
        }
        extractor = GeminiListingExtractor(api_key="test-key", model="gemini-3.5-flash")

        with patch.object(listing_parser.requests, "post", return_value=response) as post:
            parsed = extractor("2023 Cirrus SR20-G6 Total Time 75")

        self.assertEqual(parsed["model"], "SR20")
        response.raise_for_status.assert_called_once_with()
        post.assert_called_once()
        self.assertEqual(post.call_args.kwargs["headers"]["x-goog-api-key"], "test-key")
        self.assertEqual(
            post.call_args.kwargs["json"]["generationConfig"]["responseMimeType"],
            "application/json",
        )
        self.assertIn(
            "responseSchema",
            post.call_args.kwargs["json"]["generationConfig"],
        )


if __name__ == "__main__":
    unittest.main()
