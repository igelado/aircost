# Web Application

The web app starts with a SQLx-backed REST API for users, shared aircraft
reference data, sale listings, avionics, and rental reference data. It does not
store comparison studies or analysis results yet.

Run the development server:

```bash
cargo run --bin aircost-web
```

The server initializes `data/aircost.sqlite3` by default and seeds one local developer
user:

```text
developer@localhost
```

Requests default to that user. A request can also pass:

```http
X-User-Email: developer@localhost
```

The same header also accepts the dev auth subject:

```http
X-User-Email: developer
```

## Listing Preview

Preview parsing fetches or normalizes listing data without writing anything to
the database.

URL preview mode uses the Gemini API. The server first converts listing HTML to
compact plain text, then asks Gemini to return the listing fields as strict
JSON. Manual JSON mode does not use Gemini.

Set the API key in the environment before starting the server. For local
development, load the key from `~/.keys/gemini.key`:

```bash
GEMINI_API_KEY="$(tr -d '\n' < ~/.keys/gemini.key)" \
  cargo run --bin aircost-web
```

Optional server arguments:

```bash
cargo run --bin aircost-web -- \
  --host 127.0.0.1 \
  --port 8000 \
  --database data/aircost.sqlite3
```

`--database` accepts a SQLite file path. Use `--database-url` or
`AIRCOST_DATABASE_URL` to select a backend explicitly:

```text
sqlite://data/aircost.sqlite3
postgres://aircost:aircost@localhost/aircost
```

The Rust server uses axum for routing, tokio for the async runtime, eoka for
rendered listing fetches, reqwest for Gemini HTTP calls, sqlx for SQLite or
Postgres access, and scraper for HTML text extraction.

Optional tuning:

```text
AIRCOST_GEMINI_MODEL=gemini-3.1-flash-lite
AIRCOST_GEMINI_MAX_OUTPUT_TOKENS=1800
AIRCOST_GEMINI_TIMEOUT_SECONDS=60
AIRCOST_GEMINI_THINKING_LEVEL=low
```

## Browser-Rendered Fetching

Source URL previews use eoka out of the box. The server launches the browser
through eoka on the first URL fetch, reuses that browser for subsequent fetches,
opens one tab per listing page, waits briefly for JavaScript content to settle,
then extracts the rendered HTML and closes the tab.

Useful setting:

```text
AIRCOST_EOKA_SETTLE_MILLISECONDS=1200
```

```http
POST /api/listings/preview
Content-Type: application/json
```

URL mode:

```json
{
  "source_url": "https://example.com/listing"
}
```

Manual JSON mode:

```json
{
  "listing": {
    "manufacturer": "Cirrus",
    "model": "SR20",
    "model_year": 2023,
    "asking_price_usd": 579000,
    "airframe_hours": 75,
    "engine_hours": 75,
    "propeller_hours": 75,
    "avionics": [
      {
        "manufacturer": "Garmin",
        "model": "Perspective+",
        "type": "Integrated Flight Deck",
        "quantity": 1
      }
    ]
  }
}
```

URL mode returns parsed aircraft fields, avionics, and warnings. Manual JSON
mode returns the same response shape, but warns that the eventual listing will
be invalid because it has no source URL.

Only one of `source_url` and `listing` is allowed per request.

## Chrome Extension Capture

The unpacked Chrome extension in `chrome-extension/` submits rendered page HTML
from the user's browser instead of asking the server to fetch listing URLs.

Register the extension install:

```http
POST /api/plugin/register
Content-Type: application/json
X-User-Email: developer
```

```json
{
  "public_key_base64": "raw P-256 public key"
}
```

Submit the current page:

```http
POST /api/plugin/submissions
Content-Type: application/json
X-User-Email: developer
```

```json
{
  "plugin_install_id": 1,
  "source_url": "https://example.com/listing",
  "rendered_html": "<html>...</html>",
  "signature": "base64 ECDSA P-256 signature"
}
```

The signature is over:

```text
aircost-plugin-v1
plugin_install_id
source_url
sha256(rendered_html)
```

Retry extraction for a stored plugin submission:

```http
POST /api/plugin/submissions/{id}/reprocess
X-User-Email: developer
```

This reuses the rendered HTML already stored with the submission and updates
the submission with the latest extraction result and saved listing ID.

For local testing, open `chrome://extensions`, enable Developer Mode, choose
`Load unpacked`, and select `chrome-extension/`. The popup asks for the server
URL and username. Use `http://127.0.0.1:8001` and `developer` for the current
dev setup.

## Sale Listings

Create a listing from the same payload accepted by preview:

```http
POST /api/listings
Content-Type: application/json
```

```json
{
  "source_url": "https://example.com/listing"
}
```

or:

```json
{
  "listing": {
    "manufacturer": "Cirrus",
    "model": "SR20",
    "model_year": 2023,
    "asking_price_usd": 579000,
    "airframe_hours": 75,
    "engine_hours": 75,
    "propeller_hours": 75,
    "registration_number": "N12345",
    "serial_number": "1234",
    "avionics": []
  }
}
```

List visible listings:

```http
GET /api/listings
```

Fetch one listing:

```http
GET /api/listings/{id}
```

Update an unverified listing:

```http
PATCH /api/listings/{id}
Content-Type: application/json
```

```json
{
  "listing": {
    "asking_price_usd": 585000
  }
}
```

Delete an unverified listing:

```http
DELETE /api/listings/{id}
```

Listings have `is_verified` and `added_at`. New user-created listings start
with `is_verified: false`. Verified listings are globally visible and cannot be
updated or deleted through these user APIs. Unverified listings are visible only
to the user who created them.

When adding a listing with the same tail number:

- If the current user already has an unverified listing for that tail, the API
  updates that same row with the new values and refreshes `added_at`.
- If a verified listing for that tail has the same aircraft, price, hours,
  status, serial number, and avionics, the API refreshes `added_at` on
  the verified row.
- If a verified listing for that tail has different values, the API creates a
  new unverified row with the new values.
