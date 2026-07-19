# Aircraft Value Model

This document describes the database-backed valuation model used by the Rust web
application. The older Python scripts still exist as research utilities, but new
web-app estimates are computed from the Rust code in `src/aircraft.rs`,
`src/depreciation.rs`, and `src/fit.rs`.

The model estimates asking-market value. It is not a certified appraisal and it
does not try to model tax depreciation.

## Identity Levels

Aircraft identity is split into three levels:

- Manufacturer: the aircraft maker, for example `Cessna` or `Cirrus`.
- Model: the depreciation/economic family, for example `182 SKYLANE` or `SR22`.
- Variant: the material configuration inside that family, for example `182T`,
  `T182T`, `G6`, or `G6 TURBO`.

Airframe depreciation coefficients are fitted per model when enough listing
samples exist. Variant-specific rows hold operating and component metadata
because engine, propeller, fuel burn, default avionics, and price points can
differ by variant and model year.

When a model does not have enough samples, it uses the generic database-fitted
profile built from all listings. We do not use hard-coded maker/model fallback
profiles for production estimation.

## Required Inputs

Each listing estimate needs these listing fields:

- `model_year`
- `asking_price_usd`
- `added_at`, used as the valuation date
- `airframe_hours`
- `engine_hours`
- `propeller_hours`
- installed avionics, when the listing names concrete units

Each selected model/variant also needs:

- an `aircraft_model_spec_versions` row
- a model-year price point in `aircraft_model_variant_price_points`
- engine TBO and overhaul cost, either directly on the spec row or through
  `engine_models`
- propeller TBO and overhaul cost, either directly on the spec row or through
  `propeller_models`
- `average_inflation_rate`
- default avionics for that variant/model year when listing avionics are absent

If spec metadata or the model-year price point is missing, the API returns an
`estimate_error` instead of inventing a value.

## Airframe Basis

`aircraft_model_variant_price_points` stores nominal new-purchase price points
for a specific variant and model year. The value is the price basis for that
aircraft when new, not today's replacement price.

Factory/default avionics are stored separately. Before fitting or estimating,
the code subtracts the replacement basis of the default avionics from the stored
new price point. This avoids counting factory avionics both inside the airframe
basis and again as avionics components. The airframe basis is floored at 20% of
the model-year price point to prevent bad component data from erasing the
airframe.

For current and future valuation caps, the model also computes a replacement
floor basis from the highest model-family price point available at or before the
valuation year, adjusted into the valuation year's nominal dollars.

## Inflation

The model keeps graph output in nominal dollars for each year on the X axis. A
listing observed in 2026 is compared to a 2026 nominal estimate. Historical new
price points stay in their own nominal year and are brought forward only when
the estimator needs a valuation-year dollar basis.

The default average inflation rate is currently `0.025`. It is stored per
variant spec row as `average_inflation_rate` so future enrichment or calibration
can change it without changing code.

## Airframe Formula

The core airframe residual curve is:

```text
dollar_basis_factor =
  (1 + average_inflation_rate) ^ (valuation_year - purchase_price_reference_year)

effective_new_price =
  purchase_price_new_usd * dollar_basis_factor

base_age_fraction =
  long_run_residual_fraction
  + (1 - long_run_residual_fraction) * exp(-age_decay_rate * age_years)

new_to_used_factor =
  1 - new_to_used_discount_fraction
      * min(age_years / new_to_used_discount_years, 1)

age_baseline_value =
  effective_new_price * base_age_fraction * new_to_used_factor
```

Airframe time then adjusts that baseline:

```text
expected_airframe_hours = age_years * annual_airframe_hours

airframe_factor =
  clamp(
    (actual_airframe_hours / expected_airframe_hours)
      ^ log2(1 - airframe_doubling_discount),
    1 - max_airframe_discount,
    1 + max_airframe_premium
  )

airframe_value =
  max(
    age_baseline_value * airframe_factor * high_time_liquidity_factor,
    replacement_floor_basis_usd * replacement_floor_fraction
  )
```

`annual_airframe_hours` is not stored in the aircraft profile. The current
estimate uses the default annual-hours assumption from the valuation code. The
aircraft detail graph accepts an annual-hours query parameter so users can
change future utilization scenarios without mutating model parameters.

## Engine And Propeller

Engines and propellers are timed components. The base airframe curve is treated
as a market-average aircraft with a configurable remaining-life baseline,
normally `0.5`.

```text
consumed_fraction = min(hours_since_overhaul / tbo_hours, 1)

component_adjustment =
  count
  * overhaul_cost_usd_in_valuation_year
  * (baseline_life_fraction - consumed_fraction)
```

A fresh component is a premium over the baseline. A run-out component is a
deduction. Components beyond TBO are capped at run-out, not penalized beyond
run-out.

The generic fitted model learns the shared engine and propeller
`baseline_life_fraction` from the current listing samples and writes the result
to `component_depreciation_profiles`.

## Avionics

Avionics are independent depreciating components. Each `avionics_models` row can
store:

- concrete avionics manufacturer
- concrete model or named suite
- avionics type
- introduced year
- estimated unit value
- value reference year
- value source

The value formula is:

```text
nominal_replacement_cost =
  unit_replacement_cost_usd
  * (1 + average_inflation_rate) ^ (valuation_year - value_reference_year)

residual_fraction =
  long_run_residual_fraction
  + (1 - long_run_residual_fraction)
    * exp(-age_decay_rate * avionics_age_years)

avionics_component_value =
  quantity * nominal_replacement_cost * residual_fraction
```

If the listing has concrete installed avionics, those units drive the estimate.
If listing avionics are absent or rejected as generic, the estimator falls back
to `aircraft_model_variant_default_avionics` for that variant/model year.

## Final Estimate

The final value is:

```text
raw_estimated_value =
  max(
    effective_new_price * minimum_value_fraction,
    airframe_value
      + engine_adjustment
      + propeller_adjustment
      + avionics_value
  )

valuation_basis =
  max(effective_new_price, replacement_floor_value)
  + avionics_replacement_basis

estimated_value = min(raw_estimated_value, valuation_basis)
```

The cap prevents the model from valuing a normal used aircraft above its
valuation basis while still allowing meaningful avionics upgrades to increase
value above the comparable bare-airframe value.

## Curve Generation

The aircraft tab plots actual asking prices at the listing date, not at the
aircraft build year. For each listing, the value curve spans from aircraft model
year through current listing year plus 30 years.

Past points use actual calendar years on the X axis. Future points project
airframe hours from the current listing's hours using the user-selected annual
hours slider. Engine and propeller projection currently uses the component
baseline-life convention for the curve rather than forecasting overhaul events.

## Refitting

Depreciation profiles are fitted by grid search in `src/fit.rs`.

The fitter:

- builds a global `generic:all` profile from all usable listings
- builds a `model:<aircraft_model_id>` profile for each model with enough
  samples
- minimizes mean absolute percentage error against asking price
- stores RMSE and MAE metadata in `depreciation_profile_fit_metadata`
- assigns model profiles to matching variant spec rows
- assigns the generic profile to models without enough samples
- updates generic engine and propeller baseline-life fractions

Run it with:

```bash
cargo run --bin aircost-admin -- fit-depreciation --apply
```

Adding, updating, or deleting a listing triggers a best-effort refit for the
affected model. Broader refits can be run explicitly with the admin command.
