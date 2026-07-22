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

`aircraft_reference_configurations`,
`aircraft_reference_configuration_versions`, and reference applicability,
price, avionics, engine, propeller, and feature tables

Store immutable, reviewed factory-reference configurations with explicit
model-year, serial, and market applicability. A correction creates a successor
version; it does not rewrite a published version. Legacy model specs, price
points, and default-avionics rows are not promoted into this catalog by the
migration.

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

`aircraft_sale_listings`

Stores canonical sale listing facts: model variant, source URL, model year,
asking price, currency, status, registration, serial number, and airframe,
engine, and propeller hours. `ingestion_state` keeps incomplete or quarantined
rows out of serving and training. Component times are nullable and carry an
explicit basis, evidence text, and confidence; a missing time is not converted
to zero or copied from the airframe. High-confidence installed engine and
propeller identities are linked separately from the factory configuration.

`aircraft_sale_listing_facts`

Stores source-backed condition, restoration, damage/log, and material
conversion facts that explain value without redefining the factory variant.

`plugin_installs`, `plugin_submissions`

Store Chrome extension registrations and submitted rendered HTML. Submissions
retain the HTML, extraction result or error, and the canonical listing created
from the submission when extraction succeeds.

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

`aircraft_model_spec_versions`

Stores variant-level operating and component metadata. This includes fuel burn,
oil assumptions, linked engine and propeller models, component counts, TBOs,
overhaul costs, component baseline-life fractions, annual inspection, variable
maintenance, source URL, and the depreciation profile assigned to the variant.
Only authoritative, high-confidence `factory_default` rows can be marked
valuation-eligible. A component configuration seen on one sale listing remains
listing-specific and cannot seed this shared metadata.

`aircraft_model_variant_price_points`

Stores nominal new-price points for a variant/model year. These are used as the
airframe basis for valuations after subtracting default avionics.
Evidence kind and eligibility are stored separately from confidence. Serving
uses only high-confidence direct exact-model-year points; inferred and
interpolated rows remain available for curation.

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
evidence, and a review timestamp.
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

`aircraft_sale_listing_avionics`

Links concrete avionics units to a specific sale listing. The link stores
quantity, provenance, evidence confidence, and an explicit `installed`,
`replaces`, or `removes` configuration action with an optional replacement
target. Valuation starts from factory defaults and applies these links as
deltas. New primary and replacement links require approved catalog identities;
the installation-evidence confidence on the link is independent from catalog
identity confidence.

`aircraft_model_variant_default_avionics`

Stores factory/default avionics for a variant/model year. This is used when a
listing panel is valued; high-confidence listing actions are then applied as
additions, replacements, or removals from this baseline.

`depreciation_profiles`, `depreciation_profile_fit_metadata`

Store fitted airframe depreciation coefficients and fit-quality metadata. The
current production path uses `generic:all` plus per-model fitted profiles when
enough samples exist.

`component_depreciation_profiles`

Stores generic component model parameters. Engine and propeller use
`baseline_life_fraction`; avionics use an age decay rate and long-run residual
fraction.

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
8. Build a similarity shortlist from the server catalog, then ask grounded
   Gemini to select an existing ID, propose a verified new identity, reject
   generic text, or fail unresolved. Similarity and exact normalized strings
   are retrieval aids only. Every positive identity—including an already
   approved match—undergoes an independent proposal attestation and
   candidate-by-candidate collision review before it can be associated.
9. Upsert manufacturer, model, and variant lookup rows.
10. Insert the listing, or update an equivalent existing listing, in the
   `incomplete` ingestion state.
11. Replace source-backed listing avionics, installed-component identities, and
   valuation facts.
12. Enrich and validate missing authoritative factory specs, exact-model-year
   price evidence, factory avionics, and listing avionics metadata.
13. Mark the listing `ready` only after every readiness query passes. A failed
   completion remains stored as `quarantined` with the error for inspection or
   reprocessing.
14. Mark valuation snapshots stale and remove orphaned lookup rows.

The listing insert path deliberately keeps code generic. If a Cessna, Cirrus, or
another maker needs better results, the preferred fix is better prompts, better
validation, or better data in reusable tables.

