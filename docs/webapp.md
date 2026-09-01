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

During creation or an explicit avionics replacement, a structurally valid
observation whose complete typography-normalized model equals one entry in the
closed generic equipment-category vocabulary is discarded before any local,
catalog, or provider identity work. Automatic review uses the same
provider-free rule only after capabilities, quantity, source evidence, action,
and replacement graph pass structural validation. Whole-label equality is
required; manufacturer names, capabilities, substrings, and fuzzy matches never
authorize discard. Structurally malformed observations remain pending. Other
observations that reach the grounded resolver retain its narrow tools-disabled
concreteness classifier and normal grounded fallback.
The grounded resolver has a separate discard path for a
high-confidence structured `rejection_basis` and a basis-consistent,
candidate-specific negative reason. For that path, the entire normalized
reason must appear in one linked Google Search citation support span. A
citation that only establishes product identity, contradicts the negative
claim, or is otherwise unrelated leaves the observation pending for review
instead of discarding it.

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

## Listing Acceptance and Review Workspace

The Review tab separates three operator workflows by purpose:

- **Automatic acceptance** runs the durable verifier for selected listings.
  It accepts only unambiguous FAA- and source-backed identities; unresolved
  aircraft, product variants, quantities, and occurrence collisions remain
  pending instead of being guessed.
- **Known avionics products** maintains a reusable manufacturer-source check
  for an approved catalog product, then validates only locally eligible exact
  listing occurrences.
- **Manual review** handles the residual aircraft and avionics evidence for one
  listing. Its Aircraft and Avionics tabs report their completion independently.

The known-product queue
collapses every hash-bound existing-product aspect onto one approved avionics
identity. A product is attested once from a guarded OEM fetch, without Gemini,
then its listing aspects are checked locally with bounded concurrency across
listings and serial optimistic-lock updates within each listing. These aspects
may represent a preserved link or an ordinary unlinked extraction observation;
the browser uses the same verification workflow for both. The manual listing
queue shows the aircraft, tail, year, aspect count, reason groups, and last
update. Opening a listing shows every unresolved observation, its source
context, and any suggested or proposed product.

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

The manual Avionics tab is occurrence-first: each card is one exact retained
extraction occurrence, and its quantity belongs only to that occurrence.
Distinct cards are never silently merged after they resolve to the same catalog
identity. Assigning two cards to one canonical product remains blocked until the
reviewer corrects the extraction to one source-supported occurrence and explicit
quantity, discards a duplicate observation, or selects the genuinely different
product variant.

Ordinary extracted avionics aspects offer three decisions:

- **Use verified product** searches and selects one existing approved catalog
  identity.
- **Create verified product** requires manufacturer, model, one or more
  canonical capabilities, a stable manufacturer identifier kind and value,
  and authoritative identity source URL, title, and evidence text.
- **Discard observation** requires a reason and creates neither a catalog row
  nor a listing association.

An existing-product match can be committed immediately with **Save verified
product for this entry**. The server updates only that occurrence, removes its
card from the hash-bound review, and returns a fresh revision for the remaining
cards; unsaved browser drafts for those cards are preserved. The final saved
card runs the ordinary canonical listing finalizer automatically. The UI reports
completion only when the returned listing is both `ready` and verified, and
keeps any exact association or finalization failure on the affected card or in
the terminal result instead of replacing it with a listing-wide generic error.
Create and discard decisions continue to participate in the atomic complete
review action when they remain.

Catalog search results expose `catalog.reuse_eligible`. An approved product that
lacks a current reusable manufacturer-source attestation remains visible for
diagnosis but cannot be selected. Preparing the Known avionics products queue
also promotes a still-current approved suggestion on an independent ordinary
occurrence into an explicit product-attestation target. This lets the reviewer
verify the OEM source once, validate each retained listing occurrence locally,
and then return to manual review without recreating the catalog product.

Each avionics card also allows the reviewer to correct the extracted
manufacturer, model, canonical capabilities, and quantity. A fresh unlinked
observation may additionally correct its installation action and replacement
target; an aspect already bound to a listing link keeps that relationship
locked. Saving a correction creates a new hash-bound pending-review revision,
clears any prior product suggestion, and leaves the publisher's observed text,
source evidence, and retained submission immutable. It does not write an
avionics catalog row or listing association. The corrected aspect must still be
resolved through one of the normal review decisions above.

