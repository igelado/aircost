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

The avionics Search, URL Context, ordinary structure, and collision structure
routes use `gemini-3.5-flash-lite` with low thinking for both bounded attempts.
A valid primary response ends the stage after one call. Only a returned
response that fails the stage's deterministic grounding, citation, JSON, or
provenance gates opens a second attempt, which reuses the configured primary
model with the validation failure as correction feedback. Transport failures
also remain on the primary model. No avionics validation retry automatically
escalates to Flash.

Aircraft Search, URL Context, and ordinary structure routes also start with
`gemini-3.5-flash-lite`, but retain their separately configured
`gemini-3.5-flash` validation fallback. Direct publisher dossiers rank up to
four bounded windows per source using separate optional capability and
shortlist hints; those hints never participate in the required identity-anchor
or exact-origin admission gates. Search prompts request no more than four
focused queries as a soft cost bound; the application does not reject an
otherwise valid response after the provider has already exceeded that request.

Grounded avionics requests use separate stage inputs. Search and URL Context
receive a compact research brief containing the observed product and the full
maker/model/capability/identifier shortlist, but not listing HTML, catalog IDs,
catalog status, response schemas, or decision rules. Only the tools-disabled
structure stage receives the complete decision contract. Initial avionics
identity passes allow at most eight cited URLs; the independent collision pass
keeps the shared maximum of twenty because it may need to distinguish a larger
expanded shortlist. Both limits are enforced locally at citation resolution,
the URL Context call trace, and the verified citation allow-list.
When exact publisher-text verification is required, the structure-stage
allow-list narrows again to the final URLs of publisher documents the server
actually fetched into its bounded evidence packet. Search-only or failed-fetch
citation URLs are removed from structure citation records and prompt prose;
they cannot be emitted as structured source fields.
Each bounded publisher window receives a transient request-local identifier.
Evidence fields in the structure schema select one of those identifiers instead
of asking Gemini to reproduce or count offsets into publisher prose. The server
derives and overwrites the sibling source URL from the selected window, replaces
the selector with the exact window text, and then runs the document, digest,
evidence-proof, and domain validators. A source URL emitted alongside the
selector cannot choose or rebind its evidence. Unknown or nested/offset-shaped
selectors fail closed, and neither selectors nor evidence packets are persisted.
An unusable Search-discovery redirect is discarded when another citation
resolves successfully; the unresolved URL is never forwarded as a fallback.
If none resolve, Search retries normally. URL Context citation resolution
remains all-or-nothing because its verified output is evidence for the
structure stage. A citation or fresh publisher fetch whose actual terminal URL
is not HTTPS is likewise rejected before structure conversion; the application
does not rewrite an HTTP URL into an unfetched HTTPS claim.

Legacy grounded-metadata calls that still use GenerateContent follow the
provider's Search/structured-output constraint: the Search request sets JSON
MIME output but omits `responseSchema`. If its grounded JSON needs syntax
repair, one tools-disabled request receives the schema and the original
grounding metadata remains the provenance record. The application never sends
Google Search and `responseSchema` together.

Normal application startup resolves configuration in this order, with each
later source taking precedence:

1. Compiled defaults.
2. The file named by `AIRCOST_GEMINI_CONFIG`, or `config/gemini.toml` when the
   variable is unset or blank and the checked-in file exists.
3. Legacy environment variables, retained for deployment compatibility.
4. Task-specific environment variables.

Task-specific names use the task prefix plus `_MODEL`, `_FALLBACK_MODEL`,
`_SERVICE_TIER`, `_THINKING_LEVEL`, `_FALLBACK_THINKING_LEVEL`, or
`_MAX_OUTPUT_TOKENS`. An empty/`none` fallback model disables model switching,
not the bounded validation retry; that retry reuses the primary route. An
empty/`inherit` fallback thinking level inherits the primary thinking level.
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

Listing extraction uses its primary route for the full listing response. When
that response is valid JSON but its transient avionics member still fails the
current deterministic schema, evidence, capability, or quantity gates, the
server may make exactly one avionics-only correction request through the
listing-extraction fallback route. The correction receives only the listing
text, the prior transient avionics JSON, and bounded validation feedback; it
returns only a complete replacement `avionics` array. The configured default is
Flash Lite with low thinking for the primary request and Flash with low
thinking for that correction. When `fallback_model` is unset, the correction
reuses the primary model and thinking level.

