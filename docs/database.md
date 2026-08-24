# Database Schema And Write Lifecycle

The app supports SQLite and Postgres through the same Rust data-access layer.
Schemas live in `schema/sqlite.sql` and `schema/postgres.sql`; `src/db.rs`
loads the correct schema on
startup and seeds the developer user plus baseline depreciation profiles.

## Core Tables

`users`

Stores local or authenticated users. Development defaults to
`developer@localhost`.

`aircraft_manufacturers`, `aircraft_models`, `aircraft_model_variants`

Normalize aircraft identity. Variants point to models; models point to
manufacturers. Display names are stored with normalized keys for matching and
deduplication.

`aircraft_makes`, `aircraft_model_families`, `aircraft_designations`,
`aircraft_generations`, `aircraft_factory_packages`

Form the reviewed aircraft identity hierarchy used by the new curation path.
Aliases and external identifiers are separate records; a normalized string or
an FAA reference code is a candidate-retrieval key, not automatic identity
proof. Generation and factory package remain distinct because a designation
such as `SR22` does not by itself establish a generation such as `G6` or a
package such as `GTS`.

An aircraft hierarchy decision may use `no_supported_selection` only for
generation or package. This is an approved operational `NULL` for one exact,
grounded catalog result: every retained model/variant token is accounted for
by the exact FAA designation or a positively typed hierarchy selection,
targeted primary-source research found no positive candidate for the NULL
dimension, and the catalog has no applicable relationship. Relationship
absence is rechecked transactionally on every apply and replay. It is not a
claim that the real-world dimension does not exist, carries no evidence or
selected entity, and cannot be used for required make/family/designation
decisions.

`aircraft_reference_configurations`,
`aircraft_reference_configuration_versions`, and reference applicability,
price, avionics, engine, propeller, and feature tables

Store immutable, reviewed factory-reference configurations with explicit
model-year, serial, and market applicability. A correction creates a successor
version; it does not rewrite a published version. Legacy model specs, price
points, and default-avionics rows are not promoted into this catalog by the
migration.

`aircraft_reference_fact_set_attestations` proves that the avionics, engines,
propellers, and material-feature sets were each reviewed for completeness. An
empty set is therefore distinguishable from an unresearched set. Publication
requires all four attestations, exactly one direct exact-model-year USD price
for the full standard configuration, validated primary evidence for every fact,
valuation-ready factory avionics, and non-overlapping applicability. A fact's
nominal dollar year is retained as published and is not forced to equal the
aircraft model year.

`faa_registry_snapshots`, `faa_registry_aircraft`,
`faa_registry_aircraft_references`, `faa_registry_engine_references`,
`faa_registry_coverage`

Store immutable, target-scoped projections of the FAA releasable registry for
aircraft hierarchy curation. Every imported target has an explicit `matched` or
`absent` coverage row. Only matching `MASTER` rows and the `ACFTREF` and
`ENGINE` rows reachable through their opaque codes are retained. Owner names,
addresses, other names, Mode-S values, unrelated registrations, and all other
archive members are excluded. These tables provide registration-identity
evidence only; they do not populate the canonical aircraft catalog.

`curation_evidence_sources`, `curation_evidence_claims`

Store only durable source identity and atomic cited claims used by active
approval, provenance, assignment, applicability, and reference-fact
constraints. They do not store a Gemini prompt, response, Search result set, or
complete URL-context dossier. Same-case Gemini corrections may reuse verified
working evidence in memory; cross-run matching uses approved catalog facts and
IDs instead of replaying source material.

There is deliberately no generic `faa_source_records`, `faa_source_snapshots`,
or `faa_source_checks` cache for avionics. The current FAA registry import has a
documented, versioned bulk source and is used as controlling evidence for
aircraft registration identity. The avionics sources investigated so far do
not expose an equivalent stable, unauthenticated product-identity feed: DRS API
access is credentialed and its document records are not a product catalog, and
ADS-B equipment data does not establish the exact marketed unit installed in a
listing. If a future FAA source is integrated, it should receive a typed,
source-specific snapshot schema with an explicit provenance and refresh
contract rather than being hidden behind an unused generic cache.

`aircraft_sale_listings`

Stores canonical sale listing facts: model variant, source URL, model year,
asking price, currency, status, registration, serial number, and airframe,
engine, and propeller hours. `ingestion_state` keeps `incomplete`,
`pending_review`, and `quarantined` rows out of serving and training. A pending
review is an expected curation state, while quarantine records a failed
completion. Component times are nullable and carry an explicit basis, evidence
text, and confidence; a missing time is not converted to zero or copied from
the airframe. High-confidence installed engine and propeller identities are
linked separately from the factory configuration.

Factory-reference readiness is deliberately independent of listing readiness.
Once the FAA-backed identity and every listing-specific review and persistence
check pass, the listing is `ready` and verified even when no applicable
published factory configuration exists yet. Valuation resolves the shared
reference separately and returns typed reference gaps instead of an estimate;
snapshot and training construction likewise omit that unusable valuation row.
Reference curation therefore never rewrites a valid listing back to
`incomplete`, and there is no persistent or derived `PendingReference`
listing outcome.

`aircraft_sale_listing_facts`

Stores source-backed condition, restoration, damage/log, and material
conversion facts that explain value without redefining the factory variant.

`plugin_installs`, `plugin_submissions`

Store Chrome extension registrations and submitted rendered HTML. Submissions
retain the HTML, extraction result or error, and the canonical listing created
from the submission when extraction succeeds.

`listing_replay_runs`, `listing_replay_run_items`,
`plugin_submission_materialization_receipts`

Coordinate manifest-backed clean replay without copying HTML or raw provider
response envelopes into the replay ledger. A run pins the trusted-manifest
version, SHA-256, and member count and holds an owner-token heartbeat while
active. Each unique run/submission member stores the expected capture SHA-256,
independent typed extraction and materialization state, the exact normalized
successful extraction JSON and its checkpoint SHA-256 for immutable resume,
attempts/timestamps, an optional resulting listing ID, and closed terminal
rejection or retry-failure codes. A partial unique index
permits one active replay owner across processes. Explicit stale recovery
fences the prior token; loss of ownership cancels the in-flight operation, and
checkpoint and exact completion state are re-derived before a provider-backed
retry. Listing insertion and capture binding commit atomically. Binding alone
does not imply completion: the exact receipt stores the bound listing plus
rendered-capture and extraction-checkpoint hashes only after review and
occurrence child projections finish. Checkpoint storage is first-writer
immutable, and materialization compare-and-sets against the member's pinned
checkpoint hash. A succeeded materialization must retain its non-null resulting
listing ID. Both that result and the exact materialization receipt use
`ON DELETE RESTRICT`, so replay provenance prevents deletion of the listing it
proves was produced. Startup attests both complete replay
table definitions on SQLite and the exact PostgreSQL column/type/nullability/
default/identity, primary-key, unique, foreign-key/delete-action, check-
vocabulary/hash, and full index/backing-index contracts, including key versus
included attributes, collations, operator classes, options, predicates,
expressions, null-distinctness, and lifecycle flags. Same-name objects with
weakened columns, constraints, or indexes do not satisfy the migration contract.
Marker-present migration reruns perform that complete attestation before any
replay DDL. They also reject unexpected attached indexes, triggers, policies,
rules, inheritance, partitioning, row-security behavior, or nonordinary table
kind/persistence. The original migration `installed_at` is an immutable install
receipt: exact reruns and normal application startups validate the version and
fingerprint but never replace its timestamp.

`gemini_api_usage`

Stores one accounting row per logical Gemini provider request, including its
task/purpose, API family and version, pinned model, service tier, status,
application/source correlations, request fingerprint, nullable provider usage
counters, transport attempt counts, latency, validation result, error, and an
optional dated paid-list cost estimate. Transport retries stay on the same row;
separate correction and review requests receive separate rows. Missing provider
counters remain null, and cost remains unknown rather than treating missing
usage as zero. The table stores no prompt text, response body, downloaded image
bytes, or API key.

The `benchmark-gemini` command is read-only when `--execute` is omitted. It
samples only retained source submissions linked to canonical listings. With
`--execute`, its only database writes are these usage-accounting rows; it never
updates a listing, plugin submission, catalog, or other domain row.

`engine_manufacturers`, `engine_models`

Store reusable engine metadata: manufacturer, model, TBO, overhaul cost, value
reference year, and source information. Listing-only identity evidence does not
make TBO/cost data valuation-eligible; those fields require an authoritative
component reference.

`propeller_manufacturers`, `propeller_models`

Store reusable propeller metadata with the same role as engine metadata.

`avionics_manufacturers`, `avionics_types`, `avionics_models`,
`avionics_model_types`

Store concrete avionics units or named suites. Generic entries such as
`Autopilot`, `GPS`, or aircraft-maker-as-avionics-maker labels should not be
stored as durable avionics models. `catalog_status` separates the curated
`approved` catalog from preserved legacy `unreviewed` rows. Approval requires a
stable manufacturer part/model number or authoritative SKU, its normalized
uniqueness key, `very_high` identity confidence, authoritative non-listing
evidence, and a review timestamp. A documented legacy manufacturer model
number may equal the canonical model label; a separate OEM LRU part number is
not required. Listing association is a separate claim: one current attested
product may be selected locally from a unique, complete, exact
manufacturer/model occurrence, while non-exact prefix/suffix similarity remains
review-only candidate retrieval. Catalog approval does not itself create
manufacturer-wide source trust. A grounded product whose exact evidence origin
is not independently curated may remain approved, but it has no current reuse
attestation and is excluded from the no-Gemini local path until that exact
manufacturer origin is separately approved.
One physical product can expose multiple capabilities through
`avionics_model_types`; for example, one GNX 375 identity can be both GPS and
transponder equipment without duplicating the product. Types are not part of
the product identity and exist only through this many-to-many table.
Catalog writes are staged as unreviewed product insertion, capability
membership insertion, then approval. Database triggers require every approved
product to retain at least one capability while still allowing a product delete
to cascade through its memberships.
Installed resale contribution and replacement cost have distinct fields.
`valuation_scope` distinguishes units from integrated suites, while
`avionics_suite_components` records grounded containment so a suite and its
constituents are not counted twice. An installed-contribution value is usable
only with a non-empty recorded value source. Identity approval does not approve
numeric metadata; legacy values and suite memberships are cleared when a row is
promoted and must be grounded separately.

`avionics_manufacturer_canonical_keys` is only a deterministic lookup key.
`avionics_manufacturer_identities` is the curated, authoritative manufacturer
namespace, and immutable memberships retain every raw manufacturer spelling.
Only punctuation/spacing variants with the exact same deterministic key attach
automatically to an existing evidence-backed identity. Semantic aliases are
stored in `avionics_manufacturer_alias_candidates` until a human reviewer
corroborates official evidence. A reviewed redirect is append-only; raw maker
rows and their original memberships are never rewritten or deleted.

