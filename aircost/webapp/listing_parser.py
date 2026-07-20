"""Preview parsing for aircraft sale listing pages."""

from __future__ import annotations

import json
import os
import re
import threading
from dataclasses import asdict, dataclass
from html import unescape
from html.parser import HTMLParser
from typing import Any, Callable
from urllib.parse import urlparse

import certifi
import requests


FetchUrl = Callable[[str], str]
ExtractListing = Callable[[str], dict[str, Any]]


class ListingFetchError(Exception):
    """Raised when a listing URL cannot be fetched."""


class ListingExtractionError(Exception):
    """Raised when listing text cannot be converted into structured fields."""


class ListingExtractorUnavailable(ListingExtractionError):
    """Raised when the listing extraction service is not configured."""


_LEGAL_SUFFIXES = {
    "co",
    "company",
    "corp",
    "corporation",
    "inc",
    "incorporated",
    "llc",
    "ltd",
    "limited",
}

_MANUFACTURER_ALIASES = {
    "cessna aircraft": "cessna",
    "cessna aircraft company": "cessna",
    "cirrus aircraft": "cirrus",
    "cirrus design": "cirrus",
    "the air plane factory": "sling",
    "sling aircraft": "sling",
    "sling airplane": "sling",
    "textron aviation": "cessna",
}

_MANUFACTURER_DISPLAY_NAMES = {
    "cessna": "Cessna",
    "cirrus": "Cirrus",
    "sling": "Sling",
}

_DEFAULT_MAX_LISTING_TEXT_CHARACTERS = 24_000
_DEFAULT_GEMINI_MODEL = "gemini-3.5-flash"
_DEFAULT_GEMINI_MAX_OUTPUT_TOKENS = 1800
_DEFAULT_GEMINI_TIMEOUT_SECONDS = 60.0
_DEFAULT_GEMINI_THINKING_LEVEL = "low"
_DEFAULT_EXTRACTOR: "GeminiListingExtractor | None" = None
_DEFAULT_EXTRACTOR_LOCK = threading.Lock()

_EXTRACTION_SCHEMA_DESCRIPTION = {
    "manufacturer": "string or null",
    "model": "string or null",
    "variant": "string or null",
    "model_year": "integer or null",
    "asking_price_usd": "number or null",
    "currency": "three-letter currency code, usually USD",
    "airframe_hours": "number or null",
    "engine_hours": "number or null",
    "propeller_hours": "number or null",
    "listing_title": "string or null",
    "registration_number": "string or null",
    "serial_number": "string or null",
    "status": "active, sold, pending, or unknown",
    "avionics": [
        {
            "manufacturer": "string",
            "model": "string",
            "type": "GPS, NAV/COM, Transponder, Autopilot, Integrated Flight Deck, Audio Panel, Flight Display, Traffic, Datalink, Engine Monitor, or Unknown",
            "quantity": "integer",
            "notes": "string or null",
        }
    ],
}

_GEMINI_RESPONSE_SCHEMA = {
    "type": "object",
    "properties": {
        "manufacturer": {"type": "string", "nullable": True},
        "model": {"type": "string", "nullable": True},
        "variant": {"type": "string", "nullable": True},
        "model_year": {"type": "integer", "nullable": True},
        "asking_price_usd": {"type": "number", "nullable": True},
        "currency": {"type": "string", "nullable": True},
        "airframe_hours": {"type": "number", "nullable": True},
        "engine_hours": {"type": "number", "nullable": True},
        "propeller_hours": {"type": "number", "nullable": True},
        "listing_title": {"type": "string", "nullable": True},
        "registration_number": {"type": "string", "nullable": True},
        "serial_number": {"type": "string", "nullable": True},
        "status": {
            "type": "string",
            "enum": ["active", "sold", "pending", "unknown"],
            "nullable": True,
        },
        "avionics": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "manufacturer": {"type": "string"},
                    "model": {"type": "string"},
                    "type": {"type": "string"},
                    "quantity": {"type": "integer"},
                    "notes": {"type": "string", "nullable": True},
                },
                "required": ["manufacturer", "model", "type", "quantity", "notes"],
                "propertyOrdering": [
                    "manufacturer",
                    "model",
                    "type",
                    "quantity",
                    "notes",
                ],
            },
        },
    },
    "required": [
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
        "avionics",
    ],
    "propertyOrdering": [
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
        "avionics",
    ],
}