## Update Path

`PATCH /api/listings/{id}` merges provided fields into the current listing,
then applies the same mandatory FAA admission check before variant correction,
avionics resolution, or any database mutation. An update cannot retain or
introduce a non-N, unresolved, or serial-conflicting identity. Admitted updates
replace evidence links, return the row to `incomplete`, and run the same
completion path as inserts. If the update moves a listing to a different
aircraft model, both the old and new model scopes make the frozen valuation
snapshot stale. No listing write implicitly refits or activates a model.

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
- unapproved avionics models with no listing/default/replacement/suite links;
  approved catalog identities are retained independently of current installs
- avionics manufacturers and types with no models
- engine and propeller models with no aircraft spec references
- engine and propeller manufacturers with no models

The admin command is:

```bash
cargo run --bin aircost-admin -- cleanup-orphans
```

## Healing And Enrichment Commands

Dry-run commands are available for review before applying broad DB changes:

```bash
cargo run --bin aircost-admin -- heal-aircraft-models --dry-run
cargo run --bin aircost-admin -- normalize-variants --manufacturer Cessna --model "182 SKYLANE" --dry-run
cargo run --bin aircost-admin -- curate-avionics --dry-run
cargo run --bin aircost-admin -- repopulate-avionics --limit 10 --dry-run
cargo run --bin aircost-admin -- enrich-avionics --dry-run
cargo run --bin aircost-admin -- enrich-model-year-avionics --dry-run
cargo run --bin aircost-admin -- enrich-aircraft-specs --dry-run
cargo run --bin aircost-admin -- fit-depreciation --dry-run
```

Use `--apply` only after reviewing the report.

### FAA registry import and hierarchy-admission gate

Aircraft hierarchy curation requires a current, privacy-minimized projection
of the FAA releasable registry. Download the official ZIP from the
[FAA Releasable Aircraft Database Download](https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download),
calculate the SHA-256 of the ZIP before extraction, verify the release date,
and extract only `MASTER.txt`, `ACFTREF.txt`, and `ENGINE.txt`. The application
does not download or extract the archive and cannot independently prove that
operator-supplied member files came from the supplied archive hash.

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
listing. The importer computes member, manifest, target-set, and exact
logical-record digests while discarding registrant fields. `DEREG.txt` is not
imported, and an older release is never a fallback for the admission gate.

Inspect and extract one release outside the repository:

```sh
sha256sum /tmp/ReleasableAircraft.zip
unzip -l /tmp/ReleasableAircraft.zip
unzip /tmp/ReleasableAircraft.zip MASTER.txt ACFTREF.txt ENGINE.txt \
  -d /tmp/aircost-faa-release
```

Run the importer without `--apply` first. Dry run is the default and performs
all parsing, schema, target-coverage, and digest checks without writing:

```sh
cargo run --bin aircost-admin -- import-faa-registry \
  --database /absolute/path/to/aircost.sqlite3 \
  --master /tmp/aircost-faa-release/MASTER.txt \
  --aircraft-reference /tmp/aircost-faa-release/ACFTREF.txt \
  --engine-reference /tmp/aircost-faa-release/ENGINE.txt \
  --snapshot-date YYYY-MM-DD \
  --archive-sha256 64_CHARACTER_ZIP_SHA256 \
  --dry-run
```

`--snapshot-date` is the date represented by that daily FAA release, not the
listing model year or an arbitrary import date. Review the JSON report's
separate `listing_counts` and `pending_submission_counts`, requested and
accepted explicit targets, target count, matched and absent counts, member
hashes, manifest hash, and target-set hash. Apply the same validated input
explicitly:

```sh
cargo run --bin aircost-admin -- import-faa-registry \
  --database /absolute/path/to/aircost.sqlite3 \
  --master /tmp/aircost-faa-release/MASTER.txt \
  --aircraft-reference /tmp/aircost-faa-release/ACFTREF.txt \
  --engine-reference /tmp/aircost-faa-release/ENGINE.txt \
  --snapshot-date YYYY-MM-DD \
  --archive-sha256 64_CHARACTER_ZIP_SHA256 \
  --apply
```

