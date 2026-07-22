# Proposal: Small-Sample Aircraft Value and Depreciation

Status: proposed

Scope: the Rust valuation path in `src/depreciation.rs`, `src/fit.rs`, and
`src/aircraft.rs`

Reviewed: 2026-07-20

## Goal

Estimate an aircraft's approximate value and depreciation from a small snapshot
of sale listings that can be collected today. This is a consumer estimation
problem, not a professional appraisal problem.

Moderate error is acceptable. The system should favor a stable, useful point
estimate over a highly qualified non-answer. With at least one valid training
listing, every query should receive:

1. an estimated current value in USD
2. an estimated error range
3. a depreciation schedule in constant-today dollars
4. a simple support grade explaining how much the estimate borrowed from broad
   rather than exact comparables

The only factual input data are current aircraft sale listings. The proposal
does not require historical listings, closed sales, price guides, new-aircraft
prices, CPI, exchange rates, registry data, maintenance databases, TBO tables,
overhaul costs, or avionics price catalogs. Fixed mathematical priors are part
of the estimator, not additional factual input data.

## Recommendation

Keep the current model's central idea—a bounded age curve plus adjustments—but
fit a much smaller, pooled model directly to current asking prices.

The first production successor should be a regularized structural regression,
not a large model:

- one shared two-parameter depreciation curve
- one shared airframe-hours effect
- a small number of optional shared component/equipment effects
- aircraft category, maker, model, and variant price levels that shrink toward
  their parent groups
- a hierarchical fallback that always produces a point estimate

Use today's cross-sectional age-price relationship as the depreciation curve.
This requires an explicit stationarity assumption: after controlling for the
listing facts available today, the relative price difference between a
10-year-old and a 15-year-old aircraft today is used as the approximate change
for a 10-year-old aircraft over its next five years. Freeze today's market
price level and report the result in constant-today dollars.

That assumption is imperfect, but it is simple, testable against current
listings, and appropriate for the requested tolerance for moderate error.

## Reassessment of the Current Model

### Overall verdict

The current formula is directionally sound for this goal. It has sensible
small-data behavior because it encodes strong assumptions: aircraft lose value
quickly when young, retain a nonzero residual value, higher-than-typical hours
reduce value, and component condition can adjust value. Bounded floors and caps
also prevent absurd outputs.

The problem is primarily its calibration and data dependencies, not the idea
of a structural formula.

| Property | Verdict under this project's goal |
| --- | --- |
| Always returns an approximate value | Good; preserve this behavior. |
| Bounded, interpretable age curve | Good; simplify and pool it. |
| Useful with few exact-model examples | Potentially good because of priors and generic fallback. |
| Uses only current listings | No; current valuation depends on new-price, inflation, TBO, overhaul-cost, and avionics-value metadata. |
| Fit is reliable with four model samples | No; the search space is far too large. |
| Reported fit errors predict new-listing error | No; parameters and errors are calculated on the same samples. |
| Provides an approximate depreciation projection | Yes, if presented as a constant-market cross-sectional projection. |

### Evidence already in the repository

The legacy Python listing check contains 36 manually assembled current-listing
examples. It reports:

- mean absolute percentage error: 20.5%
- median absolute percentage error: 13.5%
- mean signed percentage error: -12.1%
- per-model mean absolute percentage error: 10.9% to 25.5%
- some individual errors of roughly 40% to 60%

That check exercises a different legacy formula and is not an out-of-sample
test, so it is not a production accuracy claim. It is nevertheless encouraging
for the revised objective: a simple structural model appears capable of
producing useful ballpark values, while its large individual misses show why an
error range should accompany the point estimate.

The Rust fitter currently evaluates 77,175 parameter combinations for each
scope and can create a model-specific fit from only four listings. It then
reports errors on those same listings. Selecting that many alternatives from so
few observations will usually find accidental patterns. The apparent training
error can therefore be much better than the error on the next listing.

