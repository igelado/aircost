# LLM Usage

The app uses Gemini for extraction, normalization, and grounded metadata
enrichment. LLM output is never treated as durable truth without schema checks
and local validation.

The extraction entry points are in `src/extract.rs`; shared routing, transport,
benchmarking, and accounting are in `src/gemini/`.

## Runtime Configuration

Gemini routing is centralized in the versioned, credential-free
`config/gemini.toml`. Each `[tasks.<task>]` route can select a pinned model,
an optional pinned fallback model and fallback thinking level, service tier,
primary thinking level, and maximum output tokens. Aliases ending in `-latest`
are rejected so request accounting and comparisons remain reproducible.

The avionics and aircraft Search, URL Context, and ordinary structure routes
use `gemini-3.5-flash-lite` with low thinking first. A valid primary response
ends the stage without a fallback call. Only a returned response that fails the
stage's deterministic grounding, citation, JSON, or provenance gates is retried
with `gemini-3.6-flash`; Search and URL Context fallback calls use medium
thinking, while ordinary structure fallback calls stay low. Transport failures
do not escalate the model. The larger avionics collision schema uses its own
`tasks.avionics_collision_structure` route with `gemini-3.5-flash-lite` first
and `gemini-3.6-flash` only after deterministic validation failure. Direct
publisher dossiers rank up to four bounded windows per source using separate
optional capability and shortlist hints; those hints never participate in the
required identity-anchor or exact-origin admission gates. Search prompts
request no more than four focused queries as a soft cost bound; the application
does not reject an otherwise valid response after the provider has already
exceeded that request.

Grounded avionics requests use separate stage inputs. Search and URL Context
receive a compact research brief containing the observed product and the full
maker/model/capability/identifier shortlist, but not listing HTML, catalog IDs,
catalog status, response schemas, or decision rules. Only the tools-disabled
structure stage receives the complete decision contract. Initial avionics
identity passes allow at most eight cited URLs; the independent collision pass
keeps the shared maximum of twenty because it may need to distinguish a larger
expanded shortlist. Both limits are enforced locally at citation resolution,
the URL Context call trace, and the verified citation allow-list.

Normal application startup resolves configuration in this order, with each
later source taking precedence:

1. Compiled defaults.
2. The file named by `AIRCOST_GEMINI_CONFIG`, or `config/gemini.toml` when the
   variable is unset or blank and the checked-in file exists.
3. Legacy environment variables, retained for deployment compatibility.
4. Task-specific environment variables.

Task-specific names use the task prefix plus `_MODEL`, `_FALLBACK_MODEL`,
`_SERVICE_TIER`, `_THINKING_LEVEL`, `_FALLBACK_THINKING_LEVEL`, or
`_MAX_OUTPUT_TOKENS`. An empty/`none` fallback model disables escalation, and
an empty/`inherit` fallback thinking level inherits the primary thinking level.
For example,
`AIRCOST_GEMINI_LISTING_EXTRACTION_MODEL` overrides only
`tasks.listing_extraction`, while
`AIRCOST_GEMINI_AVIONICS_COLLISION_STRUCTURE_MODEL` overrides only collision
structure conversion and
`AIRCOST_GEMINI_AIRCRAFT_VISUAL_IDENTITY_THINKING_LEVEL` overrides only the
visual-identity route. The legacy `AIRCOST_GEMINI_MODEL`,
`AIRCOST_GEMINI_GROUNDING_MODEL`, `AIRCOST_GEMINI_AVIONICS_REVIEW_MODEL`,
`GEMINI_AIRCRAFT_VISUAL_MODEL`, `AIRCOST_GEMINI_THINKING_LEVEL`, and
`AIRCOST_GEMINI_MAX_OUTPUT_TOKENS` continue to overlay their historical task
groups; a task-specific variable wins when both are set.

`GEMINI_API_KEY` remains a runtime secret and is never read from the TOML file.
Set it to enable extraction and enrichment. If the key is absent, manual
listing preview still works, but URL/plugin extraction reports an error.

## Request Accounting

Every logical Gemini request creates a `gemini_api_usage` row before the
provider call and finalizes it afterward. One row includes all transport
retries for that logical request; a correction, review, or adjudication pass is
a separate row so its usage remains attributable. The row records task and
purpose, API family/version, pinned model and service tier, status, source and
job correlations, request fingerprint, provider counters, attempts, latency,
validation outcome, error text, and an optional dated paid-list cost estimate.

