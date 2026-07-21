# Database Schema And Write Lifecycle

The app supports SQLite and Postgres through the same Rust data-access layer.
Schemas live in `aircost/webapp/schema.sql` and
`aircost/webapp/schema.postgres.sql`; `src/db.rs` loads the correct schema on
startup and seeds the developer user plus baseline depreciation profiles.

## Core Tables

`users`

Stores local or authenticated users. Development defaults to
`developer@localhost`.

`aircraft_manufacturers`, `aircraft_models`, `aircraft_model_variants`

Normalize aircraft identity. Variants point to models; models point to
manufacturers. Display names are stored with normalized keys for matching and
deduplication.

`aircraft_sale_listings`

Stores canonical sale listing facts: model variant, source URL, model year,
asking price, currency, status, registration, serial number, and airframe,
engine, and propeller hours.

`plugin_installs`, `plugin_submissions`

Store Chrome extension registrations and submitted rendered HTML. Submissions
retain the HTML, extraction result or error, and the canonical listing created
from the submission when extraction succeeds.

`aircraft_model_spec_versions`

Stores variant-level operating and component metadata. This includes fuel burn,
oil assumptions, linked engine and propeller models, component counts, TBOs,
overhaul costs, component baseline-life fractions, annual inspection, variable
maintenance, source URL, and the depreciation profile assigned to the variant.

`aircraft_model_variant_price_points`

Stores nominal new-price points for a variant/model year. These are used as the
airframe basis for valuations after subtracting default avionics.

`engine_manufacturers`, `engine_models`

Store reusable engine metadata: manufacturer, model, TBO, overhaul cost, value
reference year, and source information.

`propeller_manufacturers`, `propeller_models`

Store reusable propeller metadata with the same role as engine metadata.

`avionics_manufacturers`, `avionics_types`, `avionics_models`

Store concrete avionics units or named suites. Generic entries such as
`Autopilot`, `GPS`, or aircraft-maker-as-avionics-maker labels should not be
stored as durable avionics models.

`aircraft_sale_listing_avionics`

Links concrete avionics units to a specific sale listing. The link stores
quantity and provenance (`listing` or `factory_default`).

`aircraft_model_variant_default_avionics`

Stores factory/default avionics for a variant/model year. This is used when a
listing has no concrete avionics, or when generic listing avionics are replaced
by a grounded factory default.

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
2. Normalize manufacturer/model/variant.
3. Compare model-family and variant candidates from known DB rows.
4. Ask Gemini to confirm plausible candidate matches when string similarity is
   insufficient.
5. Correct non-conforming variant labels when they include maker or model year.
6. Resolve avionics candidates against grounded sources and reject generic
   labels that cannot be made concrete.
7. Upsert manufacturer, model, and variant lookup rows.
8. Insert the listing or update an existing matching listing.
9. Replace listing avionics links.
10. Enrich missing aircraft spec metadata for the variant, if possible.
11. Enrich missing model-year default avionics and price point, if possible.
12. Enrich missing listing avionics metadata.
13. Normalize avionics labels.
14. Fit depreciation parameters for the affected model.
15. Remove orphaned lookup rows.

The listing insert path deliberately keeps code generic. If a Cessna, Cirrus, or
another maker needs better results, the preferred fix is better prompts, better
validation, or better data in reusable tables.

## Update Path

`PATCH /api/listings/{id}` merges provided fields into the current listing,
reruns variant correction and avionics resolution, updates the canonical listing
row, replaces avionics links, and then runs the same completion path as inserts.

If the update moves a listing to a different aircraft model, the old model is
also refit on a best-effort basis. Orphan cleanup runs after the update.

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
- avionics models with no listing or default-avionics links
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
cargo run --bin aircost-admin -- enrich-avionics --dry-run
cargo run --bin aircost-admin -- enrich-model-year-avionics --dry-run
cargo run --bin aircost-admin -- enrich-aircraft-specs --dry-run
cargo run --bin aircost-admin -- fit-depreciation --dry-run
```

Use `--apply` only after reviewing the report.

Listing-only valuation is an explicit staged workflow:

```bash
cargo run --bin aircost-admin -- snapshot-valuations --max-age-days 180 --apply
cargo run --bin aircost-admin -- fit-valuation --kind structural --snapshot-id ID --apply
cargo run --bin aircost-admin -- validate-valuation --model-version-id ID
cargo run --bin aircost-admin -- activate-valuation --model-version-id ID
```

Snapshotting and fitting default to dry run. Fitting persists only a candidate;
activation always requires a separate command.

## Schema Design Rules

- Prefer non-null columns for values required by estimation or identity.
- Keep optional columns only for truly optional metadata or unavailable external
  facts.
- Do not embed migrations in Rust runtime code. During active development it is
  acceptable to update schemas and reset local data.
- Avoid obsolete compatibility fields. If a field is no longer used, remove it
  from the schema and write path.
- Do not store canonical/non-canonical duplicates unless both are needed by an
  active query path.
- Treat sale listings and rental offerings as roots; generated lookup records
  should be removable when no root references them.