The primary listing request is tools-disabled and always sends the complete
`responseSchema` in `generationConfig`. Its text prompt therefore carries the
domain and evidence rules but does not repeat a prose rendering of that same
JSON shape. Tests at both the prompt builder and request boundary require the
schema to remain attached.

Listing extraction records avionics manufacturer and model labels as literal
source observations, not canonical catalog identities. A source-authored
shared prefix may apply within one selected evidence span, but extraction does
not reconstruct an omitted familiar series prefix, expand an OEM name, or
normalize compact model typography from product knowledge. For example, the
right-hand shorthand in `Garmin GTN 750 & 650` remains model `650`, and source
labels such as `G3X Touchscreen`, `Garmin 255`, `JPI 830`, and `GFC500` remain
literal. Later catalog curation resolves aliases, canonical OEMs, and expanded
product identities. An occurrence without a useful source-supported literal
identity is omitted rather than repaired from model familiarity.

Occurrence evidence must be contained within one unit admitted by the same
publisher source adapter that constructs extraction-model input. For Controller
that means one exact specification value; labels, sibling values, price/page
chrome, and JSON-LD are ineligible. Inline markup and line wraps within a value
remain one unit. Generic publishers use structurally visible body units, where
sibling blocks, table cells, and definition terms/values remain separate.
Whole-body flattening cannot manufacture an evidence phrase across any of
these boundaries. The same gate runs for the initial extraction checkpoint and
durable avionics re-extraction. During materialization, local matching and any
paid avionics prompt receive only that checkpoint occurrence when it is
nonempty, no more than 256 bytes, and can be rebound to the retained source
adapter's exact source-unit proof. The flattened extraction text is not a
materialization proof and cannot join sibling DOM units. Identity work never
receives a page excerpt containing adjacent listing fields. The observed
manufacturer remains separate structured request data, so an exact occurrence
need not repeat it merely to preserve the paid fallback.
After any exact same-case replay is considered, a missing, oversized, or
unrebindable occurrence stops as pending before local matching or a provider
call. A paid fallback is therefore possible only for a successfully rebound
occurrence and only when replay policy permits it.

Within the one trusted Controller
`Avionics/Radios` value, an otherwise missing right-hand model boundary is
accepted only for a source-authored run-on annotation whose immediately
adjacent and complete suffix consists of capability phrases declared for that
same occurrence. Slash-delimited multifunction capabilities and an exact
quantity-consistent `(Dual)` annotation are supported. Undeclared capabilities,
arbitrary continuations, unrelated trailing words, and product-variant suffixes
remain identity failures. This extraction-only grammar also applies to an
explicit replacement identity and never rewrites the retained evidence. During
materialization the same exact-line grammar may corroborate one ordinary local
reuse decision only after the observed model already equals the canonical
product key inside the effective manufacturer identity and the complete product
graph has a current reuse attestation. Duplicate identities, variants,
unattested products, type mismatches, and non-Controller occurrences still use
the normal fail-closed or paid path.
Two additional Controller-only source grammars treat `WAAS` as an extraction
annotation for a model that already ends in an attached `W`: an exact
`model/WAAS CAPABILITY/CAPABILITY` line for one unit, and an exact
`Dual manufacturer model WAAS CAPABILITY/CAPABILITYs` line for quantity two.
The latter permits one plural `s` only on the final capability. Both require the
complete slash-delimited capability set to equal the occurrence's declared
atomic types. Near matches, other quantities, trailing text, generic sources,
and local catalog reuse remain ineligible.
An `Integrated Flight Deck` identity may establish that core category without
repeating it in prose; every additional category assigned to the suite must be
explicit in the same occurrence evidence, and a separately extracted
component's category cannot also be absorbed into the suite.

The listing-extraction envelope is at most two logical requests. A malformed
primary JSON response may consume the second request for the existing JSON
repair, or a directly parseable primary may consume it for the semantic
avionics correction. These paths are mutually exclusive. The correction is
parsed once and never receives its own JSON repair, so transport failures,
invalid correction JSON, and invalid corrected avionics fail closed without a
third request.