_SYSTEM_PROMPT = (
    "You extract aircraft sale listing fields from plain text. "
    "Return only a single valid JSON object with the requested keys. "
    "Use null when a value is absent or ambiguous."
)


@dataclass(frozen=True)
class ParsedAvionics:
    manufacturer: str
    model: str
    type: str
    quantity: int = 1
    notes: str | None = None


@dataclass(frozen=True)
class ParsedListing:
    manufacturer: str | None
    model: str | None
    variant: str | None
    model_year: int | None
    asking_price_usd: float | None
    currency: str
    airframe_hours: float | None
    engine_hours: float | None
    propeller_hours: float | None
    listing_title: str | None
    registration_number: str | None
    serial_number: str | None
    status: str
    avionics: list[ParsedAvionics]


@dataclass(frozen=True)
class ListingPreview:
    source_url: str | None
    parsed_listing: ParsedListing
    warnings: list[str]

    def to_dict(self) -> dict:
        return asdict(self)


def preview_listing_url(
    source_url: str,
    *,
    fetcher: FetchUrl | None = None,
    extractor: ExtractListing | None = None,
) -> ListingPreview:
    """Fetch and parse a listing URL without writing to the database."""

    _validate_source_url(source_url)
    html = (fetcher or fetch_url)(source_url)
    return parse_listing_html(source_url=source_url, html=html, extractor=extractor)


def preview_manual_listing(listing: dict[str, Any]) -> ListingPreview:
    """Normalize a manual listing payload into the preview response shape."""

    warnings = ["manual listing has no source URL and will be created as invalid"]
    parsed = ParsedListing(
        manufacturer=_optional_string(listing.get("manufacturer")),
        model=_optional_string(listing.get("model")),
        variant=_optional_string(listing.get("variant")),
        model_year=_optional_int(listing.get("model_year")),
        asking_price_usd=_optional_float(listing.get("asking_price_usd")),
        currency=_optional_string(listing.get("currency")) or "USD",
        airframe_hours=_optional_float(listing.get("airframe_hours")),
        engine_hours=_optional_float(listing.get("engine_hours")),
        propeller_hours=_optional_float(listing.get("propeller_hours")),
        listing_title=_optional_string(listing.get("listing_title")),
        registration_number=_optional_string(listing.get("registration_number")),
        serial_number=_optional_string(listing.get("serial_number")),
        status=_optional_string(listing.get("status")) or "active",
        avionics=_manual_avionics(listing.get("avionics")),
    )
    warnings.extend(_missing_field_warnings(parsed))
    return ListingPreview(
        source_url=None,
        parsed_listing=parsed,
        warnings=warnings,
    )


def parse_listing_html(
    *,
    source_url: str,
    html: str,
    extractor: ExtractListing | None = None,
) -> ListingPreview:
    """Parse a sale listing page using cleaned text and an LLM extractor."""

    listing_text = clean_listing_html(html)
    structured = (extractor or default_listing_extractor())(listing_text)
    parsed = _parsed_listing_from_model_output(structured)
    warnings: list[str] = []
    warnings.extend(_missing_field_warnings(parsed))
    return ListingPreview(
        source_url=source_url,
        parsed_listing=parsed,
        warnings=warnings,
    )


def clean_listing_html(
    html: str,
    *,
    max_characters: int = _DEFAULT_MAX_LISTING_TEXT_CHARACTERS,
) -> str:
    """Convert listing HTML into compact plain text for LLM extraction."""

    extractor = _ListingHtmlExtractor()
    extractor.feed(html)
    json_ld_objects = _parse_json_ld(extractor.json_ld_scripts)
    candidates = [
        *extractor.title_parts,
        *extractor.meta_values,
        _json_text(json_ld_objects),
        *extractor.visible_text,
    ]
    lines: list[str] = []
    previous: str | None = None
    for candidate in candidates:
        for line in candidate.splitlines():
            cleaned = _normalized_page_text(line)
            if cleaned and cleaned != previous:
                lines.append(cleaned)
                previous = cleaned
    return _trim_listing_text("\n".join(lines), max_characters=max_characters)