Provider counters are nullable. An explicitly reported zero is stored as zero,
while an omitted counter remains null. Cost can be estimated only when the
provider reports every counter required by the pricing calculation and the
model/tier has a dated pricing snapshot. Otherwise cost remains unknown: both
`estimated_cost_microusd` and `pricing_snapshot_json` stay null rather than
silently treating missing counters as zero.

Interactions reports tool-use input separately from ordinary prompt input.
URL Context and custom-function tool tokens are charged at the model's uncached
input rate; Google Search retrieved context is excluded while its search-query
fee is accounted separately. If one request mixes Search with a chargeable tool
and reports only an aggregate positive tool-token count, the estimate remains
unknown rather than guessing how to split it.

The accounting table stores no prompt text, response body, downloaded image
bytes, or API key. Prompts and images exist only in memory for the request, and
`GEMINI_API_KEY` remains process configuration.

## Evidence Retention And Reuse

Search and URL Context output is request-scoped working data, not a durable
cache. A grounded curation case may retain its verified URL set and citation
spans in memory long enough to run a correction or independent structure pass
for that exact subject and candidate set. Reuse still performs a fresh,
tools-disabled structure call and revalidates the URL and citation-span
bindings. The dossier is discarded when the case ends; it is not serialized to
the database, and Gemini requests set `store=false`.

Across runs, the application reuses the approved conclusion rather than
replaying its research. Avionics first uses a deterministic, tools-free local
resolver over graph-approved catalog identities. It accepts only one exact
canonical product or stable-identifier match under the effective manufacturer
identity, requires the retained listing text to contain that identity, and
requires every observed capability to be approved for the product. When that
strict check cannot decide but the same evidence-backed manufacturer identity
has a small capability-compatible approved shortlist, one tools-disabled Lite
call may select only an unchanged supplied catalog ID at `very_high`
confidence. The caller re-reads the catalog and revalidates exact listing
evidence, membership, capabilities, and ambiguity before accepting it.
Anything else falls through to grounded curation. Stored source URLs, titles,
and excerpts are not model input. Aircraft reuse is similarly strict: an exact
current FAA record and one applicable approved hierarchy may bypass Gemini
entirely.

Existing-product re-attestation can also avoid Gemini after a fresh guarded
fetch from an origin approved for the effective manufacturer identity. The
fetched HTML or PDF must contain the complete graph-approved model and stable
identifier in one bounded visible HTML table row or reconstructed PDF visual
row. For PDFs, the server-owned product identity selects relevant fragments;
reconstruction joins fragments only on the same page and displayed horizontal
baseline after composing inherited right-angle page rotation with graphics and
text transforms. It never joins adjacent baselines or pages. An ambiguous row
that contains sibling products cannot authorize an identity, although a
separate clean exact row in the same document can. Hidden metadata, scripts,
generic page text, and cross-row matches never authorize.

Unrelated oversized or excess rows do not invalidate a targeted projection.
Target-relevant overflow, missing or undecodable invoked fonts, unhandled
invoked Form XObjects, malformed transforms, and incomplete page or resource
structures fail closed. Guarded source downloads are capped at 8 MiB; PDFs are
additionally capped at 256 pages, 2 MiB of extracted publisher text, and 2 MiB of
page content plus inspected invoked resources per page. Invoked font and Form
counts, graphics-state depth, and page-tree depth are independently bounded.

Durable evidence is limited to source records and atomic claims that active
catalog approval, FAA provenance, aircraft assignments, applicability, or
reference facts actually cite. Those rows are not complete Gemini prompts,
responses, or URL-context dossiers. The obsolete
`aircraft_curation_interaction_runs` request/response table was removed because
no runtime logic used it.

## Gemini Benchmark

`benchmark-gemini` builds a deterministic comparison suite from retained
production-shaped inputs:

```sh
cargo run --bin aircost-admin -- benchmark-gemini \
  --database /absolute/path/to/aircost.sqlite3 \
  --listing-limit 4
```