## Request Accounting

Every logical Gemini request creates a `gemini_api_usage` row before the
provider call and finalizes it afterward. One row includes all transport
retries for that logical request; a correction, review, or adjudication pass is
a separate row so its usage remains attributable. The row records task and
purpose, API family/version, pinned model and service tier, status, source and
job correlations, request fingerprint, provider counters, attempts, latency,
validation outcome, error text, and an optional dated paid-list cost estimate.

Provider counters are nullable. An explicitly reported zero is stored as zero,
while an omitted counter remains null unless the exact request configuration
proves that counter must be zero. Cost can be estimated only when every counter
required by the pricing calculation is reported or exactly deduced and the
model/tier has a dated pricing snapshot. Otherwise cost remains unknown: both
`estimated_cost_microusd` and `pricing_snapshot_json` stay null rather than
silently treating missing counters as zero.

The request adapter records zero tool-use tokens only for a first-turn text or
multimodal request that declared no tools, because tool input is then
impossible. A client-managed stateless-history continuation may carry earlier
tool calls and results even when its current tool declaration is empty, so an
omitted counter remains unknown there. It likewise remains unknown when
Search, URL Context, or a custom function was declared; task names and observed
output do not substitute for the exact sent request.

GenerateContent may omit `thoughtsTokenCount` when it is zero. When
`totalTokenCount`, `promptTokenCount`, and `candidatesTokenCount` are present,
the accounting adapter derives the exact thought count from those
provider-reported totals. It records zero cached tokens only for requests that
did not send `cachedContent`; a missing cache counter on a cache-backed request
remains unknown. These exact deductions allow pricing without treating an
unexplained missing counter as zero.

Interactions reports tool-use input separately from ordinary prompt input.
URL Context and custom-function tool tokens are charged at the model's uncached
input rate; Google Search retrieved context is excluded while its search-query
fee is accounted separately. If one request mixes Search with a chargeable tool
and reports only an aggregate positive tool-token count, the estimate remains
unknown rather than guessing how to split it.

The accounting table stores no prompt text, response body, downloaded image
bytes, or API key. Prompts and images exist only in memory for the request, and
`GEMINI_API_KEY` remains process configuration.

An avionics-only listing correction is accounted as a separate logical request
with purpose `listing_avionics_validation_correction`. It inherits the same
source, listing, and correlation attribution as the primary listing extraction
while recording the model actually selected by the fallback route. Neither its
prompt nor its response is persisted.

Apply-mode recovery of an obsolete listing extraction has a distinct durable
boundary. The returned raw avionics array must contain explicit quantity,
action, and replacement semantics, pass the current capability schema, and
bind every excerpt exactly to the retained source capture. The application
then creates one canonical current checkpoint. When the prior checkpoint is an
exact current `ParsedListing`, its non-avionics fields are preserved and only
`avionics` is replaced. When the prior is missing or unusable, the complete
newly extracted listing supplies those fields instead. Both inputs and the
final persisted object are checked against the shared exact checkpoint-field
contract, so unsupported provider-envelope, grounding, dossier, or arbitrary
keys cannot be retained. The write is bound to the exact submission, owner,
listing, source URL, capture bytes and hash, pending-review revision, canonical
listing binding, and prior extraction/error state.
PostgreSQL takes the project listing-child lock order and locks/revalidates the
listing, review, and submission rows before updating. The write is idempotent
for the same extraction and fails closed on any concurrent change. This lets a
later blocked identity run resume without paying for extraction again; it does
not persist prompts, provider envelopes, Search results, URL Context dossiers,
or grounding evidence. Preview mode never performs this domain write, and an
empty equipment extraction is not persisted.

Every automated avionics-review apply revalidates the complete current
extraction against the retained bound capture before it writes links,
authorizations, dispositions, or review state. This is unconditional for
accepted-links-only and local-reuse results even when they carry no occurrence
dispositions. When dispositions are present, their fingerprints reuse the
same parsed and validated observations instead of opening a weaker second
parse path. An empty avionics array cannot authorize an automated avionics
write. Pending-review observations are selected only when the attached raw
checkpoint also passes this complete contract; otherwise the workflow retains
the review and re-extracts the bound source before it can apply.