`avionics_approved_product_identities` is the uniqueness registry for approved
products. Its product-name and stable-identifier constraints are scoped by the
effective manufacturer identity, not a raw spelling row. Identifier kind
remains part of identity: a SKU value is not automatically equal to the same
text used as a part or model number. An approved manufacturer redirect is
blocked while the two namespaces contain an exact product collision. The
human decision remains recorded, the explicit product-consolidation workflow
adjudicates those rows, and only then can the redirect be finalized.

`avionics_legacy_manufacturer_alias_signals` is a read-only review aid for
cross-maker exact product-name or exact stable-identifier matches. It does not
create curated identities from unreviewed legacy names. Once one side gains
authoritative evidence, matching unassigned makers are staged as pending alias
candidates without approval.

`avionics_catalog_consolidation_guard` is the transient stable-identifier
transaction guard, not a review queue or durable merge log. It requires equal
canonical manufacturer plus equal non-empty stable identifier kind and
normalized value.

Grounded exact-model authority uses separate transient
`avionics_catalog_grounded_consolidation_authorizations`, `_guard`, and
`_claim` tables. The header binds the reviewed catalog and manufacturer
collision fingerprints to one effective manufacturer, exact stored model key,
survivor, and complete member count. Pair rows must enumerate every
non-survivor, and only a claim that rechecks the complete current group and
identifier compatibility exposes those pairs to remap triggers. Descriptive
expansions and meaningful variants cannot use this authority. While a claim is
active, endpoint identities and statuses are immutable. Duplicate deletion
consumes every pair by cascade; the transaction must then remove the claim and
header and verify that all transient rows are gone before committing. No Gemini
response or URL-context dossier is retained.

`aircraft_sale_listing_avionics`

Links concrete avionics units to a specific sale listing. The link stores
quantity, provenance, evidence confidence, and an explicit `installed`,
`replaces`, or `removes` configuration action with an optional replacement
target. Valuation starts from the applicable published reference profile and
applies these links as deltas. New primary and replacement links require approved catalog identities;
the installation-evidence confidence on the link is independent from catalog
identity confidence. A listing cannot contain the same canonical product twice
or install and replace the same product. Ready or verified listing associations
are immutable. A transition to `ready` additionally requires positive quantity,
approved endpoints, high confidence, and `listing` or `listing_review`
provenance for every avionics link.

`aircraft_sale_listing_pending_reviews`

Stores the durable handoff for listing avionics that cannot be resolved with
the confidence required for a canonical association. There is exactly one
bundle per listing, containing all pending aspects, an optional retained plugin
submission reference, the extraction fingerprint, the serialized payload and
its hash, and the approved-catalog revision against which it was prepared. The
row moves the listing to `pending_review`; unresolved text is not inserted into
`avionics_models` or `aircraft_sale_listing_avionics` merely so it can be
reviewed. Replacing the bundle is an upsert, and deleting the listing cascades
to its bundle.

An explicit avionics review rebuild is permitted only from an exactly bound
retained submission using the current explicit occurrence schema. Before any
write, every extracted occurrence must be represented one-to-one by a current
listing link or residual avionics aspect; reviewer corrections and their
connected replacement components are preserved exactly. The database has no
durable historical discard ledger, so an occurrence with no such claim cannot
be safely distinguished from a prior discard. That case returns `blocked` with
the stable `occurrence_disposition_unknown` reason code, rolls back the
transaction, and leaves the pending review byte-for-byte unchanged rather than
reconstructing or guessing review state. Reviews containing non-avionics state
are also refused before review mutation because this reset has an avionics-only
public contract.

`valuation_snapshots`, `valuation_snapshot_rows`

Freeze the listing-only training contract, selection policy, duplicate groups,
row hashes, included and excluded records, and authoritative feature JSON.
Snapshot rows retain copied source listing IDs rather than cascading from live
listings.

`valuation_model_versions`, `valuation_model_artifacts`,
`valuation_fold_predictions`

Store candidate/active/retired structural or DNN versions, hash-verified
artifacts, and grouped held-out predictions. Only one version of each model
kind can be active. Activation verifies validation gates and the artifact hash,
then retires the previous active version and activates the candidate in one
transaction.

`valuation_refresh_state`

Records that listing mutations have made the latest frozen snapshot stale.
Listing writes no longer trigger an implicit best-effort model refit.

Rental tables (`rental_clubs`, `rental_club_cost_versions`,
`rental_aircraft_offerings`, `rental_rate_versions`) are separate roots that can
also reference aircraft variants.

## Insert Path

Listings are created through either the web API or plugin submission path:

- `POST /api/listings` previews a URL or manual listing, then calls
  `create_listing`.
- `POST /api/plugin/submissions` verifies the plugin signature, extracts the
  listing from rendered HTML, then calls `create_listing`.
- `POST /api/plugin/submissions/{id}/reprocess` replays stored HTML through the
  same extraction and insertion path.

`create_listing` performs these steps:

1. Validate creation-critical listing fields.
2. Require the submitted registration and serial to pass the newest imported
   FAA projection. Missing, foreign, malformed, uncovered, absent, ambiguous,
   and serial-conflicting aircraft are rejected before normalization, Gemini,
   catalog changes, or listing-row mutation.
3. If this is an unverified same-source row with a blank registration, persist
   only the canonical FAA N-number and serial with an atomic compare-and-set.
   The row remains quarantined until full ingestion succeeds, but later
   enrichment failure cannot erase the regulator-confirmed identity.
4. Normalize manufacturer/model/variant.
5. Compare model-family and variant candidates from known DB rows.
6. Ask Gemini to confirm plausible candidate matches when string similarity is
   insufficient.
7. Correct non-conforming variant labels when they include maker or model year.
8. Reject a structurally valid observation provider-free when its complete
   normalized model is in the closed generic-category vocabulary. For every
   other unresolved observation, build a similarity shortlist from the server
   catalog, then ask grounded Gemini to select an existing ID, propose a
   verified new identity, reject unsupported text, or fail unresolved.
   Similarity and exact normalized strings are retrieval aids only. Every
   positive identity—including an already approved match—undergoes an
   independent proposal attestation and candidate-by-candidate collision
   review before it can be associated.
9. Keep only verified canonical outcomes. An approved identity can become a
   listing association. A primary candidate is automatically discarded as
   unsupported for reasons outside the closed generic-category policy only
   when Gemini returns high confidence, selects a structured
   `rejection_basis`, and states a candidate-specific negative `reason`
   consistent with that basis. The whole normalized reason must occur
   in one Google Search grounding support span linked to a cited source, and
   must explicitly name the observed model and its usable manufacturer.
   Identity-only, unrelated, fragmented, or contradictory citation support is
   not rejection evidence. An unsafe rejection is corrected once and otherwise
   becomes unresolved. An unresolved identity is converted into a review
   aspect; its raw label is not inserted into the catalog. Replacement and
   removal targets are resolved independently so uncertain configuration
   semantics remain explicit.
10. Upsert manufacturer, model, and variant lookup rows.
11. Insert the listing, or update an equivalent existing listing, in the
   `incomplete` ingestion state, and replace source-backed approved avionics,
   installed-component identities, and valuation facts.
12. If any avionics aspects remain unresolved, atomically upsert the complete
   one-row review bundle and set the listing to `pending_review`. Enrichment is
   skipped while that row exists.
13. Otherwise, complete and validate the remaining listing-specific evidence
   and canonical associations. Resolve factory-reference readiness separately
   for valuation; a missing model-year reference is reported as typed gaps and
   does not block listing verification.
14. Mark the listing `ready` after every listing-specific readiness query
   passes, regardless of shared factory-reference availability. A failed
   listing/FAA admission, listing-specific
   persistence, or listing-specific enrichment completion remains stored as
   `quarantined` with the error for inspection or reprocessing; expected
   identity uncertainty remains `pending_review` instead.
15. Mark valuation snapshots stale and remove orphaned lookup rows.

Review resolution is a separate lifecycle. Every ordinary extracted avionics
aspect offers three actions, and the reviewer must submit exactly one: use an
existing approved product, create a verified product, or discard the
observation with a reason. Creation requires one or more canonical capabilities, a stable
manufacturer identifier kind and value, and an authoritative source URL,
title, and evidence text. An unlinked observation receives an explicit legacy
promotion target only when its normalized identity selects exactly one catalog
row and that row is `unreviewed`. An aspect covering an existing legacy listing
association can instead expose that exact row by its covered catalog ID. In
either case the ID is only a candidate: a matching create decision may promote
it only after identity, status, normalized-identity uniqueness, identifier and
model collisions, global references, and exact cross-listing coverage are
rechecked under lock. A corrected manufacturer/model creates a separate
approved identity and leaves the old candidate and unrelated links untouched.

Hash-bound approved-product aspects use a product-centric workflow. One current
OEM attestation is shared by every pending occurrence of that product. The
deterministic source proof is bound to the complete manufacturer-scoped
collision snapshot, and the write transaction rechecks both that snapshot and
ownership of the hash-bound pending aspect. The attestation preflight accepts
one listing ID, review hash, and aspect ID and loads only that review; an
unrelated malformed review cannot poison or authorize the operation.
Existing-product verification then uses only retained listing text and the
live local catalog; it accepts no OEM dossier and never invokes Gemini. The
aspect's `source_evidence_text` must be one exact, bounded structurally visible
body span from the immutable `plugin_submissions.rendered_html` capture
attached to the review. Only HTML entity and whitespace normalization is
allowed. Structural visibility excludes head and executable content, hidden
attributes, inline or embedded stylesheet hiding, and closed details/dialog
containers. It does not claim browser-computed visibility from external CSS,
which is absent from the retained outer HTML. The capture must have its stored
content hash, owner, and exact `canonical_listing_id`; missing captures,
generated explanations, hidden metadata, and corrected text remain pending. A
synthetic preserved-link aspect records occurrence
corroboration without rewriting the link. It accepts any unchanged positive
quantity, including quantities greater than one, only when the staged aspect
and current listing link still agree under the mutation lock. An ordinary
installed, non-replacement aspect may cover zero or one installed link and is
committed through the normal aspect-scoped `use-existing` transaction with its
exact quantity. The transaction re-reads the capture and rechecks its hash and
visible evidence under the mutation lock, together with the hash-bound target,
current reuse attestation, approved-catalog revision, active
identity-collision closure, exact covered-link ownership, and listing action
graph. Coupled replacement aspects, ambiguous identities, implicit merges,
and stale collision decisions remain pending.

`source_evidence_text` and `source_confidence` are one paired occurrence-proof
value: neither is retained without the other, and `observed_text` is never an
evidence fallback. Before review, the explicit restage transaction rechecks
existing listing-link notes against the bound plugin capture. A unique,
unqualified exact manufacturer/model occurrence replaces generated or stale
notes with the visible source slice at high confidence for any unchanged
positive quantity. When that repair changes the note of an association that
already has a current hash-bound corroboration and collision scope, restage
atomically reissues both against the repaired exact slice; it never creates a
new corroboration from recovered text alone. An exact capture-backed existing
pair is retained even when the association shape still requires manual review.
For an otherwise auto-repairable installed link, ambiguous or missing source
clears both link fields and both staged fields atomically. Unsupported
replacement or unapproved shapes are not blanket-rewritten; their unverified
link notes are excluded from review evidence. The reviewer endpoint
`POST /api/review/listings/{id}/restage` is the apply path for this repair and
updates the review hash in the same transaction.