Omitting `--execute` is the dry run; there is no paid request in this mode. The
command uses only retained plugin submissions that are linked to a canonical
listing and contain non-empty source HTML. In other words, it samples only
source-backed canonical listings, using the configured seed and sample size,
configured explicit listing IDs, or repeatable `--submission-id` selections.
Historical extraction/audit output may accompany a case for regression review,
but is explicitly marked as not being ground truth. The suite is printed as
JSON and no database rows are written.

Paid execution must be requested explicitly:

```sh
GEMINI_API_KEY=... cargo run --bin aircost-admin -- benchmark-gemini \
  --database /absolute/path/to/aircost.sqlite3 \
  --task listing \
  --model PINNED_MODEL_ID \
  --execute
```

`--task` accepts `listing`, `metadata`, `avionics`, or `visual` and is
repeatable;
`--model` is also repeatable. Without explicit models, execution obtains the
candidate model IDs from the matching `[benchmark]` matrix in the effective
Gemini configuration. `--config FILE` loads that validated file explicitly;
otherwise normal `AIRCOST_GEMINI_CONFIG` and environment precedence applies.
The checked-in matrices are experiment definitions, not benchmark results or a
declaration of a winning/default model.

The initial real-data comparison and the rationale for the checked-in defaults
are recorded in `docs/gemini_model_benchmark_20260721.md`.

During `--execute`, live calls are paid and the command's only database writes
are `gemini_api_usage` accounting rows. It does not update listings, plugin
submissions, avionics, or any other canonical/domain table. The JSON report is
printed to stdout. Neither suite export nor execution stores prompts,
downloaded images, or API keys; visual bytes are downloaded, validated, used in
memory, and discarded. When provider usage counters are absent, reported cost
remains unknown rather than being shown as zero.

## JSON Contract

All model calls request `application/json` with an explicit response schema.
Parsing uses this contract:

- Parse the model response as a single JSON object.
- If parsing fails, send the original prompt, invalid response, and parse error
  back to Gemini for one repair attempt.
- Validate required fields locally after parsing.
- Reject or correct responses that omit required source rows, repeat source
  rows, produce unknown source IDs, return null for required fields, or produce
  generic values where concrete values are required.

Prompts for creation-critical extraction explicitly require non-null values.
Null is allowed only for optional metadata such as registration or serial number
when the listing does not provide it.

## Listing Extraction

The extraction prompt receives cleaned listing text and returns:

- manufacturer
- model family
- variant
- model year
- asking price and currency
- airframe hours and, only when explicitly stated, nullable engine and
  propeller hours with their source labels (`SNEW`, `SMOH`, `SFOH`, or `SPOH`),
  evidence text, and confidence
- explicitly identified installed engine and propeller models with source
  evidence; listing equipment never changes the factory variant spec
- registration and serial number when present
- status
- avionics candidates and explicit installed/replaces/removes actions
- source-backed restoration, damage/log, condition, conversion, and major
  modification facts

### Visual registration recovery

When retained Controller HTML does not yield a registration number, extraction
may inspect a bounded set of that listing's signed Sandhills image assets. The
downloader accepts only allowlisted HTTPS host/path combinations, resolves only
public addresses, follows no redirects, validates MIME type and file magic,
enforces per-image and aggregate byte limits, and rejects byte-identical
duplicates.

The visual call uses the versioned Gemini Interactions API request shape pinned
to API revision `2026-05-20`, with `resolution: high`, structured JSON output,
and the dedicated visual model above. Gemini may transcribe only a complete
registration visibly painted on the aircraft or printed on an explicit
registration label. It must return `high` or `very_high` confidence, the source
image ID, a bounding box, and a literal transcription; partial, inferred, or
autocompleted identifiers fail closed.

One complete, conflict-free N-number visible in one image is sufficient to
produce a visual candidate. More images add corroborating evidence but are not
required. Distinct visible registrations or serials are conflicts. Visual
acceptance is never listing admission: the candidate must still match an exact
target-scoped projection of the current FAA release, and an observed serial
must not conflict. Sibling target projections with the same snapshot date and
archive hash are one release identity. The plugin submission retains the visual decision, model, evidence,
image hashes, byte counts, and token usage for audit. An FAA-confirmed identity
repair is independent of later aircraft/avionics enrichment, so an unrelated
enrichment review cannot erase the recovered identity or its evidence.

The model/variant split is important:

- `model` is the broad economic family used for depreciation fitting.
- `variant` is the concise material configuration inside that family.
- Variant labels must omit maker and model year.
- Variant labels must keep material distinctions such as turbo, pressurized,
  retractable, amphibious, turbine, generation, or package when those affect the
  aircraft configuration.

## Aircraft Model And Variant Normalization

After extraction, the code compares the returned manufacturer/model/variant to
known database rows.

For model families, the LLM is asked whether the extracted model and a known
candidate are the same economic family. This allows values such as `182T` to map
to a broader family such as `182 SKYLANE` while preserving `182T` as variant
information.

For variants, the LLM is asked whether an extracted variant and a known variant
identify the same exact material configuration. The code passes listing context
and plausible candidates; it does not add maker/model-specific aliases.

Variant healing sends all variants for one manufacturer/model family to Gemini
and asks for groups. The local validator requires every input variant to appear
exactly once. If a subset is missing or duplicated, the correction prompt sends
the original context, previous response, validation error, missing rows, and
duplicated rows back to the model.

## Aircraft Hierarchy Curation And Mandatory FAA Grounding

`curate-aircraft-hierarchy` is read-only by default. It loads literal aircraft
labels from retained listing source, groups compatible observations, applies a
mandatory local FAA admission gate, researches primary sources, queries the
live approved aircraft catalog, and performs independent adjudication and
verification. The default run returns reviewable proposals and interaction
audits without creating or approving canonical aircraft rows.

An explicit `--apply` persists only a case that passed every reviewability gate.
Immediately before writing, the command reloads the literal observation and
requires an exact listing-id/fingerprint match to the observation and FAA
grounding retained in that case. Persistence rechecks the FAA projection and
catalog revision and atomically creates the evidence-backed catalog decisions,
FAA binding, immutable listing assignment, and valuation compatibility
projection. Missing, ambiguous, stale, or merely suggested cases are reported
as blocked; they are never mechanically normalized into the catalog.

If the read-only pass instead finds one exact already-approved hierarchy,
`--apply` creates or reuses only the listing's FAA-backed immutable assignment
and reports `catalog_reused_assigned` or `catalog_reused_current`, with zero
catalog writes. For a newly approved cluster spanning several listings, one
deterministic representative persists the shared approval; each remaining
listing then receives the same exact approved hierarchy through that
assignment-only path.

The FAA gate applies to every observation before Gemini sees it. An observation
is admitted only when all of these conditions hold:

- the registration is a syntactically valid U.S. N-number;
- an imported projection of the newest FAA release explicitly covered that
  N-number;
- the coverage status is `matched` and exactly one projected MASTER row exists;
- a listing serial, when present on both sides, does not conflict with the FAA
  serial; and
- the listing's hierarchy labels occur literally in retained source text.

Apart from the bounded visual-recovery step needed to obtain a missing
registration candidate, missing registrations, non-N registrations, malformed N-numbers, missing or
non-covering current snapshots, registrations absent from current MASTER,
ambiguous matches, and serial conflicts exclude the observation from curation.
They also reject new listing creation and updates before any Gemini call or
database mutation. Existing rows created before this policy are retained for
audit, but are excluded from curation and valuation rather than silently used.
If a cluster has no source-exact FAA-eligible observation, the workflow stops
before making a Gemini call.

Valuation snapshots freeze the exact FAA admission evidence for every included
listing and include it in their hashes. Training, structural/DNN activation,
comparable fallback, and request-time serving reject legacy snapshots or any
subsequent N-number, serial, FAA release archive, or source-record change.
Adding another target projection of the same release is not an identity
change. They never repair an immutable snapshot by silently dropping rows.

The local `lookup_faa_aircraft_registry` function does not accept a registration
number from Gemini. Its only arguments are a server-generated case token and the
schema-constrained cluster key. The returned payload was precomputed from an
immutable, digest-identified FAA release and is bound to that case. The
adjudication interaction must call this function exactly once before it may call
`search_aircraft_catalog`; changed tokens, additional registration arguments,
missing calls, duplicate calls, or missing function results fail the case.

The FAA result is controlling only for the claim-specific fields present in its
release: N-number, manufacturer serial, opaque aircraft and engine codes, joined
FAA make/model/series and engine-reference labels, `YEAR MFR`, and available
type-certificate reference fields. The local payload and prompt explicitly set
`year_manufactured_is_model_year` to false. `year_manufactured` is audit-only:
Gemini must not replace, infer, increment, decrement, or otherwise alter the
listing's `model_year`, even when the two values differ.