A newly grounded positive resolution in signed listing create or bound
materialization replay also creates a one-use, capture-bound capability for its
exact occurrence. A retry after a later association-write failure loads the
capability, recomputes its request, approved-product, and collision hashes from
current data, and makes no provider request for that unchanged same-case
occurrence. The capability is not a manufacturer-reuse attestation. Coalescing
requires a complete set of occurrence capabilities whose quantities and
actions exactly cover the proposed association; one quantity-one result cannot
authorize a quantity-two link. Final link insertion, `same_case_grounded` authorization,
and capability deletion share one transaction, so a failed apply leaves the
capability available and a successful apply cannot replay it.

This durable capability currently covers the signed listing create and bound
materialization-replay path only. Automatic-review receipts still remain
in-memory until their apply transaction, and globally reusable matches,
rejections, and other paid occurrence decisions are not a durable whole-graph
retry cache. Extending the same boundary to the verifier and staging a complete
capture-bound materialization/action plan are separate required changes; this
path does not claim a zero-provider-call retry for those cases.

Successful re-extraction always produces one complete current listing
checkpoint. If the retained checkpoint is a structurally current
`ParsedListing`, its non-avionics values are preserved and only avionics are
replaced. If it is missing, malformed, non-object, or not current, the complete
newly extracted listing becomes the repair checkpoint after full listing and
avionics validation. Only that parsed listing object is stored; provider
response envelopes and research dossiers are not retained.

Before a fresh listing extraction checkpoint is stored, one atomic transient
repair pass handles a few evidence-backed model-output defects. For an admitted
exact source span containing one unique exact model, it may set the
manufacturer to `null` when the returned maker is absent from that occurrence's
evidence or is a generic placeholder such as `Unknown`. This does not infer a
maker, canonicalize an identity, or borrow a maker from a shared-prefix list;
canonical maker resolution belongs to catalog matching. Primary and
replacement identities are evaluated independently.

For one structurally valid Controller capture with one exact `Avionics/Radios`
field, the same pass may copy one unique bounded visible span into
`source_evidence_text`. The copied span must be exactly equal to the model's
evidence locator after case and non-alphanumeric typography are removed, must
contain every primary and replacement identity, and must pass the existing
full-source suffix and ambiguity gates. Recovery is also allowed when generic
visible-text cleanup accepts the locator but the bounded source retains one
exact Controller line break. That narrow form is limited to an installed
occurrence whose declared quantity equals the exact model count, with every
continuation splitting the repeated model from one of its declared
capabilities; competing line layouts fail closed. Already valid bounded
evidence is never rewritten. Other line joins, distinct spellings, hidden text,
variant suffixes, malformed fields, and non-Controller sources fail closed.

Quantity remains explicitly model-produced and is never inferred or changed
locally. When an exact Controller evidence line starts with `Dual` immediately
scoping the returned product identity, and the model already returned quantity
two with high confidence, the repair lowers only `source_confidence` to medium.
Negated, optional, contextual, trailing, and `Dual Axis` uses do not take this
fast path. Every current extraction, regardless of publisher, must emit a
normalized manufacturer/model identity only once. A quantity greater than one
can never retain high source confidence; without a quantity signal, only one
exact identity with quantity one is eligible for automatic admission.

The Controller adapter treats punctuation-insensitive repeated model mentions
and bounded `Dual`, `#N`, or decimal markers only as quantity ambiguity. It
does not convert them into an expected physical count. This includes negated,
optional, and contextual wording such as `Not Dual`, `Optional Dual`, or
`dual screen`, lone ordinals, and the exact run-on shape with a unique
left-bounded model, slash-delimited alphanumeric suffix, and terminal `(Dual)`
in one evidence line. A high-confidence occurrence with any other such
ambiguity fails into the single avionics correction request. A corrected
medium- or low-confidence candidate may checkpoint only when its exact
`source_evidence_text` covers the complete bounded ambiguity, including count
qualifiers such as `units` or `each`; downstream reuse and valuation continue
to require high source confidence, so it remains pending. Other publishers rely
on the extraction prompt until they have an equally explicit authoritative
equipment-field adapter.