Restage remains lossless maintenance of the existing review. The separately
named `POST /api/review/listings/{id}/avionics/rebuild` boundary is the only
path that replaces machine-owned avionics cards from the complete retained
extraction. It is provider-free, current-review-hash guarded, and fail-closed
when the retained extraction or occurrence coverage proof is incomplete.

Reuse attestations use the `avionics_reuse_v2` policy and a v2 fingerprint
domain that identifies the target-aware OEM proof semantics. The v2 migration
does not promote or rewrite v1 conclusions: it removes every v1 product
attestation and lets the dependent listing corroborations and collision scopes
be removed with it. Catalog products, listing links, and pending reviews are
preserved and must earn new positive conclusions through the current workflow.

The server checks mandatory FAA admission before entering this catalog-writing
transaction. The transaction rejects stale payload or catalog hashes, applies
all catalog decisions and only the exact covered listing-link ID/role pairs,
removes the bundle, and returns the listing to `incomplete`. After commit, the
server rechecks FAA admission before final publication. Successful
source-backed completion becomes `ready` and verified after the
listing-specific checks pass. Missing or not-yet-approved shared
factory-reference data does not change that listing state: valuation reports
the current typed reference gaps until independent reference publication makes
an exact configuration available. Actual FAA admission,
listing evidence, listing-specific persistence, and listing-specific
enrichment failures become `quarantined`. If a new pending bundle appears
while post-review enrichment is running, that bundle wins: the listing remains
`pending_review` instead of becoming a stranded quarantined review.
Reviewer-accepted listing associations are stored with
high installation confidence and a `listing_review` source, which is eligible
where valuation reads otherwise accept high-confidence `listing` evidence.
Network enrichment is intentionally not held inside the review transaction.

The listing insert path deliberately keeps code generic. If a Cessna, Cirrus, or
another maker needs better results, the preferred fix is better prompts, better
validation, or better data in reusable tables.

## Update Path

`PATCH /api/listings/{id}` merges provided fields into the current listing,
then applies the same mandatory FAA admission check before variant correction,
avionics resolution, or any database mutation. An update cannot retain or
introduce a non-N, unresolved, or serial-conflicting aircraft identity.
Avionics are an explicit replacement boundary. A patch without an `avionics`
member does not invoke avionics identity resolution and does not rewrite
listing-avionics links or the pending bundle; ordinary price, status, hours,
and similar edits preserve its hashes and exact covered link IDs. Supplying any
avionics-resolution context field—`manufacturer`, `model`, `variant`,
`model_year`, `source_url`, `registration_number`, or `serial_number`—requires
an explicit valid `avionics` array in the same patch. Null, non-array, or
malformed avionics fail before mutation. An explicit empty array is a
deliberate complete replacement that clears the old links and pending evidence;
a non-empty array is resolved and restaged as needed. Admitted updates return
the row to `incomplete` and run the same completion or pending-review path as
inserts. If explicit restaging fails after links were replaced, the server
removes the now-stale prior bundle before quarantining the listing, so exact
coverage cannot point at obsolete link IDs. If the update moves a listing to a
different aircraft model, both the
old and new model scopes make the frozen valuation snapshot stale. No listing
write implicitly refits or activates a model.

## Removal Path

`DELETE /api/listings/{id}` detaches any retained plugin submissions from their
canonical listing, removes the listing, refits the affected model, and runs
orphan cleanup. The detach and listing deletion are atomic. Plugin submissions
retain their signed rendered HTML and extraction history after the canonical
listing is removed.

The cleanup code deletes unreferenced generated child records first, then
removes unreferenced lookup rows:

- default avionics, price points, and specs for variants that no listing or
  rental offering references
- aircraft variants with no listing, rental, spec, price-point, or default
  avionics references
- aircraft models with no variants or specs
- aircraft manufacturers with no models
- engine and propeller models with no aircraft spec references
- engine and propeller manufacturers with no models

Avionics catalog candidates, raw manufacturer spellings, and capability rows
are deliberately excluded from generic orphan cleanup. Pending-review payloads
can cite them without a foreign key, so only an explicit global catalog audit
may delete them after proving that no relational role or review bundle refers
to them.

The admin command is:

```bash
cargo run --bin aircost-admin -- cleanup-orphans
```

## Curation And Enrichment Commands

Aircraft labels from listings are no longer mechanically healed or normalized
into catalog rows. They remain review input until FAA-backed identity curation
selects an existing canonical hierarchy or admits a new one. Dry-run commands
are available for the remaining evidence-backed workflows:

```bash
cargo run --bin aircost-admin -- curate-avionics --dry-run
cargo run --bin aircost-admin -- verify-listings --limit 10 --preview
cargo run --bin aircost-admin -- stage-listing-reviews --limit 100 --dry-run
cargo run --bin aircost-admin -- enrich-avionics --dry-run
```

Use `--apply` only after reviewing the report.

### FAA registry import and hierarchy-admission gate

Aircraft hierarchy curation requires a current, privacy-minimized projection
of the FAA releasable registry. Download the official ZIP from the
[FAA Releasable Aircraft Database Download](https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download),
verify the release date, and give that complete archive directly to the
importer. Do not extract its members for import. The application computes the
SHA-256 of the exact ZIP bytes, validates the central directory, and streams
the required members from that same archive.

The importer accepts a conventional single-disk, non-ZIP64 archive with at
most 256 uniquely named, safe-path entries. It requires exactly one root
`MASTER.txt`, `ACFTREF.txt`, and `ENGINE.txt`; each must be a nonempty,
unencrypted regular file using stored or deflated compression and remain below
its source-specific size limit. Duplicate names, nested substitutes, unsafe
paths, unsupported compression, inconsistent directory metadata, oversized
archives or members, and malformed ZIPs abort the import before any database
write.

The importer projects these exact source columns:

- `MASTER.txt`: `N-NUMBER`, `SERIAL NUMBER`, `MFR MDL CODE`, `ENG MFR MDL`,
  and `YEAR MFR`.
- `ACFTREF.txt`: `CODE`, `MFR`, `MODEL`, `TYPE-ACFT`, `TYPE-ENG`, `AC-CAT`,
  `BUILD-CERT-IND`, `NO-ENG`, `NO-SEATS`, `AC-WEIGHT`, `SPEED`,
  `TC-DATA-SHEET`, and `TC-DATA-HOLDER`.
- `ENGINE.txt`: `CODE`, `MFR`, `MODEL`, `TYPE`, `HORSEPOWER`, and `THRUST`.

The parser scans `MASTER.txt` only for valid N-numbers already present on
listings, valid registration candidates in `extracted_listing_json` for plugin
submissions that have no canonical listing or remain linked to a listing with a
blank registration, plus any operator-supplied `--include-n-number` targets. It
then retains only reference rows reachable from
those matches. This supports the automatic two-pass flow: visual extraction can
persist a candidate on a pending submission, the next FAA import can cover it,
and submission reprocessing can then pass admission. Malformed pending JSON and
missing, foreign, or invalid pending registration candidates are counted but
never become targets. Explicit targets allow the same pre-coverage flow for a
source-proven registration recovered by an operator. Neither source mutates the
listing. The importer computes the archive digest, each exact uncompressed
member digest, the source manifest, the target set, and exact logical-record
digests while discarding registrant fields. `DEREG.txt` is not imported, and an
older release is never a fallback for the admission gate.

Optionally inspect the complete release outside the repository:

```sh
sha256sum /tmp/ReleasableAircraft.zip
unzip -l /tmp/ReleasableAircraft.zip
```

The external digest is useful for an operator comparison only; it is not an
input to the command or trusted as import provenance.

Run the importer without `--apply` first. Dry run is the default and performs
all parsing, schema, target-coverage, and digest checks without writing:

```sh
cargo run --bin aircost-admin -- import-faa-registry \
  --database /absolute/path/to/aircost.sqlite3 \
  --archive /tmp/ReleasableAircraft.zip \
  --dry-run
```

Dry run uses a diagnostic database connection rather than the normal startup
path. SQLite must already exist and is opened read-only without schema
initialization, migrations, WAL creation, or seed writes. PostgreSQL sessions
set `default_transaction_read_only=on` before the first query and likewise do
not initialize or migrate the schema. A dry run therefore diagnoses the exact
installed contract; it cannot create a missing SQLite database or repair an
old one as a side effect.

The importer derives the release date from the shared, validated ZIP member
date for `MASTER.txt`, `ACFTREF.txt`, and `ENGINE.txt`; there is no operator
date override. Review the derived date and the JSON report's separate
`listing_counts` and `pending_submission_counts`, requested and accepted
explicit targets, target count, matched and absent counts, member hashes,
archive hash, manifest hash, and target-set hash. Apply the same validated
archive explicitly:

```sh
cargo run --bin aircost-admin -- import-faa-registry \
  --database /absolute/path/to/aircost.sqlite3 \
  --archive /tmp/ReleasableAircraft.zip \
  --apply
```

For a registration recovered before its listing row can pass FAA admission,
add the flag once per aircraft to both the dry run and the corresponding apply:

```sh
cargo run --bin aircost-admin -- import-faa-registry \
  --database /absolute/path/to/aircost.sqlite3 \
  --archive /tmp/ReleasableAircraft.zip \
  --include-n-number N1925X \
  --dry-run
```

`--include-n-number` is repeatable. Inputs are normalized to canonical N-number
form and deduplicated with each other and the database-derived targets. Every
explicit input must be a valid U.S. N-number; malformed or foreign values abort
the command instead of being ignored. The JSON `explicit_targets.requested`
array preserves the provided values, and `explicit_targets.accepted` shows the
canonical values included in the merged projection.

The apply transaction is atomic. Reimporting the same archive and target set is
idempotent; adding listings can require another target-scoped projection. Each
projection is immutable, and several projections may refer to the same daily
archive. For one curation case, all selected observations must resolve through
projections with the same snapshot date, source URL, archive hash, manifest
hash, and the exact retained-record hash domain. The current immutable domain
is `aircost-faa-master-retained-aircraft-projection-v1`; it participates in the
source-manifest digest and every retained aircraft source-record digest, so a
projection produced under another algorithm cannot alias a current one.

The curation lookup always starts from the newest imported release. "Newest"
means the greatest parser-derived snapshot date and projection ID; the code
does not impose a maximum age or contact the FAA during lookup. Operations must
therefore verify that the derived date matches the official download and
refresh the import on the intended cadence. A target must have a coverage row
in a projection of that exact release. No snapshot, no current-release
coverage, an `absent` result, an ambiguous result, or a serial conflict blocks
every listing-backed workflow. Missing, foreign, and malformed registrations
are also blocked. New and updated listings are rejected before mutation.
Pre-policy rows are not deleted automatically, but they are excluded from
avionics/reference curation, valuation snapshot creation, training, and
comparable serving. The curation report records why an existing observation was
excluded. If no source-exact observation in a cluster passes the FAA gate,
Gemini is not called.