def default_listing_extractor() -> ExtractListing:
    """Return the process-wide Gemini listing extractor."""

    global _DEFAULT_EXTRACTOR
    with _DEFAULT_EXTRACTOR_LOCK:
        if _DEFAULT_EXTRACTOR is None:
            _DEFAULT_EXTRACTOR = GeminiListingExtractor.from_environment()
        return _DEFAULT_EXTRACTOR


def _trim_listing_text(text: str, *, max_characters: int) -> str:
    if len(text) <= max_characters:
        return text
    anchor = re.search(
        r"\b(?:19[7-9]\d|20[0-3]\d)\b.{0,80}\b(?:cirrus|cessna|sling|sr20|sr22|sr22t|t182t)\b",
        text,
        flags=re.IGNORECASE | re.DOTALL,
    )
    if anchor is None:
        anchor = re.search(r"\bprice\s*:", text, flags=re.IGNORECASE)
    if anchor is None:
        return text[:max_characters]
    start = max(0, anchor.start() - 1000)
    return text[start : start + max_characters]


def _build_extraction_prompt(listing_text: str) -> str:
    return (
        "Extract these fields from the aircraft sale listing text.\n"
        "Return JSON with exactly this shape:\n"
        f"{json.dumps(_EXTRACTION_SCHEMA_DESCRIPTION, indent=2)}\n\n"
        "Rules:\n"
        "- Use values only when they appear in the listing text.\n"
        "- asking_price_usd must be the aircraft asking price, not a loan payment.\n"
        "- model_year must be the aircraft model year, not an inspection or warranty date.\n"
        "- airframe_hours is total time, TTAF, TT, TTSN, or flight hours since new.\n"
        "- engine_hours is engine TTSN/SNEW/SMOH/SFRM time, not horsepower, TBO, or engine model.\n"
        "- propeller_hours is propeller TTSN/SNEW/SMOH/SPOH time, not blade count or model.\n"
        "- If engine or propeller time is absent, leave it null unless the text explicitly says all times are since new.\n"
        "- registration_number may be an N-number or another registration value from Registration No/Reg/RN.\n"
        "- avionics must come from the listing text and should include fixed installed avionics only.\n"
        "- Do not include explanations, markdown, comments, or extra keys.\n\n"
        "Listing text:\n"
        f"{listing_text}"
    )


def _load_model_json(content: str) -> dict[str, Any]:
    try:
        parsed = json.loads(content)
    except json.JSONDecodeError:
        match = re.search(r"\{.*\}", content, flags=re.DOTALL)
        if not match:
            raise ListingExtractionError("Gemini did not return JSON")
        try:
            parsed = json.loads(match.group(0))
        except json.JSONDecodeError as exc:
            raise ListingExtractionError("Gemini returned invalid JSON") from exc
    if not isinstance(parsed, dict):
        raise ListingExtractionError("Gemini JSON response must be an object")
    return parsed


def _parsed_listing_from_model_output(value: dict[str, Any]) -> ParsedListing:
    data = value.get("parsed_listing") if isinstance(value.get("parsed_listing"), dict) else value
    manufacturer = _optional_string(data.get("manufacturer"))
    if manufacturer:
        manufacturer = canonical_manufacturer_name(manufacturer)
    model = _optional_string(data.get("model"))
    variant = _normalize_variant(model, _optional_string(data.get("variant")))
    registration_number = _optional_string(data.get("registration_number"))
    serial_number = _optional_string(data.get("serial_number"))
    if (
        registration_number
        and serial_number
        and normalize_name(registration_number) == normalize_name(serial_number)
        and not registration_number.upper().startswith("N")
    ):
        registration_number = None
    currency = (_optional_string(data.get("currency")) or "USD").upper()
    return ParsedListing(
        manufacturer=manufacturer,
        model=model,
        variant=variant,
        model_year=_optional_int_in_range(data.get("model_year"), 1900, 2039),
        asking_price_usd=_optional_float_min(data.get("asking_price_usd"), 10_000),
        currency=currency,
        airframe_hours=_optional_nonnegative_float(data.get("airframe_hours")),
        engine_hours=_optional_nonnegative_float(data.get("engine_hours")),
        propeller_hours=_optional_nonnegative_float(data.get("propeller_hours")),
        listing_title=_optional_string(data.get("listing_title")),
        registration_number=registration_number,
        serial_number=serial_number,
        status=_optional_string(data.get("status")) or "active",
        avionics=_model_avionics(data.get("avionics")),
    )


