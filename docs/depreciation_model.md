# Aircraft Value Model

The Rust valuation implementation lives under `src/valuation/`, with the
reference-grounded serving adapter in `src/aircraft.rs`. It estimates
asking-market value; it is not a certified appraisal or a tax-depreciation
model.

## Valuation Contract

A listing is eligible for training or serving only when all of the following
are true:

- its current aircraft identity is backed by the current exact FAA N-number
  assignment and compatibility projection;
- exactly one published factory reference configuration applies to that
  designation, generation, package, model year, United States or global
  market, and FAA serial number;
- that reference version has a direct full-standard-configuration USD price
  and complete attested avionics, engine, propeller, and feature fact sets; and
- its reference price and any avionics delta values are expressed in the
  valuation year's nominal dollars.

The resolver fails closed on missing or ambiguous profiles. It does not fall
back to a legacy specification, model-year price point, default-avionics row,
or a label-similar aircraft. Listing registration and serial fields are
consistency guards; the current FAA assignment supplies the controlling
N-number and serial.

## Learned Market Curve

The structural model freezes a deduplicated snapshot of active USD listings,
fits a pooled log-price model, and persists a versioned artifact before
activation. Every included snapshot row freezes the applicable reference
configuration/version, full-standard-configuration price, and nominal dollar
year in the feature payload and row hash.

The bounded shared age curve is:

```text
R(age) = floor + (1 - floor) * exp(-decay * age)
```

Manufacturer, model, and variant log-price offsets are ridge-shrunk toward the
global anchor. A shared non-positive coefficient adjusts for total airframe
hours relative to a robust listing-derived age-hours trend. Engine and
propeller observations reach the model only when their basis, evidence, and
confidence pass the source-backed component gate.

Grouped out-of-fold residuals calibrate a multiplicative error range and a
high/medium/low support grade. Aircraft groups are deterministically divided
between error-band calibration and metric evaluation, so interval coverage is
not reported on the residuals that set the interval. Repeated-fold residuals
from one physical aircraft count once when an error band is selected.

Structural and adjusted-comparable predictions are evaluated together.
Activation requires structural median error, 80th-percentile error, and
absolute bias to remain within two percentage points of the comparable shadow,
in addition to the absolute safety gates. Once the snapshot contains multiple
models, empty leave-one-model-out validation blocks activation. A one-model
snapshot may serve within that model, but its report carries a scope warning.

Support requires exact-variant observations and proximity to the observed
age/hours trend for a high grade. Broad model counts alone provide at most
medium support. Projections cover horizons zero through thirty, hold today's
market scale constant, and advance hours at a utilization rate learned from
the snapshot. If no structural artifact is active, an eligible newest snapshot
with at least five deduplicated aircraft groups can serve an explicitly
uncalibrated adjusted-comparable fallback.

## Factory Reference Anchor

The model learns the relative age/hour market curve. It does not choose the
aircraft's starting configuration value at request time. The published
reference catalog supplies that anchor:

```text
configuration_basis =
  full_standard_configuration_price
  + listing_configuration_delta
```

The listing delta compares high-confidence listing avionics actions with the
immutable factory avionics set. `installed`, `replaces`, and `removes` actions
are applied explicitly. Known suite containment consumes bundled quantities so
a suite and its components are not counted twice. Only approved installed
contribution values in the valuation year's nominal dollars can contribute to
the delta; a missing or differently denominated value stops valuation instead
of invoking an inferred inflation adjustment.

After the model produces a market estimate, the serving adapter removes its
learned identity/configuration anchor and rescales the estimate and projection
range to the exact reference basis:

```text
learned_configuration_anchor =
  global_anchor
  * category_factor
  * manufacturer_factor
  * model_factor
  * variant_factor
  * optional_features_factor

reference_scale = configuration_basis / learned_configuration_anchor

reference_grounded_estimate = learned_estimate * reference_scale
```

The age and hour behavior, uncertainty, support grade, and model version remain
model outputs. The identity/configuration anchor exposed in the breakdown is
the published reference basis. Listing equipment tokens are not also passed to
the model during serving, which prevents counting the same upgrade twice.

## Identity And Applicability

Aircraft identity has reviewed make, family, designation, generation, and
factory-package dimensions. A reference configuration is keyed by family,
designation, optional generation, optional package, and a display identity.
Each immutable published version then declares:

- one model year;
- one or more market scopes (`US` or `GLOBAL` for the current policy); and
- either all serials or an explicit normalized serial prefix/range.

A correction creates and publishes a successor version. Published versions
and their facts are not edited in place. Overlapping published versions that
would make a listing ambiguous are rejected by publication and fail closed at
resolution.

## Curating A Reference Version

Grounded research produces a reviewed, normalized JSON decision containing
only approved catalog IDs, approval-decision IDs, validated claim IDs, and the
normalized reference facts. Provider prompts, responses, Search results, and
URL-context dossiers are request-scoped and are never stored as part of the
reference profile.

The price object is deliberately named `direct_cited_amount_usd` and
`direct_cited_nominal_dollar_year`. It must reproduce the primary source's
nominal full-configuration MSRP; it cannot carry an inflation-adjusted amount.
There is currently no typed official dollar-normalization fact or normalization
pipeline. When that direct nominal year differs from the active model's market
year, serving reports `reference_price_dollar_normalization_missing` and leaves
the estimate unavailable.

Preview the exact publication transaction, including database triggers, with:

```sh
cargo run --bin aircost-admin -- \
  publish-aircraft-reference --draft normalized-reference.json
```

The preview rolls back after all gates pass. Publish the same draft with:

```sh
cargo run --bin aircost-admin -- \
  publish-aircraft-reference --draft normalized-reference.json --apply
```

Publication atomically creates or reuses the configuration identity, inserts a
building version and its applicability, full price, component, feature, and
fact-set completeness rows, then transitions it to `published`. A failed gate
rolls back the whole operation.

## Curves And Failure Reporting

The aircraft graph plots actual asking prices at their listing dates. Projected
curves use calendar valuation years and the selected utilization assumption.
The API exposes the selected immutable factory reference and the derived
reference valuation basis beside the model artifact information.

If the reference, nominal-dollar basis, approved avionics delta, active model,
or eligible comparable snapshot is unavailable, `estimated_value_usd` remains
empty and `estimate_error` identifies the unmet gate. No legacy estimator is
used as a compatibility fallback.