Every new valuation snapshot freezes a versioned FAA admission manifest inside
`selection_policy_json`. For each included listing it records the canonical
N-number, normalized observed serial, FAA projection and release, archive hash,
and exact FAA retained-projection record hash. The record hash uses only the
stored non-PII aircraft fields under the immutable domain
`aircost-faa-master-retained-aircraft-projection-v1`; archive and member
hashes bind the original release bytes without putting discarded registrant or
address fields into a row identifier. That manifest participates in both snapshot
and row hashes. Snapshot creation repeats the exact admission audit immediately
before persistence; loading, model activation, comparable fallback, and serving
reject a pre-manifest snapshot or any identity/provenance mismatch instead of
filtering immutable training rows after the fact. A server-cached model is
rechecked before each estimate, so a newer FAA release cannot leave an invalid
training snapshot silently serving until restart.

N-number normalization is conservative. It uppercases and removes only spaces
and hyphens used as presentation separators. The result must start with `N`,
contain one to five following characters, begin with digits `1` through `9`,
place any letters after all digits, contain at most two letters, and exclude
`I` and `O`. Other punctuation is invalid; a foreign registration is never
mechanically converted into an N-number.

Serial evidence has five explicit grades:

- `raw_exact`: trimmed source strings are equal and the observation is eligible.
- `normalized_only`: ASCII letters and digits match after punctuation/spacing
  removal and case folding; the raw values remain preserved and the observation
  is eligible.
- `not_provided`: the listing has no serial; the current N-number match remains
  eligible and the absence stays visible.
- `registry_unavailable`: the FAA row has no serial; the N-number match remains
  eligible and the absence stays visible.
- `conflict`: both sides supplied different comparison keys; the observation is
  blocked and requires review rather than correction by Gemini.

FAA `YEAR MFR` is stored only as `year_manufactured`. It is never copied to,
compared as authority over, incremented into, or decremented into the listing's
`model_year`. A difference is emitted as an audit fact because manufacturing
year and marketed model year can legitimately differ.

Listing-only valuation is an explicit staged workflow:

```bash
cargo run --bin aircost-admin -- snapshot-valuations --max-age-days 180 --apply
cargo run --bin aircost-admin -- fit-valuation --kind structural --snapshot-id ID --apply
cargo run --bin aircost-admin -- validate-valuation --model-version-id ID
cargo run --bin aircost-admin -- activate-valuation --model-version-id ID
```

Snapshotting and fitting default to dry run. Fitting persists only a candidate;
activation always requires a separate command.

## Valuation Hardening Migration

The evidence/lifecycle changes use explicit backend-specific migrations:

```text
migrations/20260720_valuation_data_hardening.sqlite.sql
migrations/20260720_valuation_data_hardening.postgres.sql
```

Back up the database and apply the matching file during a maintenance window.
The application does not run it automatically. Existing listings are
deliberately quarantined, and legacy price/spec/component value rows are marked
unreviewed and valuation-ineligible; the migration never guesses provenance.
Review or reprocess those rows before changing them to `ready`, then create a
new frozen snapshot and explicitly fit, validate, and activate a candidate.

For SQLite, first check whether the one-time migration has already run:

```sh
sqlite3 -readonly data/aircost.sqlite3 \
  "SELECT EXISTS(SELECT 1 FROM pragma_table_info('aircraft_sale_listings') WHERE name='ingestion_state');"
```

Run the migration only when that query returns `0`, and use fail-fast mode so
the CLI cannot continue after a statement error:

```sh
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260720_valuation_data_hardening.sqlite.sql"
```

The migration tolerates additive suite/fact tables already created by a newer
binary, but it is not rerunnable because SQLite does not support
`ADD COLUMN IF NOT EXISTS` for the remaining one-time column additions.

## Avionics Catalog Migration

The curated-catalog lifecycle is a second explicit migration:

```text
migrations/20260721_avionics_catalog_curation.sqlite.sql
migrations/20260721_avionics_catalog_curation.postgres.sql
```

It preserves every legacy model and association but marks all legacy identities
`unreviewed`. It does not infer identifiers, promote rows, merge labels, or
delete data. New listing, default-avionics, and suite links require approved
identities, and valuation/training reads exclude legacy-unreviewed identities.
Apply it before deploying a binary that expects the catalog columns.

For SQLite, preflight and apply in fail-fast mode:

```sh
sqlite3 -readonly data/aircost.sqlite3 \
  "SELECT EXISTS(SELECT 1 FROM pragma_table_info('avionics_models') WHERE name='catalog_status');"
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260721_avionics_catalog_curation.sqlite.sql"
```

Run the migration only when the preflight query returns `0`. Then use
the automatic `verify-listings` workflow below to classify stored listing
equipment and replace associations safely. The former mechanical
`normalize-avionics` command was removed: typography-only maker/model matches
are review signals and can never authorize catalog rewrites or deletion.

## Avionics Multiple-Type Migration

Product identity and product capability are separated by a third explicit
migration:

```text
migrations/20260721_avionics_multi_type.sqlite.sql
migrations/20260721_avionics_multi_type.postgres.sql
```

The migration creates `avionics_model_types`, backfills every model's legacy
type, and then removes the scalar `avionics_models.avionics_type_id`. The old
composite `NAV/COM` class is decomposed into the atomic `NAV` and `COM`
capabilities; no other additional capability is inferred. Same-name legacy
rows remain unreviewed rather than being merged mechanically; approved catalog
products are unique by manufacturer/name as well as by normalized manufacturer
identifier. Apply the migration after the curated-catalog migration and before
deploying code that reads capability memberships.

For SQLite, run it only when this preflight query returns `0`:

```sh
sqlite3 -readonly data/aircost.sqlite3 \
  "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='avionics_model_types');"
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260721_avionics_multi_type.sqlite.sql"
```

## Listing Pending-Review Migration

The durable listing review state is installed by the matching backend
migration:

```text
migrations/20260724_listing_pending_reviews.sqlite.sql
migrations/20260724_listing_pending_reviews.postgres.sql
```

It extends the listing ingestion-state constraint with `pending_review` and
creates `aircraft_sale_listing_pending_reviews`. It does not populate the
table, classify legacy equipment, change listing-avionics links, or create FAA
cache tables. Apply it after the aircraft-reference and avionics catalog
migrations and before deploying code that can stage review bundles.

Back up SQLite and apply in fail-fast mode:

```sh
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260724_listing_pending_reviews.sqlite.sql"
sqlite3 -readonly data/aircost.sqlite3 \
  "PRAGMA foreign_key_check; SELECT ingestion_state, COUNT(*) FROM aircraft_sale_listings GROUP BY ingestion_state;"
```

Both backend migrations are idempotent. The SQLite version rebuilds
`aircraft_sale_listings` to extend its check constraint, preserves IDs and
listing data, recreates its indexes, and runs `PRAGMA foreign_key_check` before
returning.

## Identity Deduplication Postconditions Migration

Canonical approved-product uniqueness, guarded legacy remaps, and listing-link
postconditions are installed together by the matching backend migration:

```text
migrations/20260725_identity_deduplication_postconditions.sqlite.sql
migrations/20260725_identity_deduplication_postconditions.postgres.sql
```

Apply it after the catalog, aircraft-reference, and pending-review migrations
and before running legacy catalog consolidation. It intentionally leaves
unreviewed collisions on non-ready listings in place. Existing invalid ready
rows are preserved but quarantined and unverified with a repair reason. The
migration never merges products or approves an identity.

For SQLite, back up the database, rehearse on a copy, and use fail-fast mode:

```sh
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260725_identity_deduplication_postconditions.sqlite.sql"
sqlite3 -readonly data/aircost.sqlite3 \
  "PRAGMA foreign_key_check; PRAGMA integrity_check; SELECT COUNT(*) FROM avionics_catalog_consolidation_guard;"
```

The final guard count must be zero outside an active consolidation transaction.
Startup preflight requires the canonical-key, approved-identity, guard, and
validated-authorization objects so an old database reports the exact migration
command rather than failing later with a missing-table error.

`aircost-admin audit-avionics-duplicates` is read-only and reports collisions
by stored keys, current canonical maker/product keys, and exact maker-scoped
stable-identifier kind/normalized-value pairs. Run
`aircost-admin consolidate-legacy-avionics` without `--apply` first. Automatic
application is limited to unreviewed rows where every pair in a component has
the same non-empty manufacturer identifier kind and normalized value. A
mechanically equal maker/model label remains an audit candidate, not destructive
merge evidence. Transitive-only graphs, conflicting or differently namespaced
identifiers, fuzzy-similarity candidates, and same-listing quantity ambiguity
remain blocked for grounded review. Raw manufacturer rows are historical input
and are never reparented or deleted by legacy consolidation; semantic maker
resolution uses only the evidence-backed alias decision and redirect workflow.

## Legacy Listing Review Staging

`stage-listing-reviews` prepares the new review bundles from existing listing
data without rerunning Gemini. It reads the latest retained plugin extraction,
including old scalar `type` fields, together with current listing links and
catalog state. It deterministically identifies unlinked observations,
unapproved legacy products, low-confidence or mismatched links, unsupported
capabilities, and unresolved replacement targets. Approved, high-confidence
legacy links that do not match any usable retained observation are also staged,
including their exact installed and replacement roles, so stale imported links
cannot bypass review. If no usable retained observations exist, those
approved/high links are preserved rather than rejected without evidence.
Associations with `listing_review` provenance are never reopened by this
backfill. Retained extraction evidence is staged only as a complete
evidence/confidence pair. Notes copied from unmatched legacy installed or
replacement links are not occurrence evidence and are never copied into the
new review bundle.

Dry run is the default and is strictly read-only:

```sh
cargo run --bin aircost-admin -- stage-listing-reviews \
  --database /absolute/path/to/aircost.sqlite3 \
  --limit 100

cargo run --bin aircost-admin -- stage-listing-reviews \
  --database /absolute/path/to/aircost.sqlite3 \
  --listing-id 51
```

The JSON report includes per-listing status, reason counts, source issues,
pending aspect counts, and the approved-catalog revision. It also reports zero
Gemini calls, catalog writes, and listing-link writes. After inspecting that
report, opt in to staging:

```sh
cargo run --bin aircost-admin -- stage-listing-reviews \
  --database /absolute/path/to/aircost.sqlite3 \
  --limit 100 \
  --apply
```