def _normalize_variant(model: str | None, variant: str | None) -> str | None:
    if not model or not variant:
        return variant
    normalized_model = normalize_name(model)
    normalized_variant = normalize_name(variant)
    if normalized_model in {"sr20", "sr22", "sr22t"} and normalized_variant.startswith("g"):
        return f"{model.upper()} {variant}"
    return variant


def _model_avionics(value: Any) -> list[ParsedAvionics]:
    if not isinstance(value, list):
        return []
    avionics: list[ParsedAvionics] = []
    seen: set[tuple[str, str, str]] = set()
    for item in value:
        if not isinstance(item, dict):
            continue
        manufacturer = _optional_string(item.get("manufacturer"))
        model = _optional_string(item.get("model"))
        if not manufacturer or not model:
            continue
        avionics_type = _optional_string(item.get("type")) or "Unknown"
        key = (normalize_name(manufacturer), normalize_name(model), normalize_name(avionics_type))
        if key in seen:
            continue
        seen.add(key)
        avionics.append(
            ParsedAvionics(
                manufacturer=canonical_manufacturer_name(manufacturer),
                model=model,
                type=avionics_type,
                quantity=_optional_int_min(item.get("quantity"), 1) or 1,
                notes=_optional_string(item.get("notes")),
            )
        )
    return avionics


def fetch_url(source_url: str, *, timeout_seconds: float = 15.0) -> str:
    """Fetch a URL as text using requests and certifi CA roots."""

    _validate_source_url(source_url)
    try:
        response = requests.get(
            source_url,
            headers={
                "Accept": (
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
                ),
                "User-Agent": "aircost-listing-preview/0.1",
            },
            timeout=timeout_seconds,
            verify=certifi.where(),
        )
        response.raise_for_status()
    except requests.RequestException as exc:
        raise ListingFetchError(f"could not fetch source_url: {exc}") from exc
    if not response.encoding:
        response.encoding = response.apparent_encoding
    return response.text


class GeminiListingExtractor:
    """LLM-backed listing extractor using the Gemini REST API."""

    def __init__(
        self,
        *,
        api_key: str,
        model: str = _DEFAULT_GEMINI_MODEL,
        max_output_tokens: int = _DEFAULT_GEMINI_MAX_OUTPUT_TOKENS,
        timeout_seconds: float = _DEFAULT_GEMINI_TIMEOUT_SECONDS,
        thinking_level: str | None = _DEFAULT_GEMINI_THINKING_LEVEL,
    ) -> None:
        api_key = api_key.strip()
        if not api_key:
            raise ListingExtractorUnavailable(
                "GEMINI_API_KEY must be set to use Gemini listing extraction"
            )

        model_path = model if model.startswith("models/") else f"models/{model}"
        self._url = (
            "https://generativelanguage.googleapis.com/v1beta/"
            f"{model_path}:generateContent"
        )
        self._api_key = api_key
        self._max_output_tokens = max_output_tokens
        self._timeout_seconds = timeout_seconds
        self._thinking_level = thinking_level

    @classmethod
    def from_environment(cls) -> "GeminiListingExtractor":
        return cls(
            api_key=_required_environment_string("GEMINI_API_KEY"),
            model=os.environ.get("AIRCOST_GEMINI_MODEL", _DEFAULT_GEMINI_MODEL),
            max_output_tokens=_environment_int(
                "AIRCOST_GEMINI_MAX_OUTPUT_TOKENS",
                _DEFAULT_GEMINI_MAX_OUTPUT_TOKENS,
            ),
            timeout_seconds=_environment_float(
                "AIRCOST_GEMINI_TIMEOUT_SECONDS",
                _DEFAULT_GEMINI_TIMEOUT_SECONDS,
            ),
            thinking_level=os.environ.get(
                "AIRCOST_GEMINI_THINKING_LEVEL",
                _DEFAULT_GEMINI_THINKING_LEVEL,
            ),
        )

    def __call__(self, listing_text: str) -> dict[str, Any]:
        generation_config: dict[str, Any] = {
            "responseMimeType": "application/json",
            "responseSchema": _GEMINI_RESPONSE_SCHEMA,
            "maxOutputTokens": self._max_output_tokens,
        }
        if self._thinking_level:
            generation_config["thinkingConfig"] = {
                "thinkingLevel": self._thinking_level,
            }

        payload = {
            "contents": [
                {
                    "role": "user",
                    "parts": [
                        {
                            "text": (
                                f"{_SYSTEM_PROMPT}\n\n"
                                f"{_build_extraction_prompt(listing_text)}"
                            )
                        }
                    ],
                }
            ],
            "generationConfig": generation_config,
        }
        try:
            response = requests.post(
                self._url,
                headers={
                    "Content-Type": "application/json",
                    "x-goog-api-key": self._api_key,
                },
                json=payload,
                timeout=self._timeout_seconds,
                verify=certifi.where(),
            )
            response.raise_for_status()
        except requests.RequestException as exc:
            raise ListingExtractionError(f"Gemini extraction failed: {exc}") from exc

        try:
            response_payload = response.json()
        except ValueError as exc:
            raise ListingExtractionError("Gemini returned a non-JSON response") from exc

        content = _gemini_response_text(response_payload)
        return _load_model_json(content)