FAA coarse aircraft-type, engine-type, and category codes can be inconsistent
with the exact joined model. They cannot establish installed equipment or
engine technology by themselves. The registry also does not establish a
marketing generation, factory tier/package, default avionics, historical MSRP,
market applicability, or valuation. Those facts still require claim-specific
primary evidence:

- FAA registry or type-certificate evidence controls registered identity and
  certification/production facts within the source's stated scope.
- Manufacturer evidence controls commercial generation/package identity,
  factory configuration, standard equipment, market applicability, and
  reference price.
- Approved flight manuals and manufacturer service publications can be primary
  for certificated configuration, component, feature, and production
  applicability claims, but do not establish historical selling price unless
  they actually publish it.
- Recognized secondary sources can corroborate; they do not replace available
  primary evidence.
- Marketplace listings provide exact observations about their own advertised
  aircraft only. They cannot define factory defaults or approve catalog facts.

Alongside the local FAA evidence, Google Search and URL Context remain necessary
for facts outside registry scope. Grounded citations, successful tool traces,
live catalog candidate searches, exact source observations, and the FAA
function result are audited independently. A generally authoritative source is
not authoritative for every claim. Each evidence pass runs as three explicit
Interactions API requests: cited Google Search discovery, URL Context
verification of those exact resolved URLs, then tool-free schema-constrained
JSON conversion. The shared URL Context ceiling is twenty; callers may impose
a smaller request-scoped ceiling such as eight for initial avionics identity.
The JSON pass may copy only URLs verified by URL Context.

After the forced FAA and catalog function calls, deterministic validation runs
before the independent verifier. Fabricated catalog IDs, missing FAA identity
evidence, unresolved hierarchy dimensions, or confidence below `very_high`
block the case without spending verifier calls. The server repeats admission
against the current listing and newest FAA projection after adjudication and
again after verification; a changed listing, release, projection, or case token
invalidates the in-flight result.

## Avionics Resolution

Avionics parsing is intentionally strict. A durable avionics row should be a
concrete unit, integrated suite, or named package. Generic labels are not useful
for valuation and are not inserted into the catalog.

Every extracted candidate first goes through a local catalog pass:

1. A distinct exact OEM part number or SKU in the listing takes precedence.
2. Otherwise, an exact manufacturer-scoped canonical model label or
   typography-only normalized spelling may select one `approved` product with
   a current reuse attestation. The complete label must occur in retained
   listing text, observed capabilities may not exceed the curated product, and
   exact model/identifier collisions block selection. A possible but unwritten
   suffix does not defeat literal evidence; if the retained text explicitly
   names a longer known family member, the shorter base product is not
   selected.
3. Non-exact model similarity is retrieval only. Numeric-run-aware
   prefix/suffix candidates are retained for variant comparison and never
   assigned mechanically.

Candidates unresolved by that local pass enter the grounded workflow:

1. Gemini returns `existing_match`, `propose_new`, `reject`, or `unresolved`
   with authoritative identity evidence. Existing IDs are schema-constrained
   to the supplied shortlist.
2. Every positive identity decision, including a match to an already-approved
   row, is sent through a second independent grounded review. The reviewer must
   first attest that the exact proposed product is the same product represented
   by the raw input. For listing assignment it must also quote an exact stored
   listing substring containing the discriminating model label; a real product
   manual cannot prove that a particular listing names or installs that unit.
3. The same review compares the proposal with every shortlisted collision and
   returns `same_product` or `different_product` with `very_high` confidence
   and evidence for each ID. This proposal attestation is required even when
   the collision shortlist is empty, so an empty array is not a vacuous pass.
   The call uses the separately configurable
   `AIRCOST_GEMINI_AVIONICS_REVIEW_MODEL`.
4. Only after all checks pass does one transaction promote the
   confirmed legacy row or create a new `approved` row.