The current training query also accepts positive asking prices without a
complete modeling policy for duplicate aircraft, stale listings, listing
status, currencies, auctions, or unverifiable prices. Those data issues can
matter more than fine adjustments to the depreciation equation when the sample
is small.

The current Rust suite passes 30 unit tests, including useful checks of formula
bounds and component arithmetic. There are no dedicated tests in `src/fit.rs`
for parameter recovery, sample thresholds, or held-out performance. Passing the
suite establishes software correctness for the tested cases, but does not
establish predictive accuracy.

### Parts to retain

- a smooth, decreasing age factor with a positive floor
- bounded adjustments rather than unconstrained extrapolation
- generic fallback when no exact-model fit exists
- an explicit estimate decomposition for debugging
- point estimates for current and future ages

### Parts to replace

- Replace historical purchase price and assumed inflation with a price level
  learned directly from today's listings.
- Replace external TBO, overhaul-cost, and avionics-price adjustments with
  small shared effects learned from facts written in listings.
- Replace the six-dimensional grid search with continuous regularized fitting
  of a few global parameters.
- Replace four-sample independent model fits with partial pooling across the
  aircraft hierarchy.
- Replace in-sample MAPE as the main quality signal with grouped out-of-sample
  predictions that reuse nearly all observations.
- Replace the fixed 200-hour annual-utilization assumption with a utilization
  estimate derived from current listing hours and ages.

## What the Model Estimates

The training label is the advertised USD asking price. For this project's
purpose, the model calls its result an **estimated value**. More precisely, it
is the typical asking price implied by the current listing snapshot after
adjusting for the aircraft facts the model understands.

The distinction matters for error measurement: validation compares the model
with held-out asking prices. It does not need to solve the separate problem of
predicting the eventual negotiated sale price.

Only listings with a numeric USD price should train the first version.
Listings in another currency are usable only when the listing itself also
states a USD price; no external FX data are required.

## Listing Data Contract

### Minimum fields

Each training example should contain, when stated in the listing:

- asking price in USD
- capture time and listing URL or source identifier
- category, maker, model, and variant
- manufacture year
- total airframe hours and the stated time basis
- engine and propeller count
- engine and propeller time values with their stated bases, such as since new
  or since overhaul
- structured equipment or condition statements that can be extracted
- enough listing identity data to detect the same physical aircraft appearing
  more than once

Missing technical fields remain missing. Do not invent them from model
knowledge.

### Snapshot preparation

Before fitting:

1. Keep listings active at the snapshot time with a positive, explicit USD
   asking price.
2. Exclude `call for price`, auctions without a current numeric ask, fractional
   shares, lease rates, salvage-only parts, and obvious data-entry errors.
3. Group duplicates for the same physical aircraft. Prefer the most complete
   current record for fitting, while retaining duplicate prices for audit.
4. Normalize maker/model/variant names without enriching them from an external
   specification database.
5. Record missingness explicitly; do not treat an unstated hour value as zero.
6. Freeze the prepared rows and extraction version so validation is
   reproducible.

With small samples, a manual review of the prepared rows is worth more than a
more elaborate estimator trained on accidental duplicates.

## Proposed Structural Model

### Core equation

Fit price on a logarithmic scale:

```text
log(value_i) = alpha_i
             + log R(age_i)
             + beta_h * hours_residual_i
             + beta_e * engine_state_i
             + beta_p * propeller_state_i
             + beta_q * equipment_summary_i
             + error_i
```

where:

```text
alpha_i = global + category + maker + model + variant

R(age) = floor + (1 - floor) * exp(-decay * age)
```

`floor` and `decay` are global in the first version. This is intentionally
small: the age shape is learned from all listings together, while identity
terms establish the different dollar scales of a trainer, piston twin,
business jet, and so on.

Use a Huber loss on log price, plus regularization. Log price makes a $20,000
miss on a small piston aircraft matter more similarly to a proportionate miss
on a jet, and Huber loss prevents a single unusual listing from controlling a
tiny dataset.

### Smallest working version

When only a few directly comparable listings are available, the same model has
a particularly simple form. Normalize each comparable `j` to the target
aircraft `q`:

```text
adjusted_value_j(q) = listing_price_j
                    * R(age_q) / R(age_j)
                    * exp(hours_adjustment_q - hours_adjustment_j)

estimated_value(q) = weighted_median(adjusted_value_j(q))
```

Weight exact variants most, then exact models, maker/category matches, and
finally the global sample. Cap the influence of any one advertisement. With a
single comparable, its price is simply moved along the fixed age/hours curve;
with more listings, the median becomes resistant to an unusually optimistic
seller.

The regularized regression below is the pooled implementation of this idea. It
learns shared adjustments and group price levels across all current listings,
but the normalized-comparable calculation is also a useful baseline and an
auditable emergency fallback.

### Hierarchical price levels

Identity effects should be regularized toward their parents:

```text
variant -> model -> maker/category -> category -> global
```

An exact-model listing is useful but does not create a fully independent model.
For example, one variant observation receives a strongly shrunk variant
adjustment on top of a better-supported model or category estimate. More
observations automatically permit a larger data-driven adjustment.

This can be implemented as ridge-penalized categorical effects. A full Bayesian
implementation is optional; empirical-Bayes-style shrinkage is enough for the
first version.

### Age curve priors

With only a few listings, `floor`, `decay`, and the group price levels can trade
off against one another. Prevent unstable curves with bounded priors:

```text
0.10 <= floor <= 0.70
0.01 <= decay <= 0.25
```

Initialize them from the existing generic curve and penalize large movement
unless cross-validation supports it. These are estimator assumptions, not
additional aircraft data. Do not fit separate age curves by model until the
whole snapshot demonstrates that category-specific curves improve held-out
error.

### Airframe hours

Age and total hours are correlated. First estimate typical accumulated hours
from the current snapshot using a robust global or category relationship:

```text
expected_log_hours(age, category)
hours_residual = log(1 + stated_hours) - expected_log_hours
```

`beta_h` is shared and constrained to be non-positive: more hours than similar
age aircraft should not increase the estimate. If hours are missing, use a
missingness indicator and a zero residual, which means typical hours for age.

The category relationship should shrink to the global relationship. It must not
use the current hard-coded 200 hours per year.

### Engine and propeller state

Do not convert component time into a percentage of TBO because TBO would be an
external fact. Instead, encode what the listing states:

- `time_since_new`, `time_since_overhaul`, or `time_remaining`
- log-transformed numeric time
- component count
- a missingness flag

Fit one or two shared bounded effects, with separate direction constraints for
`time remaining` and `time since` measurements. In the earliest small-sample
version these effects may be fixed to zero. They should activate only when they
reduce grouped cross-validation error; returning a robust value is more
important than explaining every component.

### Equipment and condition

Start with a few reproducible listing-derived summaries rather than assigned
dollar values:

- count of explicitly named avionics items
- count of explicitly named upgrades or modifications
- explicit damage-history/restoration/overhaul phrases
- listing completeness or number of known technical fields

All summaries need an evidence span in the source listing. Their coefficients
are shared, heavily regularized, and bounded. Raw free text should not enter the
first model because small datasets invite memorization and price leakage.

## A Fallback That Always Returns a Value

The estimator should never abstain solely because an exact comparable is
missing. It should degrade gracefully:

1. use a shrunk variant effect when known
2. otherwise use the model effect
3. otherwise use maker/category and category effects
4. otherwise use the global price level and global age curve

Unknown categories and missing fields take the global/default effect. Provided
the snapshot contains at least one valid USD listing, the final fallback is an
age-adjusted global anchor. With an assumed global age curve, initialize that
anchor as `median(log(price) - log R(age))`; this keeps even a one-listing model
on the observed dollar scale instead of applying the age discount twice.

Return a support grade alongside every result:

| Grade | Meaning |
| --- | --- |
| High | Several deduplicated exact-model or close-variant listings contribute. |
| Medium | Estimate mainly borrows from the same maker/category or neighboring models. |
| Low | Estimate mainly uses the global price level and structural priors. |