Apply mode creates or replaces at most one complete bundle per selected
listing and moves that listing to `pending_review`. It does not delete or
rewrite the legacy catalog or listing links; those records remain available as
explicit context until a reviewer submits a complete decision set. Coverage is
recorded by exact listing-link ID and installed/replacement role; sharing a
catalog product with another link therefore cannot cause that other association
to be removed. A live review not produced by this backfill is never overwritten
or cleared. Re-running apply clears an obsolete backfill-owned bundle only when
that listing no longer has any pending aspects.

For an unlinked retained observation, staging exposes an in-place catalog
candidate only when normalized manufacturer/model selects exactly one catalog
row and that row is `unreviewed`. Duplicate identities, approved rows, and
rejected rows are not implicit promotion targets. A staged aspect that covers
an existing legacy association can expose that exact catalog row by ID without
pretending the row was selected by an unlinked uniqueness search. Neither form
is preapproved: promotion still rechecks normalized-identity uniqueness and
identifier/model collisions under the write lock, in addition to status and
reference coverage. The reviewer receives all three actions. A create decision
with the same identity may promote the surviving explicit candidate; a
corrected identity creates a new row instead of rewriting it.

The stored catalog hash is staging provenance. Review reads return the current
approved-only catalog fingerprint, and resolution recomputes that fingerprint
under the write lock. Edits to legacy `unreviewed` or `rejected` rows therefore
do not create false stale-review conflicts, while a real change to an approved
product's fingerprinted manufacturer, model, capabilities, stable identifier,
or approval membership requires the reviewer to reload. Promoting a covered
legacy product is rejected if it participates in aircraft defaults, reference
configurations, or avionics suites; those global relationships require separate
catalog curation and cannot be adjudicated from one sale listing.

## Automatic Listing Verification

`verify-listings` is the reusable aircraft, avionics, and readiness workflow
used by batch administration and the review API. Its default keyset selects
listings that are not both `ready` and verified; an exact `--listing-id`
remains available for an idempotent retry or inspection. Each listing runs
sequentially so an aircraft or catalog decision made earlier in the listing is
visible to its later stages. A failure is isolated to that listing and does
not abort the remaining page.

Aircraft verification starts with mandatory current FAA admission and the
approved local hierarchy. It assigns a unique approved FAA-backed identity
without Gemini when possible. Only an unresolved admitted identity uses the
configured Gemini and FAA DRS clients; missing DRS configuration leaves that
aircraft pending instead of weakening the admission rule.

The avionics stage runs only when the listing has a pending review. It uses
exactly the `plugin_submission_id` attached to that review; it
does not select a newer submission or silently substitute another same-URL
payload. The listing and submission must have the same owner and the
submission must either be canonically linked to the listing or have the exact
listing source URL without being linked elsewhere. Before any Gemini call, the
workflow verifies the stored review-payload SHA-256 and the retained rendered
HTML against its signed SHA-256.

A retained extraction is replayed only when it has at least one equipment item
and every item uses the current non-empty `types` capability array. Empty or
scalar `type` payloads are not normalized or mechanically converted. Instead,
the tool re-runs the current Gemini listing extractor against the verified
`rendered_html`, then passes that transient current-schema result to grounded
identity resolution. The generated extraction is not written back to the
signed plugin submission. Each listing's Gemini usage is attributed to that
listing and exact plugin submission.

Identity resolution does not resend the whole retained listing for every
candidate. It builds an exact source-only context capped at 4,096 bytes from a
header slice, the candidate neighborhood, and the nearest manufacturer
neighborhood. A model anchor with alphanumeric boundaries is mandatory, source
ranges are merged, and a fixed word-bearing separator prevents synthetic
manufacturer/model adjacency across slices. A missing or prefix-only match
such as `G5` inside `G500` returns no context and therefore fails closed.

The verifier first accepts a unique exact graph-approved, currently attested
local product with compatible capabilities and exact listing evidence. When
that strict path cannot decide, it may make one tools-disabled Gemini request
over a complete bounded manufacturer collision family. The family includes
approved selectable products together with unreviewed, unattested, and
capability-incompatible blockers, and is fingerprinted against the complete
active manufacturer catalog. Only an unchanged approved and currently
attested ID may be selected. An overflow, missing family, blocker selection,
uncertain answer, or concurrent catalog change falls through to ordinary
grounded curation; it never authorizes an association. Search and URL Context
are therefore reserved for observations that the local catalog and bounded
comparison cannot resolve.

Existing-listing aircraft corrections are retained in
`aircraft_listing_identity_correction_decisions`. Each immutable decision
references one immutable `aircraft_identity_observations` row and one validated
`curation_evidence_claims` row. It also binds the listing/capture state hash,
plugin submission and HTML digest, old and corrected identifiers, reviewer,
and—where identifiers change—the exact current FAA snapshot and source-record
digest. Visual decisions additionally retain the complete one-photo resolution
audit. The current listing is updated only in the same transaction after all
guards pass; source-evidence corroboration advances observation history without
rewriting the retained submission. Apply
`migrations/20260819_aircraft_listing_identity_corrections.*.sql` before using
the repair endpoints on an existing database.

The migration is idempotent only for its exact initial version-1 contract. A
preexisting same-name correction table without that contract is rejected
instead of being adopted. Startup independently validates the contract, both
unique indexes and their ordered columns, decision and referenced-observation
immutability triggers, and the receipt-gate definition. PostgreSQL installs
these routines and relations in `public`, fully qualifies every application
relation referenced by the routines, and pins each routine to
`search_path=pg_catalog`. Startup also checks the exact relation and routine
namespaces/OIDs, function configuration, and complete function source.

Correction-referenced identity observations are database-immutable; unrelated
observations remain mutable. A unique submission/kind receipt key prevents a
retry from recording a second decision for the same correction boundary.

The same decision table records clean-replay serial corrections. The imported
submission keeps its raw extraction JSON; only the in-memory materialization
copy uses the current FAA serial. The source N-number and raw serial must both
be exact visible retained-capture spans, and automatic correction is limited to
one internal insertion, deletion, substitution, or adjacent transposition with
the same two-character prefix and suffix. Recording is allowed after binding
only when listing, submission, rendered HTML, extraction payload, FAA snapshot,
and FAA source record all still match.

A corrected signed-source listing is inserted directly into a private
`quarantined` receipt-gate state; it is never committed as an ordinary
`incomplete` row first. Listing insertion and exact-capture binding are one
transaction. Child projection, review attachment, or receipt failures retain
that bound gated pair, never an unbound canonical correction. The database
rejects leaving the gate without the exact bound FAA correction receipt. An
exact submit, reprocess, or replay retry deterministically replaces child
projections, records at most one receipt, and finalizes the same listing.
Exact signed captures are unique by owner, plugin install, source URL, and
rendered-HTML digest. Reprocessing may also replace a prior failed extraction
checkpoint, clear its error, insert the quarantined listing, and bind the two
in one guarded transaction; a crash after that transaction resumes from the
new exact checkpoint.

Correction evidence remains source-scoped. A registration-only photo
observation records no serial or aircraft hierarchy; its decision separately
retains the FAA-derived serial and FAA snapshot binding. An FAA serial
observation records only the FAA registration/serial claim and never invents
make, family, designation, or model year. The obsolete
`aircraft_identity_observations.legacy_hint_json` column remains null and is
not used by the application.

The default mode is a zero-Gemini preflight. Start with one listing or inspect
the first ten pending listings:

```sh
cargo run --bin aircost-admin -- verify-listings \
  --database /absolute/path/to/copy.sqlite3 \
  --listing-id 51

cargo run --bin aircost-admin -- verify-listings \
  --database /absolute/path/to/copy.sqlite3 \
  --limit 10
```

Preflight does not construct a Gemini client, require `GEMINI_API_KEY`, make
provider requests, or write usage/domain data. Its checkpoint contains
`resume_after_listing_id` and `has_more`; pass the checkpoint back as
`--after-listing-id` to advance even when low-ID listings remain pending:

```sh
cargo run --bin aircost-admin -- verify-listings \
  --database /absolute/path/to/copy.sqlite3 \
  --limit 10 \
  --after-listing-id 51
```

`--after-listing-id` and `--listing-id` are mutually exclusive. An exact
listing ID remains available to retry or inspect a residual item separately.

Apply-readiness preflight also checks eligible existing avionics links that are
outside the catalog scope produced by the same identity resolver that will run
the paid candidates. That scope uses bounded selectable catalog IDs and exact
avionics manufacturer identities; it does not infer aircraft-maker aliases
from raw listing labels. Candidate adjudication remains unbounded for this
early gate because an uncertain, invalid, or stale answer falls through to
global triage, which can correct the manufacturer. When that fallback, direct
triage, or unknown-manufacturer grounding can legitimately escape the initial
bounds, preflight does not reject existing links early and leaves the complete
decision to the final transaction. Otherwise, before reporting paid work as
runnable, each unrelated link must have a current manufacturer-reuse
attestation or exact current same-listing authorization, including the current
catalog and collision-closure revisions.
A deterministic unrelated blocker contributes zero requests to the provider
plan. A link inside the resolver-produced scope is not rejected early because
the paid result may legitimately replace or repair it; the final transaction
remains the complete graph and concurrency authority. Paid preview
intentionally skips this readiness gate so it can still inspect prospective
provider behavior without claiming that the result can be applied.

The request plan counts logical provider requests rather than describing a
grounded workflow as one "call." It reports tools-disabled candidate
adjudication separately at one request per eligible identity, including the
maximum grounded fallback if those decisions do not pass their local gates.
Successful local reuse and successful candidate adjudication do not run the
concreteness classifier. Every identity that reaches the grounded route first
uses exactly one tools-disabled classifier request. A strict `very_high`
generic result can stop there; every other valid, invalid, ambiguous, or failed
classification continues normally. The fresh grounded portion then has three
baseline requests: Search, URL Context, and structure. Per-stage
model-validation fallback raises that portion to at most six requests; one
reused-evidence identity correction makes that portion's envelope eight. A
positive identity also requires an independent collision pass. Its review and
optional domain correction share a two-structure-call budget, for seven
baseline requests and a fifteen-request complete validation envelope including
the classifier. The report separates its known
minimum baseline (candidate comparison succeeds and conditional relationship
targets are skipped), all-positive baseline, and maximum validation envelope
(every candidate falls through to classifier plus grounding). A legacy
listing re-extraction is one baseline request and up to two with JSON repair.
Structurally valid current-schema observations whose complete normalized model
is in the closed generic-category vocabulary contribute zero requests to every
plan total and are discarded deterministically. Structurally malformed
observations stay in review without a provider request. Whole-label equality is
required; similarity and partial overlap do not authorize discard.
Transport retry attempts are reported separately and are not multiplied into
these logical counts. Identity counts produced by legacy re-extraction, later
verified-local reuse, fallback, and correction outcomes remain explicitly
unknown; the tool does not infer a dollar estimate. The identity envelope does
not include data-dependent finalization enrichment for aircraft specs,
installed-product values, or model-year reference configurations. The report
states that exclusion explicitly, and every request actually made by
finalization is still written to `gemini_api_usage`.