def _gemini_response_text(response_payload: dict[str, Any]) -> str:
    candidates = response_payload.get("candidates")
    if not isinstance(candidates, list) or not candidates:
        raise ListingExtractionError("Gemini response did not include candidates")
    content = candidates[0].get("content") if isinstance(candidates[0], dict) else None
    parts = content.get("parts") if isinstance(content, dict) else None
    if not isinstance(parts, list):
        raise ListingExtractionError("Gemini response did not include content parts")
    text_parts = [
        part.get("text")
        for part in parts
        if isinstance(part, dict) and isinstance(part.get("text"), str)
    ]
    text = "\n".join(text_parts).strip()
    if not text:
        raise ListingExtractionError("Gemini response did not include text content")
    return text


def normalize_name(value: str) -> str:
    """Normalize a maker, model, or variant name for stable DB uniqueness keys."""

    value = unescape(value).lower()
    value = re.sub(r"[^a-z0-9]+", " ", value)
    parts = [part for part in value.split() if part not in _LEGAL_SUFFIXES]
    normalized = " ".join(parts).strip()
    return _MANUFACTURER_ALIASES.get(normalized, normalized)


def canonical_manufacturer_name(value: str) -> str:
    """Return a display manufacturer name after alias normalization."""

    normalized = normalize_name(value)
    return _MANUFACTURER_DISPLAY_NAMES.get(normalized, value.strip())


class _ListingHtmlExtractor(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.visible_text: list[str] = []
        self.title_parts: list[str] = []
        self.meta_values: list[str] = []
        self.json_ld_scripts: list[str] = []
        self._capture_title = False
        self._capture_json_ld = False
        self._json_ld_chunks: list[str] = []
        self._ignored_depth = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attr_map = {name.lower(): value or "" for name, value in attrs}
        if tag in {"style", "noscript"}:
            self._ignored_depth += 1
            return
        if tag == "script":
            script_type = attr_map.get("type", "").lower()
            self._capture_json_ld = script_type == "application/ld+json"
            if not self._capture_json_ld:
                self._ignored_depth += 1
            return
        if tag == "title":
            self._capture_title = True
            return
        if tag == "meta":
            value = attr_map.get("content")
            name = attr_map.get("name", "").lower()
            prop = attr_map.get("property", "").lower()
            if value and name in {"description", "title"}:
                self.meta_values.append(value)
            elif value and prop in {"og:title", "og:description"}:
                self.meta_values.append(value)

    def handle_endtag(self, tag: str) -> None:
        if tag in {"style", "noscript"} and self._ignored_depth:
            self._ignored_depth -= 1
        elif tag == "script":
            if self._capture_json_ld:
                self.json_ld_scripts.append("".join(self._json_ld_chunks))
                self._json_ld_chunks = []
                self._capture_json_ld = False
            elif self._ignored_depth:
                self._ignored_depth -= 1
        elif tag == "title":
            self._capture_title = False

    def handle_data(self, data: str) -> None:
        if self._capture_json_ld:
            self._json_ld_chunks.append(data)
        elif self._capture_title:
            self.title_parts.append(data)
        elif not self._ignored_depth:
            self.visible_text.append(data)


def _parse_json_ld(scripts: list[str]) -> list[Any]:
    objects: list[Any] = []
    for script in scripts:
        try:
            objects.append(json.loads(script))
        except json.JSONDecodeError:
            continue
    return objects


def _json_text(values: list[Any]) -> str:
    strings: list[str] = []

    def visit(value: Any) -> None:
        if isinstance(value, dict):
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)
        elif isinstance(value, str):
            strings.append(value)

    visit(values)
    return " ".join(strings)