The grade informs the error range but never suppresses the point estimate.

## Depreciation From Today's Listings

### Definition

For an aircraft currently aged `a`, estimate value `t` years from now by
holding today's market scale and listed equipment constant, advancing age, and
advancing total hours at a utilization rate learned from today's listings:

```text
future_hours(t) = current_hours + utilization_rate * t

value(t) = predict(
    same identity and equipment,
    age = a + t,
    airframe_hours = future_hours(t)
)
```

Equivalently, the age-only part is:

```text
value(t) = value_now * R(a + t) / R(a)
```

with an additional bounded hours adjustment when hours are available.

Estimate `utilization_rate` as the robust median of
`airframe_hours / max(age, 1)` for comparable current listings, shrunk toward
the global snapshot median. This remains listing-only.

### Output

For each requested year, return:

- estimated value in constant-today USD
- dollar depreciation from today
- percentage depreciation from today
- estimated one-year depreciation rate at that age
- the same error range and support grade as the current estimate, widened with
  projection horizon

Do not add inflation or forecast whether the overall aircraft market rises or
falls. The result answers: "What would this aircraft approximately be worth in
today's market after it has aged and accumulated typical hours?"

Component hours should remain at their current listing-derived effect in the
first version. Simulating overhaul cycles without external TBO and overhaul
costs would add false precision.

## Giving Error a Number

The output should contain both a point estimate and an empirical error band.
Use only predictions made without the target aircraft in the training fold.

For each out-of-fold prediction, record:

```text
log_error = abs(log(actual_price / predicted_price))
```

Use the median and 80th percentile of these errors, pooled hierarchically in the
same way as price effects. Convert an 80th-percentile log error `q80` into a
multiplicative range:

```text
low  = estimate * exp(-q80)
high = estimate * exp( q80)
```

When there are too few out-of-fold residuals for a group, borrow the broader
group's error. When the entire snapshot is extremely small, use a conservative
fixed fallback range and label support low. A starting fallback of approximately
`-35% / +55%` is consistent on a log scale and deliberately wider than the
legacy median error. Re-estimate it as soon as enough out-of-fold residuals
exist.

For future years, widen `q80` smoothly with horizon rather than claiming the
current-value error is unchanged. The widening factor is a policy assumption
until repeated snapshots become available.

## Validation With Small Samples

A large permanent test set wastes too much data at the start. Use grouped
resampling:

- For fewer than 20 deduplicated aircraft, use leave-one-aircraft-out
  cross-validation.
- For 20 or more, use repeated grouped five-fold cross-validation.
- Keep duplicate advertisements for the same physical aircraft in the same
  fold.
- Also run leave-one-model-out validation to measure the broad fallback used
  for aircraft without exact comparables.
- Bootstrap complete aircraft groups to show how sensitive values and curves
  are to the particular small sample.

Compare three candidates on identical folds:

1. current-category or global median asking price
2. simplified existing bounded formula with listing-only anchors
3. proposed pooled structural regression

The first release targets useful, not appraisal-grade, performance:

| Metric | Initial target |
| --- | --- |
| Median absolute percentage error | 25% or less |
| Mean signed percentage error | between -10% and +10% |
| 80th-percentile absolute percentage error | 40% or less |
| 80% empirical range coverage | 70% to 90% |

These are starting engineering gates, not claims that every aircraft will be
within 25%. If the proposed model does not beat the median baseline, deploy the
simpler baseline while still returning its empirically measured error range.

Track metrics overall and by support grade. Do not publish exact-model metrics
from groups with only a handful of out-of-fold observations; pool them upward.

## Implementation Instructions

These instructions target the current Rust application. Build the new path
alongside `src/depreciation.rs` and switch callers only after shadow validation.
Do not mutate the old `AircraftProfile` types until the new API is serving the
same product endpoints.

### 1. Add a shared valuation module

Create this module tree:

```text
src/valuation/
  mod.rs          public model-independent API
  types.rs        query, estimate, curve, support, and artifact types
  dataset.rs      listing-only snapshot construction
  comparable.rs   adjusted-comparable median baseline
  structural.rs   pooled structural fit and inference
  validation.rs   grouped folds, metrics, and error calibration
  store.rs        snapshot/model persistence and activation
```

Export it from `src/lib.rs` with `pub mod valuation;`. Keep database access in
`dataset.rs` and `store.rs`; all fitting and prediction functions should accept
plain Rust data so their tests do not require SQL.

Define a model-independent boundary similar to:

```rust
pub trait ValuationModel: Send + Sync {
    fn model_version_id(&self) -> i64;
    fn estimate(&self, query: &ValuationQuery) -> Result<ValuationEstimate, ValuationError>;
}

pub struct ValuationQuery {
    pub manufacturer_id: Option<i64>,
    pub model_id: Option<i64>,
    pub variant_id: Option<i64>,
    pub model_year: i64,
    pub valuation_year: i64,
    pub airframe_hours: Option<f64>,
    pub engine_times: Vec<ComponentObservation>,
    pub propeller_times: Vec<ComponentObservation>,
    pub equipment_tokens: Vec<String>,
}

pub struct ValuationEstimate {
    pub estimated_value_usd: f64,
    pub low_value_usd: f64,
    pub high_value_usd: f64,
    pub estimated_error_fraction: f64,
    pub support: SupportGrade,
    pub model_version_id: i64,
    pub breakdown: ValuationBreakdown,
    pub depreciation: Vec<DepreciationPoint>,
}
```

Use `Option<f64>` in the new boundary even though the current listing table has
non-null hour columns. This prevents the application from confusing a future
missing value with a real zero. Validate age and hours once when constructing
the query.

Do not reuse `PriceEstimate` or `EstimateBreakdown` for the new implementation.
Those types encode new-price, inflation, TBO-cost, and avionics-dollar concepts
that the listing-only model deliberately removes.

### 2. Add additive persistence tables

The repository initializes databases by executing
`aircost/webapp/schema.sql` or `schema.postgres.sql`; it does not currently have
a migration runner. Add new `CREATE TABLE IF NOT EXISTS` statements to both
schema files instead of adding required columns to an existing table.

Add these tables:

| Table | Required contents |
| --- | --- |
| `valuation_snapshots` | ID, capture time, input SHA-256, selection-policy JSON, feature-schema version, included/excluded counts, created time. |
| `valuation_snapshot_rows` | Snapshot ID, listing ID, duplicate-group key, inclusion flag, exclusion reason, frozen feature JSON, target price, and row hash. |
| `valuation_model_versions` | ID, snapshot ID, `structural`/`dnn` kind, artifact-format version, candidate/active/retired state, metrics JSON, configuration JSON, created time. |
| `valuation_model_artifacts` | Model-version ID, artifact name, artifact bytes, SHA-256, and media type. Use SQLite `BLOB` and Postgres `BYTEA`. |
| `valuation_fold_predictions` | Model-version ID, fold ID, duplicate-group key, listing ID, actual/predicted price, log error, absolute percentage error, and support grade. |

Use foreign keys with cascade deletion from a candidate model version to its
artifacts and fold predictions. Do not cascade from listings into a frozen
snapshot: the snapshot must remain reproducible even if a live listing later
changes. `valuation_snapshot_rows.feature_json` is therefore the authoritative
training row after capture. Store the source listing ID as a copied scalar with
no cascading foreign key, or make a foreign key nullable with `ON DELETE SET
NULL` while retaining a separate copied source ID.

Activation must occur in one database transaction:

1. verify the artifact hash and validation gates
2. change the previous active model of the same kind to `retired`
3. change the candidate to `active`
4. commit

Application startup should fail closed for a corrupt active artifact and load
the most recent valid structural artifact. It should not silently create new
weights during a request.

### 3. Build a listing-only frozen snapshot

Implement `dataset::create_snapshot(db, policy)` using
`aircraft_sale_listings` joined only to listing identity and listing-attached
equipment names. The training query must not join:

- `aircraft_model_variant_price_points`
- engine or propeller TBO/cost columns
- avionics estimated values or introduction dates
- `aircraft_model_spec_versions` economic metadata

The first implementation can use fields already present:

- manufacturer/model/variant IDs from the listing's identity joins
- `model_year`, `asking_price_usd`, `currency`, `added_at`, and `status`
- registration and serial number
- airframe, engine, and propeller hours
- names and counts from `aircraft_sale_listing_avionics` rows whose source is
  the listing itself, but not their values

The current schema has no aircraft category or component time basis. Set
`category_key` and time-basis features to unknown and keep their coefficients
disabled. Add them later only as optional values in the frozen feature JSON
when the listing extractor preserves a source-backed value. Their absence must
not block the first release.

Apply the selection policy in this order and record an exclusion reason for
every rejected row:

1. require `status = 'active'`
2. require `currency = 'USD'` and a finite positive asking price
3. require a plausible nonfuture model year and nonnegative hour values
4. require a source-backed or explicitly trusted listing under the configured
   collection policy
5. enforce a configured maximum listing age relative to snapshot capture time
6. deduplicate physical aircraft

Deduplicate with the strongest available listing-derived identifier:

```text
normalized serial number
  else normalized registration number
  else conservative fingerprint(
         manufacturer, model, variant, model_year,
         rounded airframe_hours, rounded engine_hours, rounded propeller_hours
       )
```

Prefer not to merge ambiguous fallback fingerprints. When duplicates disagree
on price, retain the most recently captured complete advertisement as the
training row and record every member of the group in the snapshot metadata.

Sort frozen rows by listing ID before hashing. The snapshot SHA-256 should cover
the selection policy, feature-schema version, listing IDs, row hashes, and
duplicate-group assignments. A dry run must print included/excluded counts,
duplicate groups, missingness, and counts per manufacturer/model/variant before
anything is persisted.

### 4. Implement the adjusted-comparable baseline first

In `comparable.rs`, implement the formula from "Smallest working version."
Store the fixed initial age curve and hours coefficient in a versioned
`ComparableConfig`, not scattered constants.

For a target query:

1. calculate the age and hours adjustment from every eligible frozen listing
2. assign a similarity level: exact variant, exact model, same manufacturer,
   or global
3. calculate the weighted median adjusted value
4. cap any one duplicate group at 50% of total weight
5. return the global age-adjusted anchor if no closer group is available

Use deterministic initial similarity weights and persist them in model
configuration. Tune them only as one of a few predeclared configurations in
grouped validation. Unit-test the single-listing case: a query identical to the
only listing must reproduce its asking price before rounding.

This baseline supplies a production-safe fallback while the pooled fitter is
being implemented.

### 5. Implement the pooled structural fitter

Serialize the fitted model as `StructuralArtifactV1` containing at least:

```rust
pub struct StructuralArtifactV1 {
    pub snapshot_id: i64,
    pub snapshot_year: i64,
    pub global_log_anchor: f64,
    pub age_floor: f64,
    pub age_decay: f64,
    pub expected_hours: HoursTrend,
    pub beta_hours: f64,
    pub identity_offsets: IdentityOffsets,
    pub optional_feature_coefficients: BTreeMap<String, f64>,
    pub group_counts: GroupCounts,
    pub error_bands: ErrorBands,
    pub utilization_rates: UtilizationRates,
    pub feature_schema_version: u32,
}
```

Implement fitting in this sequence:

1. Convert price to `log(price)` and age to
   `max(snapshot_year - model_year, 0)`.
2. Fit the pooled expected-hours relationship on `log1p(hours)` with robust
   iteratively reweighted least squares. Fit a category adjustment only after
   category exists and validation supports it.
3. For fixed `floor` and `decay`, subtract `log R(age)` from log price and build
   a design matrix for the global anchor, identity effects, hours residual, and
   enabled optional features.
4. Solve the ridge-penalized Huber problem with iteratively reweighted least
   squares. Use a Cholesky or QR solve; never form a matrix inverse.