The same unified atomic repair and complete current extraction validator run
after an avionics-only correction. On success, only the transient `avionics`
member is replaced; aircraft identity, visual recovery, price, hours, valuation
facts, and all other primary values remain byte-for-value unchanged. If any
repair or final validation fails, the complete transient payload is restored
and no checkpoint is written.

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
has a complete bounded collision family, one tools-disabled Lite call may
select only an unchanged approved and currently attested catalog ID at
`very_high` confidence. The prompt also includes unreviewed, unattested, and
capability-incompatible family members as nonselectable blockers. The family
is bound to the complete current manufacturer-catalog revision. The caller
re-reads that catalog, rebuilds the family, and revalidates exact listing
evidence, membership, capabilities, selectability, and ambiguity before
accepting it. Overflow, missing closure, uncertainty, or stale input falls
through to grounded curation. Typography-equivalent labels and a trailing
server-owned capability description may be selectable; every other suffix or
generation difference is treated as a meaningful variant by default. Such
variants remain visible as collision blockers but cannot be selected for one
another, and a family containing no structurally compatible approved product
skips the Lite call entirely. Stored source URLs, titles, and excerpts are not
model input. Aircraft reuse is similarly strict: an exact current FAA record
and one applicable approved hierarchy may bypass Gemini entirely.

When a noisy listing splits a high-signal model label across manufacturer and
model fields, or the global catalog exposes exact duplicates and meaningful
suffix neighbors that a manufacturer-scoped shortlist would hide, a distinct
tools-disabled Lite triage may run once. The server supplies the complete
bounded global exact-model family, every unreviewed row, and its suffix
blockers, then rechecks the full catalog fingerprint after the response. A
current approved singleton can proceed only through the unchanged local reuse
and listing-occurrence gates. An unreviewed or multi-row result is merely a
request-scoped Search hint: it cannot approve a row, establish product
existence, or turn listing prose into authoritative evidence, and ordinary
Search, URL Context, structure validation, and complete collision review still
run. Uncertain, negative, invalid, incomplete, overflowed, or stale triage
falls through without changing the observed candidate.

When the exact local avionics row is unique and capability-compatible but
cannot use the deterministic approval fast path, an active
`manufacturer_primary` source-origin record may be used as a retrieval hint.
The hint is selected only within the effective manufacturer identity, must
name the exact maker and model, and is rejected when the catalog or source text
exposes an unresolved suffix variant or duplicate. The origin is re-authorized
and the URL is fetched again; stored evidence text never substitutes for the
fresh publisher document. If that server-side fetch, anchor, or product-proof
preflight fails, the opportunistic path falls back to ordinary Search before
any Gemini request is made. After a structure request begins, or when a caller
explicitly supplied a direct source, failures remain fail-closed and never
open a second research path.

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

Only `avionics_reuse_v2` attestations are current. Their fingerprint domain is
explicitly bound to this target-aware OEM verifier contract. Earlier
attestations and their listing-level corroborations are invalidated rather
than copied forward, so a policy upgrade cannot silently reuse proof admitted
under weaker verifier semantics.

Unrelated oversized or excess rows do not invalidate a targeted projection.
Target-aware PDF projection recursively interprets bounded text-bearing Form
XObjects. Each Form uses its own resource dictionary when present and otherwise
falls back only to the nearest inherited page resource dictionary; page-tree
resources are inherited as a whole dictionary and never merged name by name.
Form matrices compose with the invoking graphics transform, Form invocation
restores the caller's graphics and resolved font state, and Form bounding boxes
and supported rectangular clips must contain the conservative painted extent of
the complete text run, not merely its origin. Repeated invocation is allowed at
each invocation's transform, while active-path cycles are rejected.

