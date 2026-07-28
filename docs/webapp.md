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

At startup the server loads one approved, hash-verified valuation artifact into
shared application state. If no artifact is active, an eligible newest frozen
snapshot with at least five valid deduplicated aircraft can provide an explicitly
uncalibrated adjusted-comparable fallback. A corrupt active structural artifact
is rejected rather than creating or repairing weights during a request; the
server then attempts the same eligible comparable fallback and otherwise marks
valuation unavailable.

`GET /api/valuation/status` reports `calibrated`, `comparable_fallback`, or
`unavailable`, plus calibration state, model kind/version, snapshot ID, and
warnings. `/health` includes the same object. The web header displays this
state continuously. When neither an approved artifact nor an eligible snapshot
exists, primary estimate fields remain null and `estimate_error` explains that
listing-only valuation is unavailable; the legacy compatibility estimator does
not silently fill those fields.

## Listing Preview

Preview parsing fetches or normalizes listing data without writing anything to
the database.

URL preview mode uses the Gemini API. The server first converts listing HTML to
compact plain text, then asks Gemini to return the listing fields as strict
JSON. Manual JSON mode does not use Gemini.

The extension popup hands each signed capture to its background service worker,
which owns the upload independently of the popup. Plugin uploads become
server-owned once the complete signed request has been received and
authenticated. Closing the popup therefore does not interrupt either the upload
or subsequent extraction, normalization, listing persistence, and valuation.
The service worker stores its latest progress locally, and a reopened popup
queries the server for the authoritative submission and listing state. It keeps
a bounded 24-hour history of up to 25 jobs so multiple captures can run at once
and their latest stages remain visible in the popup. The popup releases its
upload action as soon as the server accepts a page, allowing the user to move to
the next listing while extraction and normalization continue. An upload
interrupted before the complete request reaches the server must be retried, and
in-flight work is not preserved across a server process restart.

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
        "types": ["Integrated Flight Deck", "Flight Display"],
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
from the user's browser instead of asking the server to fetch listing URLs. The
popup captures and signs the page, then hands the signed payload to the
extension's background service worker. The service worker continues the upload
if the popup closes and persists per-upload progress for the next time the popup
opens. The recent-uploads panel shows the current stage of concurrent and
completed jobs.

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

The server currently admits only aircraft with U.S. N-numbers that match the
newest imported FAA registry projection. Creation and update fail before any
listing or catalog mutation when registration is missing, foreign, malformed,
not covered, absent, ambiguous, or conflicts with the supplied serial number.
Preview remains read-only and may still display extracted data that admission
will reject.

During creation or an explicit avionics replacement, the server automatically
discards an observation as garbage only for a high-confidence structured
`rejection_basis` and a basis-consistent, candidate-specific negative reason.
The entire normalized reason must appear in one linked Google Search citation
support span. A citation that only establishes product identity, contradicts
the negative claim, or is otherwise unrelated leaves the observation pending
for review instead of discarding it.

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

Avionics are replaced only when the PATCH body explicitly contains a valid
`avionics` array. Without that member, the server skips avionics identity
resolution and preserves the pending-review hashes and exact listing-link IDs;
price, status, hours, and similar edits do not silently restage the review.
Including `manufacturer`, `model`, `variant`, `model_year`, `source_url`,
`registration_number`, or `serial_number` requires an explicit avionics array
in the same request. Null, object-valued, or malformed avionics fail before any
mutation. `"avionics": []` deliberately clears the prior avionics set and its
pending evidence; a non-empty array replaces and restages it as necessary.

Delete an unverified listing:

```http
DELETE /api/listings/{id}
```

Listings have `is_verified` and `added_at`. A row is inserted unverified; a
source-backed listing becomes verified only after mandatory FAA admission,
avionics resolution, enrichment, and readiness checks all pass. Source-less
manual drafts remain unverified. Verified listings are globally visible and
cannot be updated or deleted through these user APIs. Unverified and
`pending_review` listings are visible only to the user who created them.