5. Penalize variant effects most, model effects less, manufacturer effects less
   again, and the global anchor not at all. A fixed per-level penalty naturally
   shrinks small groups more because repeated groups contribute more rows.
6. Constrain `beta_hours <= 0`. Keep component/equipment coefficients at zero
   until a source-backed feature occurs repeatedly and improves validation.
7. Optimize only `floor` and `decay` outside the linear solve. Start from the
   existing generic curve, use a small bounded coordinate search, halve its
   step size until convergence, and enforce the bounds in this proposal. Do not
   reproduce the existing 77,175-candidate search.
8. Refit all coefficients after selecting the age parameters, then calculate
   group counts, utilization, and the age-adjusted global fallback.

Adding a small linear-algebra dependency such as `nalgebra` is preferable to a
home-grown matrix inverse. Keep feature construction and coefficient ordering
explicit in the artifact; inference must not depend on a hash-map iteration
order.

The fitter should be a pure function at its core:

```rust
pub fn fit_structural(
    rows: &[TrainingListing],
    config: &StructuralFitConfig,
) -> Result<StructuralArtifactV1, ValuationError>;
```

Reject an artifact containing nonfinite values, an invalid floor/decay, a
positive hours coefficient, or an anchor that cannot reproduce the training
price scale. Do not reject merely because a group has few samples; omit or
shrink that group instead.

### 6. Implement grouped evaluation and error calibration

Put all fold construction in `validation.rs` and share it with the DNN. Assign
folds by `duplicate_group_key` before any preprocessing. Use a stable seeded
hash so the same frozen snapshot produces the same folds.

For each outer fold:

1. build vocabularies, hour trends, normalizers, and coefficients using only
   training rows
2. predict every held-out row without modifying the fit
3. assign support using counts from that training fold
4. persist the fold prediction

Use leave-one-aircraft-out below 20 groups and repeated grouped five-fold above
that threshold. Run leave-one-model-out as a separate fallback report; do not
mix those residuals into ordinary fold metrics.

Calculate MdAPE, mean signed percentage error, 80th-percentile absolute
percentage error, log RMSE, and empirical interval coverage from held-out
predictions. Calibrate `q80` from absolute log errors. Pool error bands upward
from variant/model to manufacturer/global whenever the group has fewer than ten
held-out residuals.

Use this initial deterministic support policy and store it in configuration:

```text
high:   at least 5 deduplicated exact-model training rows
medium: at least 2 exact-model rows or at least 5 same-manufacturer rows
low:    everything else
```

Support affects range selection, never whether a point estimate exists. When
there are fewer than two total aircraft groups, skip empirical calibration and
use the proposal's conservative `-35% / +55%` fallback.

### 7. Add admin commands and safe activation

Extend `src/admin.rs` with:

```text
aircost-admin snapshot-valuations [--max-age-days DAYS] [--apply]
aircost-admin fit-valuation --kind structural --snapshot-id ID [--apply]
aircost-admin validate-valuation --model-version-id ID
aircost-admin activate-valuation --model-version-id ID
```

All commands default to dry run. `fit-valuation` should always print:

- snapshot and deduplicated sample counts
- chosen capacity/features and coefficients
- grouped metrics and baseline deltas
- error bands by support grade
- artifact hash and whether activation gates pass

`--apply` on fitting may persist a candidate, but it should not activate it.
Activation is a separate explicit command so a newly collected outlier cannot
replace the serving model automatically.

Replace the best-effort per-model refit hook in `src/listings.rs` with a stale
marker or diagnostic. The current hook silently refits after listing mutations;
the new model must instead be built from an identified frozen snapshot. Keep
`fit-depreciation` available during one compatibility release, but label it
legacy in help text.

### 8. Integrate prediction without external metadata

Load the active artifact once into an `Arc<dyn ValuationModel>` in application
state. Verify its hash and format version at load time. Do not query model
coefficients from SQL for every listing.

Refactor `aircraft.rs` in this order:

1. build `ValuationQuery` directly from the listing and its identity
2. call the active `ValuationModel`
3. map `ValuationEstimate` into API response fields
4. remove the requirement that a listing have an
   `aircraft_model_variant_price_points` row before it can receive an estimate
5. remove TBO, overhaul-cost, inflation, replacement-price, and avionics-dollar
   inputs from the new path

Add these response fields while retaining old fields during the compatibility
release:

```text
estimated_value_usd
estimated_value_low_usd
estimated_value_high_usd
estimated_error_fraction
valuation_support
valuation_model_kind
valuation_model_version_id
valuation_snapshot_id
valuation_breakdown
value_curve
```

Generate `value_curve` for horizons 0 through 30 from the current valuation
year, not from the aircraft's manufacture year. At each horizon keep identity
and equipment fixed, advance age, and advance hours using the artifact's
listing-derived utilization rate. Calculate depreciation relative to the
horizon-zero estimate so the first point always has zero dollar depreciation.

If no active listing-only artifact exists, use the adjusted-comparable model
from the newest valid snapshot. Use the external-metadata estimator only as a
temporary shadow comparison, never as an input to the listing-only result.

### 9. Add tests before switching traffic

Add pure unit tests beside the new modules for:

- one listing reproduces its own price through the comparable anchor
- several listings recover a known synthetic age curve within tolerance
- age-only estimates are non-increasing and retain the configured floor
- extra hours never increase value
- unknown variant/model/manufacturer falls back in the documented order
- missing optional fields still return a finite positive estimate
- duplicate advertisements never cross folds or receive duplicate weight
- fold preprocessing never reads held-out prices or vocabularies
- interval conversion is symmetric on log scale
- artifact serialization round-trips without changing predictions

Add SQLite and Postgres-oriented database tests for snapshot creation,
candidate persistence, single-active-model transactions, and artifact hash
failure. Reuse the repository's database test patterns; do not make unit tests
depend on a production snapshot.

Before activation, run:

```text
cargo fmt --all -- --check
cargo test --locked
cargo check --locked
```

### 10. Roll out in three steps

1. **Shadow:** compute old and new estimates, serve the old value, and record
   only aggregate comparison diagnostics without changing training data.
2. **Listing-only active:** serve the structural estimate and range; retain the
   old calculation only for rollback.
3. **Cleanup:** after one successful snapshot refresh and rollback drill,
   remove the serving dependency on new-price, inflation, TBO-cost, and
   avionics-value metadata. Keep those fields only for unrelated operating-cost
   features if still needed.

Implementation is complete when a fresh database can ingest current listings,
freeze a snapshot, fit and validate a candidate, activate it transactionally,
and return a point value plus range and 30-year curve for a query with no
model-year purchase-price record.

## Decision Rules

Adopt the pooled structural model when it:

- returns a finite positive value for every supported query
- satisfies age monotonicity and configured bounds
- improves grouped median error or signed bias over the hierarchical median
- remains stable when one aircraft is removed
- produces error ranges with reasonable out-of-fold coverage

Prefer the simpler median or generic structural fallback whenever extra listing
features make resampled accuracy worse. Small-sample modeling rewards restraint.

## Relationship to the DNN Proposal

The DNN in [dnn_valuation_model.md](dnn_valuation_model.md) uses the same
listing contract, estimand, fallback behavior, depreciation assumption, and
validation folds. It is a deliberately tiny, constrained network rather than a
large tabular Transformer. The pooled structural regression remains the
recommended first implementation and the baseline the DNN must beat.

## References

- Rosen, S. (1974), [Hedonic Prices and Implicit Markets](https://doi.org/10.1086/260169).
- Hyndman, R. J. and Koehler, A. B. (2006), [Another look at measures of forecast accuracy](https://doi.org/10.1016/j.ijforecast.2006.03.001).
- Current model description: [depreciation_model.md](../depreciation_model.md).
- Current estimator: [depreciation.rs](../../src/depreciation.rs).
- Current fitter: [fit.rs](../../src/fit.rs).
