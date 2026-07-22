# LLM Usage

The app uses Gemini for extraction, normalization, and grounded metadata
enrichment. LLM output is never treated as durable truth without schema checks
and local validation.

The extraction entry points are in `src/extract.rs`; shared routing, transport,
benchmarking, and accounting are in `src/gemini/`.

## Runtime Configuration

Gemini routing is centralized in the versioned, credential-free
`config/gemini.toml`. Each `[tasks.<task>]` route can select a pinned model,
service tier, thinking level, and maximum output tokens. Aliases ending in
`-latest` are rejected so request accounting and comparisons remain
reproducible.

Normal application startup resolves configuration in this order, with each
later source taking precedence:

1. Compiled defaults.
2. The file named by `AIRCOST_GEMINI_CONFIG`, or `config/gemini.toml` when the
   variable is unset or blank and the checked-in file exists.
3. Legacy environment variables, retained for deployment compatibility.
4. Task-specific environment variables.

Task-specific names use the task prefix plus `_MODEL`, `_SERVICE_TIER`,
`_THINKING_LEVEL`, or `_MAX_OUTPUT_TOKENS`. For example,
`AIRCOST_GEMINI_LISTING_EXTRACTION_MODEL` overrides only
`tasks.listing_extraction`, while
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

The accounting table stores no prompt text, response body, downloaded image
bytes, or API key. Prompts and images exist only in memory for the request, and
`GEMINI_API_KEY` remains process configuration.

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
acceptance is never listing admission: the candidate must still match the
current target-scoped FAA projection exactly, and an observed serial must not
conflict. The plugin submission retains the visual decision, model, evidence,
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

`curate-aircraft-hierarchy` is a read-only, evidence-producing workflow. It
loads literal aircraft labels from retained listing source, groups compatible
observations, applies a mandatory local FAA admission gate, researches primary
sources, queries the live approved aircraft catalog, and performs independent
adjudication and verification. It returns reviewable proposals and interaction
audits; it cannot create or approve canonical aircraft rows.

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
subsequent N-number, serial, FAA projection, release archive, or source-record
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
JSON conversion. Search is limited to the 20 URLs accepted by the URL Context
stage. The JSON pass may copy only URLs verified by URL Context.

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

Every extracted candidate, including an exact-looking string, goes through a
two-stage grounded Gemini workflow:

1. Local normalization and similarity scoring build a shortlist from current
   `approved` and legacy-`unreviewed` catalog rows. This is retrieval only; an
   exact normalized string is never identity proof.
2. Gemini returns `existing_match`, `propose_new`, `reject`, or `unresolved`
   with authoritative identity evidence. Existing IDs are schema-constrained
   to the supplied shortlist.
3. Every positive identity decision, including a match to an already-approved
   row, is sent through a second independent grounded review. The reviewer must
   first attest that the exact proposed product is the same product represented
   by the raw input. For listing assignment it must also quote an exact stored
   listing substring containing the discriminating model label; a real product
   manual cannot prove that a particular listing names or installs that unit.
4. The same review compares the proposal with every shortlisted collision and
   returns `same_product` or `different_product` with `very_high` confidence
   and evidence for each ID. This proposal attestation is required even when
   the collision shortlist is empty, so an empty array is not a vacuous pass.
   The call uses the separately configurable
   `AIRCOST_GEMINI_AVIONICS_REVIEW_MODEL`.
5. Only after all checks pass does one transaction promote the
   confirmed legacy row or create a new `approved` row.

Approved identities require an official manufacturer part number, manufacturer
model number, or authoritative manufacturer SKU. The display identifier is
retained and a compact normalized identifier is used only as a uniqueness key
within the manufacturer. Manufacturer/model normalization and canonical
capability keys are used for candidate lookup and storage, never to assign raw
listing text mechanically. Positive identities must return a non-empty array
containing only server-owned avionics capabilities. Multifunction products
retain every verified capability on one identity; Gemini cannot introduce a
typo, `Unknown`, or a new free-form capability as part of approval.

For positive decisions the server requires Gemini's returned
`groundingChunks` and `groundingSupports`, and verifies that the evidence claim
is linked to the claimed web source. Merely returning a plausible URL or a
search-query marker is insufficient. Honest second-stage `not_confirmed`
answers become normal unresolved outcomes rather than being corrected toward
approval.

The identity classifier never returns prices. Product-identity confidence and
listing-installation confidence remain separate: proving that a GTX 345R exists
does not upgrade a weak claim that one is installed on a particular aircraft.
Replacement and removal targets resolve independently. If Gemini is absent,
evidence is incomplete, candidates conflict, or the catalog changes during
review, ingestion fails closed and the listing remains quarantined.

Catalog approvals take an optimistic fingerprint of the active catalog before
the model calls, serialize the final write, and compare the fingerprint again
inside the transaction. A concurrent catalog edit forces a retry instead of
allowing the model to approve against a stale shortlist. A non-empty legacy
manufacturer identifier likewise cannot be silently overwritten with a
different identifier.

`approved` rows are the curated catalog. Legacy rows remain `unreviewed` until
grounded review; rejected listing text is not stored as a catalog row. Promoting
a legacy identity clears its old value and suite metadata, because identity
evidence cannot validate previously imported dollar assumptions.

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
`incomplete`; deterministic readiness queries recheck all evidence. Any failed
enrichment or incomplete result is persisted as `quarantined` with an error and
is excluded from snapshots and serving until reprocessed.

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