For a registration recovered before its listing row can pass FAA admission,
add the flag once per aircraft to both the dry run and the corresponding apply:

```sh
cargo run --bin aircost-admin -- import-faa-registry \
  --database /absolute/path/to/aircost.sqlite3 \
  --master /tmp/aircost-faa-release/MASTER.txt \
  --aircraft-reference /tmp/aircost-faa-release/ACFTREF.txt \
  --engine-reference /tmp/aircost-faa-release/ENGINE.txt \
  --snapshot-date YYYY-MM-DD \
  --archive-sha256 64_CHARACTER_ZIP_SHA256 \
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
projections with the same snapshot date, source URL, archive hash, and manifest
hash.

The curation lookup always starts from the newest imported release. "Newest"
means the greatest operator-supplied snapshot date and projection ID; the code
does not impose a maximum age or contact the FAA during lookup. Operations must
therefore verify the date against the downloaded members and refresh the import
on the intended cadence. A target must have a coverage row in a projection of
that exact release. No snapshot, no current-release coverage, an `absent`
result, an ambiguous result, or a serial conflict blocks every listing-backed
workflow. Missing, foreign, and malformed registrations are also blocked. New
and updated listings are rejected before mutation. Pre-policy rows are not
deleted automatically, but they are excluded from avionics/reference curation,
valuation snapshot creation, training, and comparable serving. The curation
report records why an existing observation was excluded. If no source-exact
observation in a cluster passes the FAA gate, Gemini is not called.

Every new valuation snapshot freezes a versioned FAA admission manifest inside
`selection_policy_json`. For each included listing it records the canonical
N-number, normalized observed serial, FAA projection and release, archive hash,
and exact FAA source-record hash. That manifest participates in both snapshot
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
the temporary `repopulate-avionics` workflow below to classify stored listing
equipment and replace associations safely. Mechanical `normalize-avionics` is
legacy unreviewed-row hygiene, not catalog approval, and is never run
automatically during listing ingestion.

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

### Temporary stored-listing avionics rebuild

`repopulate-avionics` uses retained `plugin_submissions` rather than asking
users to add listings again. It prefers the latest submission linked through
`canonical_listing_id` and uses an exact source-URL fallback for an unlinked
submission. A retained extraction is replayed only when it has at least one
equipment item and every item uses the current non-empty `types` capability
array. Empty or scalar `type` payloads are not normalized or mechanically
converted. Instead, the tool re-runs the current Gemini listing extractor
against the retained `rendered_html`, then passes that transient current-schema
result to grounded identity resolution. The signed plugin payload is never
overwritten.

Start with one listing or the default ten-listing dry run:

```sh
cargo run --bin aircost-admin -- repopulate-avionics \
  --database /absolute/path/to/copy.sqlite3 \
  --listing-id 51

cargo run --bin aircost-admin -- repopulate-avionics \
  --database /absolute/path/to/copy.sqlite3 \
  --limit 10
```

Dry run is the default. It still makes one listing-extraction call for each
legacy or otherwise incompatible payload, in addition to the approximately two
grounded Gemini calls per attempted identity and any correction retries. Apply
mode has the same re-extraction behavior but persists approved catalog
identities and the final listing links. Set `GEMINI_API_KEY` and review the
per-listing source, re-extraction, error, and call-count fields before adding
`--apply`.

Apply mode resolves catalog identities through the same two-stage service and
replaces a listing's links only when every non-rejected primary/replacement
identity resolves to an approved ID. The replacement is one transaction; an
unresolved candidate, conflicting duplicate, or insert failure leaves all old
links intact. Multiple capability rows such as GPS and transponder labels for
one GNX 375 are coalesced only after both independently resolve to the same
approved ID. Quantities use the maximum rather than a sum.

The rebuild fails closed when an incompatible payload has no retained HTML or
source URL, the retained HTML cleans to no usable text, Gemini is unavailable,
or Gemini returns anything outside the current capability-array schema. An
empty extracted equipment array cannot erase existing links. These cases are
reported and leave all old associations intact. The workflow never invents
replacement semantics, never changes listing readiness, and never writes
dollar metadata; identity rebuilding alone does not make a listing
valuation-grade.

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