Use `--preview` to explicitly enable a paid preview. Preview
writes no catalog, listing, review, or plugin domain data; normal Gemini usage
accounting still applies. In apply mode, obsolete avionics in a plugin
extraction are a separate durable first phase. The returned raw occurrence
array must provide explicit quantity, action, and replacement semantics, pass
the current capability schema, and bind every evidence excerpt to the retained
source. The workflow then replaces only the `avionics` member of the retained
top-level extraction object; aircraft identity, hours, price, valuation facts,
and every other non-avionics value remain unchanged. A missing, invalid, or
non-object prior extraction fails closed rather than accepting unvalidated
whole-listing values from this avionics pass. One optimistic transaction binds
the compact merged JSON to the exact submission, owner, listing, source URL,
rendered HTML bytes and SHA-256, pending-review revision, canonical binding,
and prior extraction/error state. PostgreSQL writers first take the shared
listing-child table lock order, then lock and revalidate the exact listing,
pending-review, and submission rows before updating. A concurrent change fails
closed, while an identical repeated write is idempotent. This phase stores no
prompt, response envelope, Search result, URL Context dossier, or grounding
evidence. Because it commits before identity work, a later catalog or listing
block retains the validated avionics and a retry can preflight and replay them
without another listing-extraction request.
Set `GEMINI_API_KEY` and review the per-listing
source, re-extraction, error, accepted, safely-discarded, and remaining-review
counts before using `--apply`. Apply mode persists independently grounded
catalog identities through the ordinary catalog resolver; listing links and
review state are handled separately by the atomic apply boundary below.
`prepared_link_count` is diagnostic: it counts links assembled from resolved
observations for the attempted atomic request, not every existing database
link inspected by the readiness gate. `accepted` counts only links committed
by a successful atomic apply. A blocked attempt or final transaction rejection
therefore reports zero accepted links even when its prepared-link count is
nonzero.

Candidate outcomes are partial rather than all-or-nothing:

- A grounded approved product becomes a listing link only when the occurrence
  itself has exact `high` source confidence.
- An approved product with weaker occurrence evidence remains a review aspect
  with the verified catalog product as its suggestion.
- An unresolved identity, provider error, or input error remains a review
  aspect.
- A safely grounded rejection of an installed or subject observation is
  discarded and does not enter either the catalog links or the residual
  review.

A `replaces` or `removes` subject and target are one dependency. Both must
resolve with high listing evidence before the relationship is accepted. A
rejected subject safely discards the complete unit; an unresolved or rejected
target leaves both subject and target for review. Multiple capability rows such
as GPS and transponder labels for one GNX 375 are coalesced only after they
resolve to the same stable product identity. Quantities use the maximum rather
than a sum, and the complete accepted action graph is validated before apply.

The avionics apply boundary writes the accepted listing links and residual review bundle through one
transaction. That transaction revalidates the review and HTML hashes,
submission ownership/binding, current FAA snapshot and source record, and
approved catalog graph identities. Unrelated existing verified,
high-confidence links are preserved. A stale source, FAA change, invalid
accepted graph, conflicting duplicate, or database failure leaves the prior
links and pending review intact. If residual aspects remain, the listing stays
`pending_review`; an all-pass result returns it to `incomplete`. The enclosing
automatic verifier then finalizes only when the FAA-backed aircraft identity
is assigned and no review aspects remain. A successful finalizer makes the
listing `ready` and verified. Factory-reference resolution remains a separate
valuation concern: missing prerequisites are returned as typed gaps without
changing listing state or replaying a stored Gemini dossier. Residual listing
review remains `pending_review`, while actual
listing, FAA, or listing-specific finalization failures follow the quarantine
path.

The workflow also fails closed before apply when retained HTML or its source
URL is missing, the HTML hash is invalid, the HTML cleans to no usable text,
the listing lacks current FAA admission, or a re-extraction returns no usable
equipment observations. An empty equipment array is not durably stored and is
never treated as evidence that all prior observations are garbage. The
workflow never writes dollar
metadata; avionics acceptance alone does not make a listing valuation-grade.

## Durable Listing Verification Runs

The web verifier coordinates automatic work through
`listing_verification_runs` and `listing_verification_run_items`. These tables
contain only operational state: the authenticated owner, idempotency
fingerprint, ordered listing IDs, status, attempts, leases, sanitized terminal
outcomes, and reviewer-facing failure codes. Gemini prompts, source documents,
grounding dossiers, provider responses, and raw errors are never stored here.
Provider accounting remains exclusively in `gemini_api_usage`.

One owner/idempotency key identifies one ordered request. Reusing it with the
same request returns the existing run; changing the request is a conflict. A
partial unique index prevents one listing from being queued or running in two
runs, and a second partial index permits only one running item per run. Expired
leases are requeued with an incremented attempt count. Cancellation changes
queued items to `cancelled`; an already-running item finishes under its current
lease while the run is `cancelling`, after which the run becomes `cancelled`.

Fresh databases receive the tables from the canonical schema. Existing
databases use the matching additive, idempotent migration:

```text
migrations/20260809_listing_verification_runs.sqlite.sql
migrations/20260809_listing_verification_runs.postgres.sql
```

For SQLite, apply the migration in fail-fast mode and verify its contract and
integrity:

```sh
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260809_listing_verification_runs.sqlite.sql"
sqlite3 -readonly data/aircost.sqlite3 \
  "SELECT contract_version, contract_fingerprint
     FROM schema_migration_contracts
    WHERE migration_name = '20260809_listing_verification_runs';
   PRAGMA foreign_key_check;
   PRAGMA integrity_check;"
```

The migration does not create a run, alter a listing, call a provider, or add
usage rows.

## Durable Listing Replay Runs

Fresh databases receive the manifest replay ledger from the canonical schema.
Existing databases must apply the matching additive migration before starting
the new binary:

```text
migrations/20260819_listing_replay_runs.sqlite.sql
migrations/20260819_listing_replay_runs.postgres.sql
```

The migration refuses a mismatched contract and refuses partially pre-existing
replay objects without the exact installed contract. A marker-present rerun
attests the complete canonical tables, constraints, foreign-key targets and
actions, indexes, and absence of unexpected attached behavior before any replay
DDL. It never updates the existing marker's `installed_at`. It is safe to apply
a second time only after the exact contract and complete objects exist. For
SQLite, back up the database, run it in fail-fast mode, then check the contract,
foreign keys, and integrity:

```sh
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260819_listing_replay_runs.sqlite.sql"
sqlite3 -readonly data/aircost.sqlite3 \
  "SELECT contract_version, contract_fingerprint
     FROM schema_migration_contracts
    WHERE migration_name = '20260819_listing_replay_runs';
   PRAGMA foreign_key_check;
   PRAGMA integrity_check;"
```

The migration creates no replay run, listing, provider call, or copied capture
payload.

## Aircraft Reference Catalog And FAA Projection Migration

The clean aircraft hierarchy, evidence workflow, immutable reference profiles,
and FAA registry projection are installed together by this additive migration:

```text
migrations/20260722_aircraft_reference_catalog.sqlite.sql
migrations/20260722_aircraft_reference_catalog.postgres.sql
```

It does not delete or rewrite listings, and it deliberately does not copy or
approve legacy manufacturers, models, variants, specs, price points, or default
avionics. Existing listings therefore do not need to be re-added. The FAA
tables start empty and must be populated with `import-faa-registry` after the
migration. Existing databases must be migrated before starting a binary that
expects the clean catalog; fresh databases receive the same schema directly.

After the base catalog migration, install the FAA projection reachability and
record-hash-domain contracts in order:

```text
migrations/20260819_faa_reference_reachability.postgres.sql
migrations/20260820_faa_record_hash_domain.postgres.sql
migrations/20260820_faa_record_hash_domain.sqlite.sql
```

PostgreSQL databases apply both PostgreSQL files in that order. SQLite applies
the SQLite record-domain file; the reachability contract is already enforced
by the SQLite base objects. Each migration accepts only its exact predecessor
shape and preserves the original `installed_at` on an exact rerun. A missing,
nonempty legacy FAA projection is deliberately rejected: delete only those
derived FAA projection rows and regenerate them by importing the exact retained
FAA ZIP. Do not add the domain with ad hoc SQL, mechanically rehash rows, or
relabel a legacy projection. The archive bytes and domain are both inputs to
the authoritative hashes, so only importer regeneration establishes the new
identity.

Back up the database and test the matching migration on a copy. For SQLite,
representative clean-catalog and FAA tables should all be absent before the
one-time migration:

```sh
sqlite3 -readonly data/aircost.sqlite3 \
  "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('curation_evidence_sources','aircraft_makes','aircraft_reference_configuration_versions','faa_registry_snapshots','faa_registry_aircraft','faa_registry_aircraft_references','faa_registry_engine_references','faa_registry_coverage');"
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260722_aircraft_reference_catalog.sqlite.sql"
```

Run the migration only when the first query returns `0`. A partial count is an
inconsistent schema and must be investigated instead of rerunning blindly.
Afterward, the count should be `8` and `PRAGMA foreign_key_check` should return
no rows. For Postgres, apply the Postgres file with the client's stop-on-error
option during the same maintenance workflow.

Snapshot and projection rows are append-only. Database constraints require an
exact `regulator_primary` evidence source whose official FAA URL and content
digest match the recorded archive, require reference rows to be reachable from
a retained target, require coverage to agree with the retained MASTER row, and
reject updates or deletes. Corrections arrive as a new release/projection, not
as mutations to prior evidence.

An earlier draft of this migration also created
`aircraft_curation_interaction_runs` and nullable references from decisions and
profile proposals. No runtime path reads or writes those fields: request
accounting belongs in `gemini_api_usage`, while approved source facts belong in
`curation_evidence_sources` and `curation_evidence_claims`. Databases that
received that draft should apply the matching idempotent cleanup migration:

```text
migrations/20260727_remove_unused_aircraft_curation_runs.sqlite.sql
migrations/20260727_remove_unused_aircraft_curation_runs.postgres.sql
```

For SQLite, back up the database and run:

```sh
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260727_remove_unused_aircraft_curation_runs.sqlite.sql" \
  "PRAGMA foreign_key_check;"
```

The cleanup preserves decisions, proposals, validated evidence, and usage
accounting. It removes only the unused request/response dossier table and its
two unused foreign-key columns, and is safe to run more than once.

The optional-dimension decision semantics are upgraded independently by:

```text
migrations/20260728_aircraft_identity_no_supported_selection.sqlite.sql
migrations/20260728_aircraft_identity_no_supported_selection.postgres.sql
```

This migration removes the overloaded `not_an_entity` action and admits the
approved, evidence-free `no_supported_selection` outcome for newly validated
generation and package decisions only. Canonical historical `not_an_entity`
rows remain generic `reject`/`rejected` decisions because they were not
validated under the new token, grounding, and catalog-relationship predicates;
they are never retroactively approved. The migration fails closed on malformed
legacy combinations and is safe to rerun. Rehearse it on a backup before
applying it to a stopped writer:

```sh
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260728_aircraft_identity_no_supported_selection.sqlite.sql" \
  "PRAGMA foreign_key_check;" \
  "PRAGMA integrity_check;"
```

For PostgreSQL, use:

```sh
psql -v ON_ERROR_STOP=1 "$DATABASE_URL" \
  -f migrations/20260728_aircraft_identity_no_supported_selection.postgres.sql
```

## Aircraft Catalog Retrieval-Key Repair

Canonical make, family, generation, and factory-package rows use a mechanical
retrieval key: preserve ASCII letters and digits, lowercase ASCII letters,
replace every other code point with a separator, then collapse and trim the
separators. These keys deliberately do not apply manufacturer aliases or remove
legal suffixes. For example, `TEXTRON AVIATION INC` is stored as
`textron aviation inc`; `Cessna` remains a separately approved alias.

Older catalog rows may have been written with the manufacturer-aware legacy
normalizer, which could store `cessna` as the key for the canonical display
name `TEXTRON AVIATION INC`. Repair those rows with:

```text
migrations/20260729_aircraft_catalog_retrieval_keys.sqlite.sql
migrations/20260729_aircraft_catalog_retrieval_keys.postgres.sql
```

The migration derives keys for all existing makes, families, generations, and
factory packages. It fails before changing data if a derived key is empty,
collides within its catalog scope, or would overlap another make's approved
alias. It does not merge or delete rows and preserves catalog IDs, approval
decisions, assignments, projections, and aliases. Only the update side of the
assigned/projected immutability barrier is suspended inside the repair
transaction; delete protection remains active and the full barriers are
restored before commit. The migration is safe to rerun.

Stop writers, make a backup, and apply the SQLite repair with:

```sh
backup_path="data/aircost.pre-aircraft-catalog-retrieval-keys-$(date +%Y%m%d%H%M%S).sqlite3"
cp --reflink=auto data/aircost.sqlite3 "$backup_path"
sqlite3 -bail data/aircost.sqlite3 \
  "PRAGMA foreign_keys=ON;" \
  ".read migrations/20260729_aircraft_catalog_retrieval_keys.sqlite.sql" \
  "SELECT id, name, normalized_name FROM aircraft_makes ORDER BY id;" \
  "PRAGMA foreign_key_check;" \
  "PRAGMA integrity_check;"
```

For PostgreSQL, use:

```sh
psql -v ON_ERROR_STOP=1 "$DATABASE_URL" \
  -f migrations/20260729_aircraft_catalog_retrieval_keys.postgres.sql
```

## Listing Aircraft Identity Assignment Migration

Listings cross the publication and valuation boundary through a durable
curated identity assignment installed by:

```text
migrations/20260725_listing_aircraft_identity.sqlite.sql
migrations/20260725_listing_aircraft_identity.postgres.sql
```

The SQLite migration uses one `BEGIN IMMEDIATE` transaction; the Postgres
migration is transactional as well. It creates a release-scoped FAA aircraft
code-to-designation binding, append-only listing assignment versions, and a
single current pointer. Assignment insertion requires an approved curated
make/family/designation, validated regulator evidence, the exact cited FAA
projection/record/code, and compatible generation and trim-tier dimensions.
Changing identity appends a successor and advances the pointer; assigned
hierarchy rows, applicability links, evidence bindings, and assignment history
cannot be edited in place.

The designation binding also proves that the designation's owning make matches
the FAA manufacturer. A typographically exact make label is sufficient;
otherwise the catalog must contain one approved, deterministic make alias that
is applicable to the United States and to the aircraft/listing model year.
Aliases for another market or year are ignored. Overlapping market/year aliases
cannot map the same normalized FAA label to different makes, and approved make
aliases are immutable. Both runtime admission and the database `ready` gate
repeat this check. The ready-update trigger includes `model_year`, so changing a
year cannot retain a stale make alias, generation, or factory-package scope.

Apply the matching file after backing up and rehearsing on a copy. Existing
listings are retained. A pre-migration `ready` row without a valid current
assignment is unverified and quarantined with a precise migration reason; it
is never grandfathered or deleted. Direct SQL also cannot insert or restore a
`ready` row without the current assignment. Because the live curated hierarchy
may be empty, the migration deliberately does not fabricate assignments from
legacy labels. Such rows remain pending curation until an exact existing
family/designation can be selected and evidence-backed.

FAA snapshots are target-scoped projections. Currentness therefore compares
the release identity—snapshot date plus archive SHA-256—not one projection ID.
An assignment continues to admit when another projection of the same release
covers its N-number and yields the same aircraft code and source-record hash.
Importing a genuinely newer/different release atomically quarantines and
unverifies ready listings whose assignment still cites the prior release.
After re-grounding, a successor assignment can restore readiness. The legacy
`aircraft_model_variant_id` remains only a valuation compatibility projection;
its display labels are not FAA or publication identity evidence.

## Standalone Historical Migration Contract Provenance

When a backend-specific historical migration file is run directly, its row in
`schema_migration_contracts` is an installation receipt, not a mutable "latest
version" record. A strict migration accepts either an absent receipt or its
exact version and fingerprint. The first installation inserts the receipt; an
exact standalone rerun leaves `installed_at` unchanged; a different version or
fingerprint—including a null value exposed by a weakened receipt table—aborts
before subsequent domain statements execute. Operators must investigate a
mismatch instead of rerunning a migration to heal the marker.

Each of the 27 receipt-bearing PostgreSQL historical migration files pins its
transaction-local search path to `public`, `pg_catalog`, and an explicitly last
`pg_temp`, then holds a transaction-wide `SHARE ROW EXCLUSIVE` lock on
`public.schema_migration_contracts` from before its guard through the final
receipt insert. Explicitly placing `pg_temp` last prevents PostgreSQL's implicit
temporary-relation precedence from replacing the canonical ledger or domain
objects; shadows on the caller's search path are likewise ignored. The caller's
search path is restored at commit. A ledger writer that started first must
commit before the guard reads the receipt, while a later writer waits until the
migration commits. The guard and domain statements therefore cannot observe
different committed receipt states during one standalone rerun. Each of the 26
receipt-bearing SQLite migration files obtains the corresponding write
serialization from `BEGIN IMMEDIATE`.

The only historical in-place receipt upgrades are the documented version-1 to
version-2 transitions in the default-avionics quarantine and avionics product
reuse-attestation migrations. They update `installed_at` only while moving the
exact predecessor fingerprint to the exact version-2 fingerprint. An exact
version-2 rerun is a no-op, and any other predecessor is rejected.

Canonical schema application during process startup is a separate provenance
contract. Before executing canonical DDL, startup classifies every active
migration as `Fresh`, `Installed`, or `Invalid`: only joint absence of its
anchor and receipt is fresh, while an installed migration requires both its
anchor and the exact receipt version and fingerprint. Every partial pairing,
mismatch, or null marker is invalid. An existing receipt ledger must also match
the canonical backend definition, including its table kind, columns, types,
nullability, collation, timestamp default, primary key, check definitions and
constraint flags. PostgreSQL additionally requires a permanent, non-inherited
ordinary table and exactly one canonical permanent btree primary-key index,
including its key, collation, operator class, options, and validity flags.
Receipt reads use `ONLY public.schema_migration_contracts`, so a child table
cannot supply a missing parent receipt. Invalid provenance is also any attached
behavior: SQLite forbids ledger triggers and explicit indexes, while PostgreSQL
forbids extra indexes, user triggers, rewrite rules, row-level security and
policies, partition attachment, identity columns, and generated columns.

Startup performs both complete provenance gates, every canonical DDL statement,
and the developer seed on one real SQLx transaction connection. SQLite starts
with `BEGIN IMMEDIATE`. PostgreSQL first takes the process-wide session advisory
lock, determines whether the qualified public ledger exists, then starts a
repeatable-read transaction; an existing ledger is locked in `SHARE ROW
EXCLUSIVE` mode before the first transaction snapshot read. The transaction
pins its local search path, runs the full preflight, applies the schema, and runs
the full postflight before commit. PostgreSQL explicitly releases and verifies
the session lock, and discards the connection on every path so a failed unlock
cannot leak lock ownership into the pool. A failed preflight or late DDL/seed/
postflight error rolls back all startup changes. Canonical receipt seeds are
insert-only, so normal startup preserves every original `installed_at` value;
unknown historical receipts are allowed and preserved rather than rewritten.
Every normal and diagnostic PostgreSQL pool connection pins `search_path` to
`public, pg_catalog, pg_temp` (with `pg_temp` explicitly last), so URL or role
defaults cannot redirect preflight lookups or canonical DDL into
attacker-controlled schemas.

## Listing Aircraft Compatibility Projection Migration

The compatibility bridge from curated aircraft identity to the valuation
schema is installed by:

```text
migrations/20260726_listing_aircraft_compatibility_projection.sqlite.sql
migrations/20260726_listing_aircraft_compatibility_projection.postgres.sql
```

Apply the matching file only after the `20260725_listing_aircraft_identity`
identity v2 migration. Unresolved new listings use one immutable, schema-owned
placeholder hierarchy (`-1/-1/-1`). Parsed or manually entered labels are
instead retained append-only in
`aircraft_listing_identity_input_observations`. Those observations are
non-authoritative retrieval history, survive parent-listing deletion with a
null listing reference, and cannot satisfy an evidence or identity gate.

`aircraft_valuation_compatibility_projections` is the only canonical bridge.
Each immutable row maps one deterministic valuation variant to one positive
make/family/designation/generation/package tuple and copies the controlling
decision, evidence claim, FAA snapshot, N-number, and FAA source-record digest
from the assignment that created it. Generation and package may be null, but
the tuple is still null-safely unique. A later listing with the same exact
tuple reuses the existing projection; labels from a listing never select or
create one.

An insert into `aircraft_valuation_projection_transitions` is an ephemeral
command with kind `initial`, `current_repair`, or `successor`. In one atomic
operation it validates the current FAA-backed assignment, creates or reuses
the collision-free reserved valuation hierarchy, repoints the listing,
advances the current assignment when required, and deletes itself. A failed
sub-step rolls back the whole command. PostgreSQL also locks the relevant
catalog and listing rows while checking reserved-key collisions. Committed
databases must therefore contain no transition rows.

The exact join is exposed as
`aircraft_sale_listing_exact_compatibility_projections`. Both insertion as
`ready` and transition to `ready` require that exact projection, and the
placeholder can never become ready. Migration of an older database quarantines
and unverifies formerly ready listings that lack it; it does not delete them.
Valuation snapshot creation applies the same gate and freezes the projected
variant and five-part tuple alongside the FAA admission evidence. A successor
assignment or projection mismatch invalidates that snapshot instead of
silently changing its aircraft identity.

The migration deliberately performs no mechanical legacy adoption, merge, or
label-based backfill. Existing unreviewed duplicate manufacturer, model, or
variant rows can therefore remain as historical rows outside the projected
path unless a later evidence-backed operation explicitly consolidates or
deletes them. They remain pending or quarantined and cannot become ready
listings or valuation inputs merely because their text looks similar.