Target-relevant overflow, malformed Form types, subtypes, resources, matrices,
bounding boxes, or operators, unsupported reference XObjects, and incomplete
page or resource structures fail closed. Text painted with a missing,
undecodable, or unsupported font is ineligible to establish proof; the verifier
never substitutes a fallback encoding for it.
Every declared page and Form content stream must decompress successfully within
the cumulative page budget and parse without ignored trailing syntax. Font
encodings and `ToUnicode` maps are explicit and validated, except for the
spec-defined StandardEncoding default on exact, unembedded Latin Base-14 Type1
fonts; encoding-difference dictionaries must name their recognized base
encoding. Unsupported custom glyph programs and Type0/CID fonts whose descendant
metrics are not interpreted cannot establish proof. Text-state advancement,
text rise, the leaf Page's non-inherited `UserUnit`, and the graphics transform
at the time a clipping path is constructed are included in layout decisions.
Text under unsupported optional-content, transparency-group, soft-mask,
nonstandard blend, arbitrary clipping, or non-painting text-rendering state
cannot establish deterministic proof. Guarded source downloads are capped at 8
MiB; PDFs are additionally capped at 256 pages, 2 MiB of extracted publisher
text, and 2 MiB of page content plus recursively inspected invoked resources per
page. Distinct invoked fonts and Forms, total XObject invocations, Form
recursion, graphics-state depth, and page-tree depth are independently bounded.
The lopdf loader's eager cross-reference and object-stream decompression cap is
currently per structural stream. The cumulative 2 MiB inspection budget starts
after loading, when page, Form, and font streams are inspected; it does not
aggregate eager structural-stream inflation. A true aggregate load-time
decompression budget requires a shared counter in lopdf's `Reader` and
`ObjectStream` paths and cannot be proven by application-level post-load checks.

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

The extraction prompt receives bounded listing text and returns:

Controller captures use the publisher's unique listing main, primary asking
price, and direct specification heading/label/value structure. Each label and
value is retained in an explicit separate envelope, including author-entered
line breaks in Description and Avionics/Radios values. Raw JSON-LD is excluded;
the adapter may extract only availability from an exact Schema.org `Offer`
owned by the Schema.org `Product` whose numeric `@id` exactly matches the
validated listing route. `InStock`
establishes an active sale listing. Other or missing availability remains
unknown unless the listing explicitly states sold or pending, and aircraft
Condition values such as New or Used never substitute for sale lifecycle.
Financing amounts, navigation, advertising, service-log link chrome, and
related listings are not part of this extraction source. A recognized
Controller URL never falls back to broad page cleaning: missing or ambiguous
structure and any critical-section or total-source overflow fail closed before
a Gemini request.
Other publishers continue to use generic visible-page cleaning.

The extractor returns:

- manufacturer
- model family
- variant
- model year
- asking price and currency
- airframe hours and, only when explicitly stated, nullable engine and
  propeller hours with their source labels (engine: `SNEW`, `SMOH`, `SFOH`, or
  `SFRM`; propeller: `SNEW`, `SMOH`, `SFOH`, or `SPOH`), evidence text, and
  confidence. `STOH`/`TSTOH` is top-overhaul-only time and is omitted rather
  than reinterpreted as an engine time basis
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

Existing FAA-blocked listings use the same visual contract through an explicit
reviewer repair action. Provider-free preflight returns a hash of the current
listing identity and retained capture plus the allowlisted retained visual
assets; it never downloads an image or calls Gemini. The reviewer selects one
asset. The server downloads exactly that current asset through the guarded
media path and sends only those bytes to the visual model. An applied decision
stores that fresh download's SHA-256, byte count, MIME type, visible bounding
evidence, interaction/model identity, and prompt/schema versions. A mutable CDN
URL is not proof that the bytes equal an earlier ingestion download.

Visual recovery is only a candidate. Missing, invalid, or currently unassigned
registrations must still pass exact target-scoped admission in the latest FAA
MASTER projection. A visually unchanged N-number absent from current MASTER
returns `recovered_registration_not_found` and changes no listing data. A valid
candidate outside the imported target set returns `faa_target_import_required`
and likewise performs no listing mutation. The workflow does not store or use
RESERVED or DEREG owner data. A narrow, exact-source serial typo on an already
exact current FAA N-number uses the provider-free FAA serial correction instead
of a photo. Other serial conflicts remain manual.