A hash-bound approved-product target is not a fourth decision type. Product
attestation and retained-source occurrence verification are separate
operations.
For a preserved link, successful local verification corroborates the existing
occurrence without rewriting it, including an unchanged positive quantity
greater than one. The staged aspect quantity must exactly equal the current
listing-link quantity at preflight and again under the mutation lock. For an
ordinary installed, non-replacement aspect, the same operation may create an
exact-quantity association or update its one covered installed link through
the normal aspect-scoped `use-existing` transaction. Until local verification
succeeds, the aspect
remains pending and may only be explicitly discarded from the listing
workflow. Automated verification requires `source_evidence_text` to be one
exact, bounded structurally visible body span in the immutable plugin
submission attached to that review, after HTML entity and whitespace
normalization only. Structural visibility excludes head and executable
content, hidden attributes, inline or embedded stylesheet hiding, and closed
details/dialog containers. It cannot reconstruct browser-computed visibility
from external CSS absent from the retained outer HTML. The submission must
belong to the listing owner, name that exact canonical listing, and retain its
stored content hash. Missing captures, generated explanations, hidden
metadata, corrected text, and substring-only model matches remain pending.
The UI labels exact source evidence separately and never substitutes the
structured `observed_text` label when occurrence evidence is absent.

Restaging also repairs legacy occurrence evidence under the same listing,
review, link, catalog, and capture locks. Exact capture-backed pairs remain
available; a unique unqualified manufacturer/model occurrence can replace
generated notes for an installed link of any positive quantity. Ambiguous or
missing evidence is removed from auto-repairable installed links and from the
hash-bound aspect, so the listing remains pending. Replacement and other
manual-review shapes are not blanket-mutated, but their unverified notes are
not exposed as source evidence.

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
GET /api/review/verification/preflight?limit=100&after_listing_id={listing_id}
POST /api/review/verification-runs
GET /api/review/verification-runs/{run_id}
GET /api/review/verification-runs/{run_id}/items?limit=100&after_item_id={item_id}
POST /api/review/verification-runs/{run_id}/cancel
GET /api/review/avionics/products?limit=25&cursor={opaque_cursor}
GET /api/review/avionics/products/{product_id}/associations?limit=25&cursor={opaque_cursor}
POST /api/review/avionics/products/{product_id}/attest
POST /api/review/listings/{listing_id}/restage
POST /api/review/listings/{listing_id}/aircraft/visual-recovery
POST /api/review/listings/{listing_id}/aircraft/faa-serial
POST /api/review/listings/{listing_id}/aircraft/publisher-hierarchy
POST /api/review/listings/{listing_id}/avionics/rebuild
POST /api/review/listings/{listing_id}/avionics/consolidate
POST /api/review/listings/{listing_id}/avionics/use-existing
POST /api/review/listings/{listing_id}/avionics/revise
POST /api/review/listings/{listing_id}/avionics/verify-existing
POST /api/review/listings/{listing_id}/avionics/approve-replacement
POST /api/review/listings/{listing_id}/resolve
```

The aircraft tab exposes only repair actions returned by provider-free
preflight. Every request carries `expected_state_sha256`, which binds current
identifiers, hierarchy, owner, retained submission, source URL, and rendered
HTML digest. A serial conflict on an exact current FAA N-number offers the
zero-Gemini FAA serial correction only when the retained source contains the
exact N-number and observed serial and the FAA value is a narrow internal
one-edit typo. Other serial conflicts require explicit evidence and manual
adjudication. Missing, invalid, or unassigned
registrations offer a reviewer-selected one-photo visual recovery when a safe
retained asset exists. The server applies the visual result only after exact
current FAA admission; non-covered candidates return an import-required result
and N-numbers absent from current MASTER return a terminal not-assigned result,
both without changing the listing. `source_evidence_missing` offers an exact
visible publisher-span form; it changes no hierarchy labels and must contain
the current maker, model, and variant under the bounded token rules.

Explicit restaging is the recovery boundary for a synthetic preserved-link
card whose covered listing association has changed since staging. It removes
only the stale relationship component, recreates cards from the current link
set, and retains independent raw observations. Correction and final resolution
remain strict: they reject stale covered links instead of repairing them
implicitly.

The avionics rebuild endpoint is a separate, explicitly confirmed reset of the
machine-owned avionics cards. It accepts the current `review_payload_sha256`,
uses only the exact retained current-schema extraction, current catalog,
listing links, authorizations, and reviewer-owned corrections, and never calls
Gemini. It does not run during ordinary restage or page load. Because historical
discard decisions do not have a durable disposition ledger, the server first
requires every retained extraction occurrence to have a one-to-one claim in a
current link or residual avionics review aspect. Legacy/defaulted extraction
fields, stale capture bindings, ambiguous matches, an unrepresented occurrence,
or review state outside the avionics workflow return a typed `blocked` response
without changing the review. The response exposes only a stable `reason_code`
and fixed safe message; parser and database details are not reflected to the
browser. A successful `rebuilt` response carries the new hash-bound review; the
browser warns that unresolved machine cards may need review again before it
sends the request.

The aspect-scoped consolidation endpoint never calls Gemini. A reviewer cites
authoritative evidence and names one survivor plus the complete set of
unreviewed catalog rows that represent the same product. The server accepts
typography-only labels and a complete curated-capability description, such as
`G1000` and `G1000 Integrated Flight Deck`, while treating meaningful hardware
variants such as `G1000 NXi` as categorically distinct. Preview returns a
hash-bound authorization snapshot; apply rechecks every member, its current
capabilities, the approved-catalog revision, and the pending-review provenance
under the mutation lock. The proposed review product may use any selected
member label. Omitted model-equivalent rows, stale member keys, conflicting
stable identifiers, or reference claims that cannot be preserved block the
operation.

The ordinary create-product review path can also discover a complete grounded
exact-model duplicate group. In that case the server consolidates and approves
the catalog group atomically but does not apply the now-stale listing decision.
The browser treats the dedicated response as progress, reloads the same listing
with the verified survivor selected, and asks the reviewer to confirm once
more. It is not displayed as a failed verification.

Automatic acceptance and opened-listing **Run safe automatic checks** actions
create the same durable server-owned verification run. The browser sends the
selected listing IDs with a cryptographically random `Idempotency-Key`, stores
only the returned run ID locally, and reloads the authoritative run and
paginated item state after navigation or browser restart. A network retry that
reports an existing active run resumes that run instead of starting parallel
work. The browser never attempts to execute the aircraft, avionics, or
finalization stages itself.

One run processes its listings serially while exposing queued, running, and
terminal item counts. The UI polls with request-sequence guards, shows the
current listing and terminal result of every item, and can request a stop after
the current listing. Verified items, residual manual reviews, blocked items,
failures, and cancelled items remain distinct. Factory-reference readiness is
displayed independently from the verification-run outcome. A manual
review link is shown only after a provider-free refresh confirms that the
listing still has a current pending review.

Before creating a run, the browser reports the current full automatic-acceptance Gemini
request plan and warns that finalization enrichment is additional. This is a
cost warning, not a hard budget. The avionics totals include one tools-disabled
concreteness-classifier request for every identity that reaches grounded
curation; verified-local identities and successful closed-context candidate
adjudications do not incur that request. Exact whole-label generic categories
contribute no requests; after structural validation they are discarded
deterministically and never proceed to grounding.
Deterministically FAA-rejected aircraft remain visible but are not selectable
for another automatic identity run. A pending factory reference never blocks
automatic listing verification. `GEMINI_API_KEY` enables extraction and
curation; `FAA_DRS_API_KEY` additionally enables unresolved aircraft grounding.
Deterministic FAA/catalog reuse still works without paid calls; unresolved or
uncertain observations remain in review rather than being auto-approved.
When aircraft and avionics review is complete but valuation-grade factory
reference data is not yet published, the listing can still be `verified` and
`ready`; its independent `reference.status` is `pending_reference`. Preflight
and preview derive this display-only valuation gap from local reference rows
without Gemini.

The Review page opens on the provider-free **Automatic acceptance** view. It
follows the numeric `resume_after_listing_id` checkpoint from the verification
preflight endpoint and displays every non-ready listing, including listings whose
identity review is complete but factory reference data remains pending. The
table keeps aircraft, avionics, and reference status separate, shows whether
Gemini is expected or may be needed after local checks, and exposes manual
review only when the response's listing context explicitly reports a current
pending review. Reference-pending rows remain visible and can still be selected
when automatic identity or listing-readiness work remains.

The Automatic acceptance overview translates the current preflight stages into
four
operator-facing backlog categories without making provider calls:

- **Current avionics review** has retained current-schema observations ready
  for local catalog checks, with Gemini used only if those checks cannot decide.
- **One-time avionics re-extraction** identifies legacy retained captures that
  must be extracted once into the current observation shape.
- **FAA admission blocked** identifies aircraft rejected by mandatory FAA
  admission; their avionics checks remain paused until the registration or
  serial identity is corrected.
- **Factory reference pending** identifies listings whose aircraft and avionics
  review is complete but whose model-year reference configuration is not yet
  available for valuation.

These counts use only the existing aircraft, avionics, and finalization statuses.
Rows with other blocked or transitional states remain visible in the Pipeline
table and are not forced into an inaccurate category.

Pipeline request counts are estimates from the same verifier plan used by the
administrative command. Loading or refreshing the view never calls Gemini and
never writes domain or usage data. The summary also reports whether Gemini and
FAA DRS are configured; unresolved aircraft grounding is called out when
`FAA_DRS_API_KEY` is absent. Product and listing review queues remain available
alongside Pipeline for focused manual work.

Product cursors are opaque keyset tokens ordered by immutable product ID.
Association cursors are bound to one product and ordered by listing and aspect.
Each returned association includes a read-only `verification_eligibility`
object. Its `status` is `auto_verifiable`, `product_attestation_required`, or
`manual_review_required`; blocked rows also include a stable `reason_code` and
an explanatory `reason`. The projection runs the same retained-source
preflight and complete local catalog collision decision as the mutation, but
does not fetch a source, call Gemini, or write data. It is advisory: the POST
rechecks every fact and its optimistic guards under current state. The web
client submits only associations reported as `auto_verifiable`; it displays the
remaining reason instead of issuing a mutation merely to discover that manual
review is required.

The product queue summarizes associations as total pending, ready locally,
needing source recovery, needing OEM attestation, or manual/ambiguous. Its
`eligibility_counts` projection uses the same categories as association
preflight. Missing retained occurrence proof is reported separately as
`source_evidence_missing`; meaningful model or capability qualifiers remain
manual rather than being treated as recoverable text. The browser can restage
each affected listing once, with bounded concurrency across listings, then
reloads current review hashes before offering local validation. Restaging does
not promise that evidence can be recovered and does not call Gemini.

The attestation request carries the catalog revision, one OEM source dossier,
and exactly one direct association authorization tuple:
`listing_id`, `review_payload_sha256`, and `aspect_id`. The server loads only
that listing review and requires the exact hash-bound aspect to target the
requested product; it does not scan other pending reviews for authorization.
The existing-product verification request carries only the canonical review
hash, catalog revision, and aspect ID; source fields and revision aliases are
rejected. It requires retained evidence to identify the hash-bound, currently
attested product uniquely in the live local catalog. The mutation lock
re-reads the exact review-bound source capture and rechecks its content hash
and visible evidence, as well as the review and catalog hashes, the complete
active collision closure, the target's current reuse eligibility, exact
covered-link ownership, and the listing action graph. Ordinary aspects must be
installed, positive-quantity, and independent of every replacement edge; they
may cover zero or one installed link. Attestation separately rechecks both the
manufacturer-scoped collision snapshot and ownership of the exact pending
aspect under the mutation lock.

Catalog `identity_evidence_text` is historical approved-product provenance, not
the fresh attestation excerpt. The product form may suggest a valid HTTPS OEM
URL and a source title within the request limit, but it deliberately leaves the
new exact publisher excerpt blank. A reviewer must provide one exact excerpt of
at most 128 characters from the source being freshly checked; unconstrained
historical catalog prose is never copied into that request field.

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

The aspect-scoped `use-existing`, `verify-existing`, and replacement responses
include the refreshed `review`, current `listing`, `review_complete`,
`listing_ready`, `listing_verified`, `finalization_attempted`, and an exact
`finalization_error` when the last-card finalizer fails. A non-terminal response
contains the next review revision and never finalizes the listing.

The complete resolve request includes `review_payload_sha256`,
`catalog_revision_sha256`, one decision for every returned aspect, and an
optional `finalize_listing` boolean. Both hashes are optimistic concurrency
controls. `finalize_listing` defaults to `false`, so API and batch reviewers
can save a local review without unexpectedly starting Gemini enrichment. When
it is `true`, the same already authorized request runs final enrichment only
after the all-or-nothing review transaction commits. The browser sends `true`
for its one-click **Complete manual review** action. The detail response
supplies the current approved-catalog hash; resolution recomputes it while
holding the write lock. The hash covers approved product IDs, manufacturer/model labels,
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
failure is stored as `quarantined`. Missing factory reference data is not a
completion failure: the API returns the resolved listing as `incomplete` and
unverified, the browser removes it from the manual queue, and the workspace
reports that factory reference curation remains pending before valuation. An
unresolved bundle remains
`pending_review` and is not treated as an ingestion failure. A bundle staged
concurrently with post-review enrichment takes precedence over quarantine and
stays visible in the review queue.