Stop writers and back up the target before applying either backend migration.
For SQLite, rehearse the exact order on a disposable copy:

```sh
rehearsal_db="$(mktemp /tmp/aircost-compatibility.XXXXXX.sqlite3)"
cp data/aircost.sqlite3 "$rehearsal_db"
sqlite3 -bail "$rehearsal_db" \
  ".read migrations/20260725_listing_aircraft_identity.sqlite.sql" \
  ".read migrations/20260726_listing_aircraft_compatibility_projection.sqlite.sql" \
  "PRAGMA foreign_key_check;" \
  "PRAGMA integrity_check;"
rm -f "$rehearsal_db"
```

After a successful rehearsal, apply the same two files to the backed-up live
database:

```sh
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260725_listing_aircraft_identity.sqlite.sql" \
  ".read migrations/20260726_listing_aircraft_compatibility_projection.sqlite.sql"
```

For PostgreSQL, restore a production backup into a disposable rehearsal
database and run:

```sh
psql -v ON_ERROR_STOP=1 "$REHEARSAL_DATABASE_URL" \
  -f migrations/20260725_listing_aircraft_identity.postgres.sql \
  -f migrations/20260726_listing_aircraft_compatibility_projection.postgres.sql
```

Run the same fail-fast command against the stopped, backed-up live PostgreSQL
database only after the rehearsal passes:

```sh
psql -v ON_ERROR_STOP=1 "$DATABASE_URL" \
  -f migrations/20260725_listing_aircraft_identity.postgres.sql \
  -f migrations/20260726_listing_aircraft_compatibility_projection.postgres.sql
```

Verify either backend with:

```sql
SELECT migration_name, contract_version, contract_fingerprint
FROM schema_migration_contracts
WHERE migration_name IN (
  '20260725_listing_aircraft_identity',
  '20260726_listing_aircraft_compatibility_projection'
)
ORDER BY migration_name;

SELECT singleton_id, aircraft_manufacturer_id, aircraft_model_id,
       aircraft_model_variant_id
FROM aircraft_sale_listing_pending_compatibility_placeholder;

SELECT count(*) AS unfinished_projection_commands
FROM aircraft_valuation_projection_transitions;

SELECT count(*) AS ready_without_exact_projection
FROM aircraft_sale_listings listing
WHERE listing.ingestion_state = 'ready'
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_exact_compatibility_projections exact_projection
    WHERE exact_projection.listing_id = listing.id
  );

SELECT aircraft_make_id, aircraft_model_family_id, aircraft_designation_id,
       coalesce(aircraft_generation_id, 0) AS generation_id,
       coalesce(aircraft_factory_package_id, 0) AS package_id,
       count(*) AS duplicate_count
FROM aircraft_valuation_compatibility_projections
GROUP BY aircraft_make_id, aircraft_model_family_id, aircraft_designation_id,
         coalesce(aircraft_generation_id, 0),
         coalesce(aircraft_factory_package_id, 0)
HAVING count(*) > 1;
```

The contract query must return both migrations at version 2. The placeholder
query must return exactly `1/-1/-1/-1`; both count queries must return zero;
the duplicate query must return no rows. SQLite must additionally return no
rows from `PRAGMA foreign_key_check` and `ok` from `PRAGMA integrity_check`.

## Reference Catalog Publication Cutover

Fresh databases include the strict reference publication contract. Upgrade an
existing database with the backend-specific migration:

```text
migrations/20260819_reference_catalog_cutover.sqlite.sql
migrations/20260819_reference_catalog_cutover.postgres.sql
```

The one-time migration runs inside one `BEGIN IMMEDIATE` SQLite transaction or
one PostgreSQL transaction. It adds the price configuration-basis discriminator
and the four fact-set completeness attestations, then replaces the publication
gates. It permanently drops the old specification, variant-price, default-avionics,
airframe-depreciation, fit-metadata, and component-depreciation tables; none of
their rows are copied into the immutable catalog. Any error, including a late
contract-write failure, restores all seven legacy tables and their old triggers.
Bounded applicability created outside the final universal serial-key contract
fails the preflight before destructive work. Rehearse the SQLite migration on a
consistent disposable backup:

```sh
rehearsal_db="$(mktemp /tmp/aircost-reference-cutover.XXXXXX.sqlite3)"
sqlite3 data/aircost.sqlite3 ".backup '$rehearsal_db'"
sqlite3 -bail "$rehearsal_db" \
  ".read migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "PRAGMA foreign_key_check;" \
  "PRAGMA integrity_check;"
rm -f "$rehearsal_db"
```

For PostgreSQL, restore a backup into a rehearsal database and run:

```sh
psql -v ON_ERROR_STOP=1 "$REHEARSAL_DATABASE_URL" \
  -f migrations/20260819_reference_catalog_cutover.postgres.sql
```

After the migration, grounded research and adjudication hand off a normalized
JSON draft containing only approved decision IDs, validated evidence-claim
IDs, catalog IDs, applicability, and normalized facts. Preview the exact
database assembly/publication transaction (it always rolls back) with:

```sh
cargo run --bin aircost-admin -- \
  publish-aircraft-reference --draft normalized-reference.json
```

Add `--apply` to atomically create or reuse the reference configuration, insert
the building version and facts, and publish it. Provider prompts, responses,
Search transcripts, and complete URL-context dossiers are not accepted by this
boundary and are not stored.

The draft price fields are `direct_cited_amount_usd` and
`direct_cited_nominal_dollar_year`; they represent the primary source's nominal
MSRP, not an inflation-adjusted value. `official_dollar_normalization_facts`
stores only an immutable source year, target year, official index series,
source/target index values, their checked factor, and a validated
regulator-primary evidence-claim ID. The normalized draft can publish that
fact transactionally; no prompt, response, search transcript, or URL dossier
is retained. Serving and snapshots consume the exact factor. A missing pair
produces `reference_price_dollar_normalization_missing` and no estimate.

Verify the installed contract with:

```sql
SELECT contract_version, contract_fingerprint
FROM schema_migration_contracts
WHERE migration_name = '20260819_reference_catalog_cutover';

SELECT publication_state, count(*)
FROM aircraft_reference_configuration_versions
GROUP BY publication_state;

SELECT name
FROM sqlite_schema
WHERE type = 'table'
  AND name IN (
    'aircraft_model_spec_versions',
    'aircraft_model_variant_price_points',
    'aircraft_model_variant_default_avionics',
    'aircraft_model_variant_default_avionics_candidates',
    'depreciation_profiles',
    'depreciation_profile_fit_metadata',
    'component_depreciation_profiles'
  );
```

The contract must be version `1` with fingerprint
`fe31ca0eaae57cfc4ba5c824679bd950fcb98e20d6dd3e686a477fd22d05aab5`.
The fingerprint is the SHA-256 of this newline-terminated manifest:

```text
20260819_reference_catalog_cutover:v1
sqlite-old:238:a2e2d5d3fdbc38847b9bddcebbf587c50447b3415ba3c7f1c3ed8a0b94605b45
sqlite-post:213:82cac0c7a143383a589aaf58699690392f111c7e5daa329ec6f6b385e64590d1
postgres-old:925:379464a027df1c61f99c754b28ff4738
postgres-post:793:5bea7b82d356e161fe8a160f68845c68
```

Its `installed_at` value records the first successful installation and remains
unchanged across schema reruns and application startups. A marker mismatch or
marker-present damaged cutover contract fails before canonical DDL can heal it.
Listing reference resolution uses only one complete published version matching
the current exact FAA identity, model year, `US`/`GLOBAL` market, and FAA serial
scope. Missing and ambiguous matches remain ineligible for snapshots, training,
and serving. The final query must return no rows.

## Avionics Generic Feature-Label Migration

Fresh databases reject the closed feature-only avionics vocabulary at both the
application and database boundaries. Upgrade an existing database with the
matching backend migration before starting the new binary:

```text
migrations/20260824_avionics_generic_feature_labels.sqlite.sql
migrations/20260824_avionics_generic_feature_labels.postgres.sql
```

The migration replaces the approved-model trigger/function, audits every
approved avionics row through that invariant, and records an immutable contract
receipt. The audit is a side-effect-free predicate check: it does not issue
model updates, rewrite, demote, or delete a row, and therefore does not
invalidate listing authorization proofs for otherwise valid products. If a
label such as `Synthetic Vision`, Garmin's feature-only `SVT` shorthand,
`SafeTaxi`, `FliteCharts`, or generic `ADS-B In/Out` is already approved,
explicitly correct or demote that row and rerun the migration. Concrete labels
that merely include a feature annotation, such as
`GTX 345 ADS-B In/Out`, remain admissible because the policy requires exact
whole-label equality.

```sh
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260824_avionics_generic_feature_labels.sqlite.sql" \
  "PRAGMA foreign_key_check;" \
  "PRAGMA integrity_check;"
```

## Gemini Usage Accounting Migration

Fresh databases receive `gemini_api_usage` from `schema/sqlite.sql` or
`schema/postgres.sql`. Existing databases need the matching additive migration
before any Gemini-enabled workflow or an executed benchmark can record usage:

```text
migrations/20260723_gemini_usage_accounting.sqlite.sql
migrations/20260723_gemini_usage_accounting.postgres.sql
```

The migration is idempotent and creates only the accounting table and its
indexes; it does not alter listing, plugin, curation, catalog, or valuation
data. Back up the database first. For SQLite, inspect the target and apply in
fail-fast mode:

```sh
sqlite3 -readonly data/aircost.sqlite3 \
  "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='gemini_api_usage');"
sqlite3 -bail data/aircost.sqlite3 \
  ".read migrations/20260723_gemini_usage_accounting.sqlite.sql"
```

For Postgres, apply the Postgres file with the client's stop-on-error option.
The schema requires the estimated cost and pricing snapshot to be either both
present or both null. If the provider omits any counter required for pricing,
both remain null so unknown cost is distinguishable from a real zero-cost
request.

## Schema Design Rules

`aircraft_sale_listing_avionics_dispositions` is the immutable terminal receipt
table for current retained avionics occurrences. Its stable coordinate is the
exact extraction hash plus occurrence array index and primary/replacement role.
It stores only a verified product link or a bounded discard decision; unresolved
observations remain solely in the pending-review bundle. Existing databases
must apply the matching
`20260819_listing_avionics_dispositions.{sqlite,postgres}.sql` migration before
starting a binary that writes these receipts.

- Prefer non-null columns only for facts actually required and known at write
  time. Preserve unavailable observations as null; never turn an unknown
  component time into zero.
- Do not embed migrations in Rust runtime code. During active development it is
  acceptable to update schemas and reset local data.
- Avoid obsolete compatibility fields. If a field is no longer used, remove it
  from the schema and write path.
- Do not store canonical/non-canonical duplicates unless both are needed by an
  active query path.
- Treat sale listings and rental offerings as roots; generated lookup records
  should be removable when no root references them.