def _normalized_page_text(value: str) -> str:
    return re.sub(r"\s+", " ", unescape(value)).strip()


def _manual_avionics(value: Any) -> list[ParsedAvionics]:
    if not isinstance(value, list):
        return []
    avionics: list[ParsedAvionics] = []
    for item in value:
        if not isinstance(item, dict):
            continue
        manufacturer = _optional_string(item.get("manufacturer"))
        model = _optional_string(item.get("model"))
        avionics_type = _optional_string(item.get("type"))
        if manufacturer and model and avionics_type:
            avionics.append(
                ParsedAvionics(
                    manufacturer=manufacturer,
                    model=model,
                    type=avionics_type,
                    quantity=_optional_int(item.get("quantity")) or 1,
                    notes=_optional_string(item.get("notes")),
                )
            )
    return avionics


def _missing_field_warnings(parsed: ParsedListing) -> list[str]:
    warnings: list[str] = []
    for field_name in (
        "manufacturer",
        "model",
        "variant",
        "model_year",
        "asking_price_usd",
        "airframe_hours",
        "engine_hours",
        "propeller_hours",
    ):
        if getattr(parsed, field_name) is None:
            warnings.append(f"{field_name} not found")
    if not parsed.avionics:
        warnings.append("avionics not found")
    return warnings


def _validate_source_url(source_url: str) -> None:
    parsed = urlparse(source_url)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise ValueError("source_url must be an absolute http or https URL")


def _optional_string(value: Any) -> str | None:
    if value is None:
        return None
    if isinstance(value, str):
        stripped = value.strip()
        return stripped or None
    return str(value)


def _optional_float(value: Any) -> float | None:
    if value is None or value == "":
        return None
    if isinstance(value, str):
        value = value.replace(",", "").replace("$", "").strip()
        if not value:
            return None
    return float(value)


def _optional_int(value: Any) -> int | None:
    if value is None or value == "":
        return None
    if isinstance(value, str):
        value = value.replace(",", "").strip()
        if not value:
            return None
    return int(float(value))


def _optional_nonnegative_float(value: Any) -> float | None:
    number = _optional_float_or_none(value)
    if number is None or number < 0:
        return None
    return number


def _optional_float_min(value: Any, minimum: float) -> float | None:
    number = _optional_float_or_none(value)
    if number is None or number < minimum:
        return None
    return number


def _optional_int_min(value: Any, minimum: int) -> int | None:
    number = _optional_int_or_none(value)
    if number is None or number < minimum:
        return None
    return number


def _optional_int_in_range(value: Any, minimum: int, maximum: int) -> int | None:
    number = _optional_int_or_none(value)
    if number is None or number < minimum or number > maximum:
        return None
    return number


def _optional_float_or_none(value: Any) -> float | None:
    try:
        return _optional_float(value)
    except (TypeError, ValueError):
        return None


def _optional_int_or_none(value: Any) -> int | None:
    try:
        return _optional_int(value)
    except (TypeError, ValueError):
        return None


def _required_environment_string(name: str) -> str:
    value = os.environ.get(name)
    if value and value.strip():
        return value.strip()
    raise ListingExtractorUnavailable(f"{name} must be set")


def _environment_int(name: str, default: int) -> int:
    value = os.environ.get(name)
    if not value:
        return default
    try:
        return int(value)
    except ValueError as exc:
        raise ListingExtractorUnavailable(f"{name} must be an integer") from exc


def _environment_optional_int(name: str) -> int | None:
    value = os.environ.get(name)
    if not value:
        return None
    try:
        return int(value)
    except ValueError as exc:
        raise ListingExtractorUnavailable(f"{name} must be an integer") from exc


def _environment_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    if not value:
        return default
    try:
        return float(value)
    except ValueError as exc:
        raise ListingExtractorUnavailable(f"{name} must be a number") from exc