Approved identities require an official manufacturer part number,
manufacturer model number, or authoritative manufacturer SKU. A documented
legacy model number may be identical to the canonical display label; a
separate LRU part number is not required. The display identifier is retained
and a compact normalized identifier is used only as a uniqueness key within
the evidence-backed manufacturer identity. Manufacturer/model normalization
may authorize only typography-equivalent exact labels for already-attested
products. Prefix, suffix, semantic, and fuzzy similarity remain retrieval
signals. Plausible semantic aliases and cross-maker product collisions stop
approval and create pending human review candidates. Positive identities must
return a non-empty array containing only server-owned avionics capabilities.
Multifunction products retain every verified capability on one identity;
Gemini cannot introduce a typo, `Unknown`, or a new free-form capability as
part of approval.

Legacy-product research prioritizes historical OEM manuals and catalogs, FAA
records, aircraft equipment lists, and installation or service documents.
Wikipedia, Wikidata, forums, and reseller catalogs may generate search terms or
lead to cited documents, but they are not sufficient as the sole source for
catalog approval. The resolver follows those references to durable primary or
regulatory evidence instead of requiring an unavailable modern orderable
part-number page.

For positive decisions the server requires Gemini's returned
`groundingChunks` and `groundingSupports`, and verifies that the evidence claim
is linked to the claimed web source. Merely returning a plausible URL or a
search-query marker is insufficient. Honest second-stage `not_confirmed`
answers become normal unresolved outcomes rather than being corrected toward
approval.

The identity classifier never returns prices. Product-identity confidence and
listing-installation confidence remain separate: proving that a GTX 345R exists
does not upgrade a weak claim that one is installed on a particular aircraft.
Replacement and removal targets resolve independently. A primary observation
that Gemini rejects as generic or garbage is discarded only at `high` or
`very_high` confidence and with one allowed structured `rejection_basis`. Its
`reason` must be a candidate-specific negative claim consistent with that
basis, explicitly name the observed model and its usable manufacturer, and
have its whole normalized text contained in one Google Search grounding
support span linked to a cited source. Support that merely proves the product's
identity, contradicts the negative basis, is unrelated, or splits the reason
across spans is insufficient. The server requests one correction for an unsafe
reject; if it remains unsafe or correction fails, the outcome becomes
`unresolved` rather than an automatic discard. An ordinary `unresolved`
result—including unavailable classification, incomplete evidence, or an
uncertain candidate—is a durable `pending_review` outcome rather than a
quarantine. Provider, persistence, and enrichment failures remain real
ingestion errors and can still quarantine a stored listing.

Catalog approvals take an optimistic fingerprint of the active catalog before
the model calls, serialize the final write, and compare the fingerprint again
inside the transaction. The same transaction creates or loads the authoritative
manufacturer identity and immutable membership before approving the product.
A concurrent catalog edit forces a retry instead of allowing the model to
approve against a stale shortlist. A non-empty legacy manufacturer identifier
likewise cannot be silently overwritten with a different identifier.

Stable-identifier equality always includes both the non-null identifier kind
and its normalized value inside the canonical manufacturer namespace. The same
text labeled as a SKU and as a manufacturer part/model number is not an
automatic match; it remains separate evidence for review.

`approved` rows are the curated catalog. Legacy rows remain `unreviewed` until
grounded review; rejected listing text is not stored as a catalog row. Promoting
a legacy identity clears its old value and suite metadata, because identity
evidence cannot validate previously imported dollar assumptions.

### Pending Human Review

The listing pipeline persists all unresolved avionics aspects together in one
hash-addressed bundle per listing. The listing becomes `pending_review`, stays
unverified, and skips metadata enrichment while that bundle exists. Observed
text, installation action, source evidence, and replacement relationships are
review context only; they do not become catalog products or canonical listing
links before a decision.

All three actions remain available for every avionics aspect, including an
aspect with a suggested match or legacy candidate. The reviewer must make
exactly one decision for every aspect in the bundle:

- `use_verified_product` selects an existing `approved` catalog ID.
- `create_verified_product` supplies a concrete manufacturer and model,
  canonical capabilities, a stable manufacturer identifier kind and value,
  plus authoritative source URL, title, and evidence text.
- `discard` records a reason and creates no product or association.