Clean replay uses that same provider-free serial rule before materialization.
Only a `serial_conflict` may be retried against the exact current FAA row for
the same N-number. Both the N-number and observed serial must be exact visible
retained-source spans. The observed and FAA serials must differ by only one
internal insertion, deletion, substitution, or adjacent transposition while
retaining their first and last two normalized characters. The working
`ParsedListing` receives the FAA serial, while the extraction checkpoint
remains byte-for-byte unchanged. After the capture is bound, the workflow
rechecks the retained source, current FAA projection, exact submission binding,
and raw extraction before recording the raw value, corrected FAA value,
capture and extraction digests, snapshot, and FAA source-record digest as
immutable history. This provider-free path does not ask Gemini to infer or
repair an identifier.

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

For bounded factory applicability, Gemini may identify the approved
make-specific serial-shape scheme and return source-visible display bounds and
an optional prefix. It never supplies database sort keys. The application
normalizes the displays and derives the universal natural-order key; SQLite and
PostgreSQL recompute it independently before accepting the row. Different
scheme IDs or prefixes therefore cannot create separate overlap domains.

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

After request validation and before any local, catalog, or provider identity
work, an observation whose complete typography-normalized model equals one
label in the server-owned generic equipment-category vocabulary is rejected
deterministically. This is whole-label equality only: manufacturer names,
capability types, substrings, fuzzy similarity, and product labels containing a
generic term do not authorize discard.

Every remaining candidate first goes through a local catalog pass:

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

For other observations that reach grounded research, a tools-disabled
concreteness classifier provides a separate narrow discard path. The server
accepts its exact response shape and automatically rejects only `generic` at
`very_high` confidence when
`model_identifies_single_unit=false` and the response contains concrete,
non-empty generic indicators. Classifier errors, unknown or malformed fields,
blank indicators, `ambiguous`, and every weaker-confidence answer continue into
ordinary grounding. All supplied context is explicitly labeled untrusted; the
classifier cannot approve or create a catalog identity.

Automatic review applies that deterministic closed-vocabulary policy only
after validating the complete retained observation. A current-schema
observation with valid capabilities, quantity, source confidence, installation
action, and replacement graph is discarded without a provider request when its
complete normalized model is a generic category. Structurally malformed
observations remain pending; similarity or partial label overlap never reaches
this discard path.

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
4. Only after all checks pass does one transaction promote the confirmed
   legacy row or create a new `approved` row. When the review marks multiple
   unreviewed rows as the same product, automatic consolidation is limited to
   the complete group sharing one exact stored normalized model and one
   effective manufacturer identity. The write transaction rechecks full
   catalog and manufacturer-collision fingerprints, exact membership, stable
   identifier compatibility, and every association remap. It then approves one
   survivor atomically or leaves the observation pending; model similarity,
   descriptive aliases, and meaningful variants never authorize this path.

Grounded product approval and cross-run reuse are separate conclusions. A
verified product is approved even when its evidence URL is outside every
independently curated, active exact `manufacturer_primary` origin. That
approval does not create or widen a manufacturer source origin, receives no
current reuse attestation, and remains ineligible for the no-Gemini local fast
path. A later origin-specific verification may create the reuse attestation
without changing the approved product identity.

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
quarantine. Provider, persistence, and enrichment failures tied to the listing,
its FAA admission, or its listing-specific evidence remain real ingestion
errors and can still quarantine a stored listing. Shared factory-reference
readiness is separate: once those listing-specific checks succeed, finalization
makes the listing `ready` and verified. Valuation reports a typed reference gap
when the applicable factory specification, price, or configuration is
unavailable; it does not move the valid listing back to `incomplete`.

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

Occurrence evidence and its confidence are an inseparable pair. Structured
`observed_text`, catalog identity evidence, resolver explanations, and legacy
listing-link notes never fill a missing `source_evidence_text`. Legacy review
backfill retains complete extraction evidence pairs, but stages unmatched
installed/replacement link notes without evidence. The explicit local restage
path can recover a unique exact visible manufacturer/model slice from the
bound plugin capture without Gemini; otherwise the aspect remains pending. If
that canonical repair changes a link that was already covered by a current
corroboration and collision scope, the same transaction reissues both proofs
for the repaired exact slice. Restage does not mint a new occurrence conclusion
when no current pre-repair corroboration exists.