When adding a listing with the same tail number:

- If the current user already has an unverified listing for that tail, the API
  updates that same row with the new values and refreshes `added_at`.
- If a verified listing for that tail has the same aircraft, price, hours,
  status, serial number, and avionics, the API refreshes `added_at` on
  the verified row.
- If a verified listing for that tail has different values, the API creates a
  new unverified row with the new values.

Listing estimate responses include the point estimate, low/high range,
estimated error fraction, support grade, model kind/version, snapshot ID,
listing-only factor breakdown, and a constant-today-dollar value curve for
horizons zero through thirty. The listing-only path does not require aircraft
spec metadata or a model-year new-price record.

`valuation_calibrated` distinguishes approved structural/DNN artifacts from the
adjusted-comparable fallback, and `valuation_warning` carries any serving caveat
that should be shown with the estimate.

The listing table also displays each row's ingestion state. Hovering an
`incomplete` or `quarantined` badge shows its persisted completion error.
`pending_review` identifies expected curation work and has no ingestion error.
All three states are excluded from valuation and training rather than silently
treated as ready data.

## Listing Review Workspace

The Review tab has **By product** and **By listing** queues. By product
collapses every hash-bound existing-product aspect onto one approved avionics
identity. A product is attested once from a guarded OEM fetch, without Gemini,
then its listing aspects are checked locally with bounded concurrency across
listings and serial optimistic-lock updates within each listing. These aspects
may represent a preserved link or an ordinary unlinked extraction observation;
the browser uses the same verification workflow for both. By listing shows the
aircraft, tail, year, aspect count, reason groups, and last update. Opening a
listing shows every unresolved observation, its source context, and any
suggested or proposed product.

Review access has a server-side allowlist until durable application roles are
available. Production deployments must provide a comma-separated list of exact
reviewer emails:

```text
AIRCOST_REVIEWER_EMAILS=reviewer@example.com,second-reviewer@example.com
```

Debug builds also admit the seeded local `developer@localhost` user. A local
release build can opt in to that developer with
`AIRCOST_ALLOW_LOCAL_REVIEWER=true`; do not use that override as production
authorization. The store layer additionally scopes queue, detail, and resolve
operations to listings owned by the authenticated reviewer. The current web
authentication adapter trusts `X-User-Email`; production must expose these
routes only behind a trusted proxy that strips any client-supplied value and
injects the authenticated identity itself.

Ordinary extracted avionics aspects offer three decisions. The reviewer chooses
exactly one before the Verify Listing button is enabled:

- **Use verified product** searches and selects one existing approved catalog
  identity.
- **Create verified product** requires manufacturer, model, one or more
  canonical capabilities, a stable manufacturer identifier kind and value,
  and authoritative identity source URL, title, and evidence text.
- **Discard observation** requires a reason and creates neither a catalog row
  nor a listing association.

A hash-bound approved-product target is not a fourth decision type. Product
attestation and source-free occurrence verification are separate operations.
For a preserved link, successful local verification corroborates the existing
occurrence without rewriting it. For an ordinary installed, non-replacement
aspect, the same operation may create an exact-quantity association or update
its one covered installed link through the normal aspect-scoped
`use-existing` transaction. Until local verification succeeds, the aspect
remains pending and may only be explicitly discarded from the listing
workflow.

For an unlinked observation, an explicit legacy candidate means normalized
manufacturer/model selected one and only one `unreviewed` catalog row. An
aspect already tied to a legacy listing association may instead show that exact
covered catalog row by ID. Neither is preapproved: creating the same identity
can promote the row only after the server rechecks normalized-identity
uniqueness and identifier/model collisions under lock. Entering a corrected
manufacturer/model creates a separate verified product and leaves the old
candidate unchanged.

The corresponding API is:

```http
GET /api/review/listings?limit=25&offset=0
GET /api/review/listings/{listing_id}
GET /api/review/avionics/products?limit=25&cursor={opaque_cursor}
GET /api/review/avionics/products/{product_id}/associations?limit=25&cursor={opaque_cursor}
POST /api/review/avionics/products/{product_id}/attest
POST /api/review/listings/{listing_id}/avionics/verify-existing
POST /api/review/listings/{listing_id}/avionics/approve-replacement
POST /api/review/listings/{listing_id}/resolve
```

Product cursors are opaque keyset tokens ordered by immutable product ID.
Association cursors are bound to one product and ordered by listing and aspect.
The attestation request carries the catalog revision, one OEM source dossier,
and exactly one direct association authorization tuple:
`listing_id`, `review_payload_sha256`, and `aspect_id`. The server loads only
that listing review and requires the exact hash-bound aspect to target the
requested product; it does not scan other pending reviews for authorization.
The existing-product verification request carries only the canonical review
hash, catalog revision, and aspect ID; source fields and revision aliases are
rejected. It requires retained evidence to identify the hash-bound, currently
attested product uniquely in the live local catalog. The mutation lock
rechecks the review and catalog hashes, the complete active collision closure,
the target's current reuse eligibility, exact covered-link ownership, and the
listing action graph. Ordinary aspects must be installed, positive-quantity,
and independent of every replacement edge; they may cover zero or one
installed link. Attestation separately rechecks both the
manufacturer-scoped collision snapshot and ownership of the exact pending
aspect under the mutation lock.

The replacement endpoint is the only aspect-scoped path that accepts a
replacement relationship. Its strict source-free request names the staged
parent and child aspect, selected approved product ID, and exact staged
quantity for each. The parent must explicitly target the child; the child
quantity is one. Both products must already have current global reuse
attestations. The server changes one listing link atomically, preserves that
link's ID when the bundle covers an existing relationship, and performs no
Gemini or OEM fetch. It rejects half relationships, stale or cross-listing link
coverage, implicit association merges, quantity changes, and invalid action
graphs. The ordinary single-aspect `use-existing` route continues to reject
both sides of a replacement relationship.

The resolve request includes `review_payload_sha256`,
`catalog_revision_sha256`, one decision for every returned aspect, and an
optional `finalize_listing` boolean. Both hashes are optimistic concurrency
controls. `finalize_listing` defaults to `false`, so API and batch reviewers
can save a local review without unexpectedly starting Gemini enrichment. When
it is `true`, the same already authorized request runs final enrichment only
after the all-or-nothing review transaction commits. The browser sends `true`
for its one-click Verify Listing action. The detail response supplies the
current approved-catalog hash; resolution recomputes it while holding the write
lock. The hash covers approved product IDs, manufacturer/model labels,
capabilities, stable identifiers, and approval membership only, so unrelated
changes to preserved unreviewed or rejected legacy rows do not invalidate the
form. If the bundle or those approved identity fields change, the API rejects
the stale submission and the workspace offers Reload Review.

Resolution is all-or-nothing. The server first checks mandatory FAA admission,
before any catalog write. The database transaction then validates the full
decision set, creates or selects verified catalog identities, replaces only
the exact covered listing-link ID/role pairs, and removes the pending bundle
together. Without the explicit finalization flag it leaves the listing
incomplete and private. With the flag set, the server rechecks FAA admission
and runs grounded enrichment outside the transaction. Only successful
admission, enrichment, and readiness checks publish it as `ready` and
verified. Accepted associations receive `listing_review`
provenance and high installation confidence and participate in valuation under
the same evidence rule as high-confidence `listing` associations. A completion
failure is stored as `quarantined`; an unresolved bundle remains
`pending_review` and is not treated as an ingestion failure. A bundle staged
concurrently with post-review enrichment takes precedence over quarantine and
stays visible in the review queue.