The bundle stores both its own payload hash and an approved-only catalog
fingerprint of product IDs, manufacturer/model labels, capabilities, stable
identifiers, and approval membership. Its stored catalog hash records staging
provenance; each read exposes the current catalog hash, and a resolve request
must echo that current value. Resolution recomputes it under the write lock.
Changes to approved identity fields therefore force a reload, while unrelated
edits to preserved `unreviewed` or `rejected` rows do not invalidate work. The
resolution transaction validates a complete decision set, creates or promotes
verified identities, replaces only exact covered listing-link ID/role pairs,
deletes the bundle, and returns the listing to `incomplete` atomically. An
unlinked observation gets an explicit promotion target only when normalized
manufacturer/model uniquely identifies one `unreviewed` row. An aspect already
covering an exact legacy listing association may expose that known catalog row
by ID, but this does not bypass the same locked normalized-identity uniqueness
and identifier/model collision checks. A create decision that still matches
may promote a surviving candidate; a corrected identity creates a separate
product and leaves the legacy row untouched. A legacy product referenced by
aircraft defaults, reference configurations, or suite membership cannot be
promoted through listing review. It never admits an undecided or unverified
candidate into canonical state.

Avionics are also an explicit `PATCH` boundary. Omitting `avionics` skips
avionics identity resolution and preserves the pending bundle, its hashes, and
the exact listing-link IDs; ordinary price, status, hours, and similar changes
can therefore proceed without silently restaging review work. A patch that
includes `manufacturer`, `model`, `variant`, `model_year`, `source_url`,
`registration_number`, or `serial_number` must include a valid avionics array
because those fields change the resolution context. Null, non-array, or
malformed avionics fail before mutation. `"avionics": []` is an intentional
complete clear, while a non-empty array runs identity resolution and replaces
or restages the bundle and links.

Mandatory FAA admission is checked before any review-driven catalog write. It
is checked again after that transaction and before grounded metadata enrichment
and final publication, closing the race around network calls. A source-backed
listing becomes `ready` and verified only if FAA admission, full enrichment,
and the readiness pass all succeed. A failure at that stage is persisted as
`quarantined`; it does not roll back or hold a network request inside the
catalog/link transaction. Associations explicitly corroborated by a reviewer
use `listing_review` provenance with high installation confidence and are
valuation-eligible wherever equivalent high-confidence `listing` associations
are accepted.

## Grounded Metadata

The metadata request enables Gemini's Google Search tool for factual metadata:

- avionics introduction year, installed resale contribution, and replacement
  cost
- default/factory avionics by aircraft variant and model year
- model-year new-price points
- variant-level aircraft specs, engine model, propeller model, TBOs, overhaul
  costs, fuel burn, and maintenance assumptions

Grounded prompts require source URL, source title, confidence, and non-null
values for fields that the database needs.

The provider can still decline to call an enabled Search tool. Production
metadata enrichment currently validates the returned values and independently
resolves product identities, but does not yet reject the value payload solely
because the metadata request omitted observed Search/citation evidence. The
benchmark does reject that condition. Until evidence discovery is moved to a
forced-tool workflow, a plausible URL in metadata output is not proof that the
year or dollar values were grounded.

Confidence alone is not valuation eligibility. The local validators also
classify provenance and purpose:

- Factory specs and reusable engine/propeller costs require an authoritative
  reference. A sale-listing page, including a generic marketplace listing URL,
  is rejected as factory evidence.
- New-price anchors require direct evidence for the exact model year. Inferred,
  interpolated, other-year, homepage-only, or unexplained discontinuous values
  may be retained for review but cannot drive valuation.
- Avionics prompts return installed resale contribution and replacement cost as
  separate values and retain the value source. Factory-default avionics must
  cite factory/reference evidence, not an ordinary sale page. Named integrated
  suites and every contained unit must independently resolve to approved
  catalog identities so the suite and its components are not counted twice.

LLM completion does not make a listing ready by itself. The database row starts
`incomplete`; deterministic readiness queries recheck all evidence. Expected
avionics uncertainty is persisted as `pending_review` and skips enrichment.
Failed enrichment or another actual completion error is persisted as
`quarantined`. Both states are excluded from snapshots and serving until the
review is resolved or the failure is reprocessed.

## Normalization Philosophy

Do not fix LLM mistakes by adding one-off maker/model branches. The preferred
repair path is:

1. Make the prompt more precise.
2. Add generic validation for the class of error.
3. Send the original prompt, invalid response, and exact validation issues back
   to the model for correction.
4. Reject low-confidence or generic output instead of storing bad facts.
5. Add durable, reusable database facts only after they are concrete.

This keeps the system able to handle new manufacturers and aircraft families
without accumulating fragile special cases.