The explicitly confirmed avionics review rebuild is also provider-free. It
reprojects machine-owned cards from a strict, capture-bound retained extraction
and current database facts; it neither invokes Gemini nor changes ordinary
restage behavior. Without a durable discard receipt, an extraction occurrence
that lacks a one-to-one current link or residual-review claim produces the
typed `blocked`/`occurrence_disposition_unknown` result with no review mutation.
The API and browser use fixed copy for all rebuild block codes and never expose
provider, parser, or database error details.

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
listing becomes `ready` and verified once FAA admission, listing-specific
enrichment, and the listing readiness pass succeed. Missing, incomplete, or
not-yet-approved shared factory specifications, model-year prices, and factory
configurations are valuation gaps: they prevent that valuation row from being
served, snapshotted, or used for training, but do not make the listing itself
incomplete. An actual listing, FAA, or listing-specific enrichment failure is
persisted as `quarantined`; neither path rolls back or holds a network request
inside the catalog/link transaction. Associations explicitly corroborated by a
reviewer use `listing_review` provenance with high installation confidence and
are valuation-eligible wherever equivalent high-confidence `listing`
associations are accepted.

## Grounded Product And Factory Facts

Gemini research is staged by claim type. Search discovery finds candidate
primary documents; URL Context verifies the exact resolved URLs; a
tools-disabled schema pass converts only verified claims into normalized facts;
and independent adjudication binds those facts to existing approved catalog and
decision IDs. Search output, URL-context windows, prompts, and provider
responses are request-scoped and are discarded after adjudication.

Aircraft factory-reference research covers the exact reviewed
make/family/designation/generation/package, model year, market, and serial scope.
It must independently establish:

- a direct USD price for the full standard configuration and the nominal dollar
  year in which that price was published;
- the complete standard avionics, engine, propeller, and material-feature sets;
  and
- primary-source applicability for every normalized fact.

The four fact sets each carry a completeness claim, including when the reviewed
set is empty. A marketplace listing can motivate research but cannot establish
a reusable factory configuration or price. Manufacturer evidence is primary
for commercial configuration and price; FAA evidence remains controlling for
registration, serial, certification, and production facts within its scope.

The persistence boundary accepts a strict normalized JSON draft containing only
approved catalog IDs, approval-decision IDs, validated claim IDs, applicability,
and normalized values. The exact insert-and-publication transaction is exercised
in rollback-only mode by default:

The draft's `direct_cited_amount_usd` and
`direct_cited_nominal_dollar_year` fields must reproduce the cited nominal
MSRP. They must never contain a model-generated inflation adjustment. A
separate optional `dollar_normalization` fact contains the source/target years,
official index series, both official index values, their exact factor, and a
validated regulator-primary claim ID. Gemini may structure already-grounded
values, but database gates—not model confidence—admit the fact. Serving and
snapshot creation consume it directly; a missing pair is the deterministic
`reference_price_dollar_normalization_missing` gap rather than another Gemini
task.

```sh
cargo run --bin aircost-admin -- \
  publish-aircraft-reference --draft normalized-reference.json
```

`--apply` persists and publishes the version only after database gates recheck
all primary evidence, complete fact sets, the exact-model-year full-configuration
price, valuation-ready avionics, and non-overlapping applicability. A correction
atomically supersedes its exact published predecessor and publishes the higher
revision; any failure rolls both state changes back.

Confidence alone is not valuation eligibility. Avionics identities must resolve
to approved concrete products or named suites; integrated suite membership must
be explicit so bundled units are not counted twice. Installed contribution and
replacement cost remain separate facts. Reference and avionics dollar values
retain their actual nominal year. Official regulator-backed index facts perform
the only accepted valuation-year conversion; serving fails closed when the
exact pair is absent. Installed-contribution values are current resale values:
the full listing delta is applied after factory-aircraft age/hour scaling and
held constant in valuation-year dollars across the displayed curve.

LLM completion does not make a listing ready by itself. Deterministic readiness
rechecks the current exact FAA assignment, listing-specific avionics, and the
unique complete published reference applicable to the FAA serial. Reference
gaps are recomputed from current state and are not stored in a second pending
table. Expected avionics uncertainty remains `pending_review`; failed aircraft
admission or invalid listing evidence is quarantined. All non-ready states stay
out of snapshots, training, and serving.

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
