# Aircraft Depreciation Model

This model estimates stable-market aircraft value from:

- purchase price when new
- aircraft age
- total airframe hours
- engine hours since overhaul/new
- propeller hours since overhaul/new
- installed avionics model, introduction year, and reference equipment value

It is built as a practical appraisal-style model, not a tax depreciation schedule. Tax schedules such as straight-line or MACRS can be useful for accounting, but they do not explain aircraft resale value well because market value depends heavily on make/model comparables and maintenance status.

## Selected Model

The selected model is a hybrid residual-value and half-life maintenance adjustment model:

```text
effective_new_price = purchase_price_new * new_price_basis_factor

base_age_fraction =
  residual_floor + (1 - residual_floor) * exp(-age_decay_rate * age_years)

new_to_used_discount =
  new_to_used_discount_fraction * min(age_years / new_to_used_discount_years, 1)

age_fraction = base_age_fraction * (1 - new_to_used_discount)
age_baseline_value = effective_new_price * age_fraction

expected_airframe_hours = age_years * profile_annual_airframe_hours
airframe_factor = clamp(
  (actual_airframe_hours / expected_airframe_hours) ^ log2(1 - doubling_discount),
  1 - max_discount,
  1 + max_premium
)

utilization_adjusted_value =
  age_baseline_value * airframe_factor * high_time_liquidity_factor

component_adjustment =
  count * overhaul_cost * (baseline_life_fraction - min(hours_since_overhaul / TBO, 1))

avionics_component_value =
  quantity * reference_unit_value *
  (avionics_residual_floor + (1 - avionics_residual_floor) * exp(-avionics_decay_rate * avionics_age_years))

raw_estimated_value = max(
  effective_new_price * minimum_value_fraction,
  utilization_adjusted_value + engine_adjustment + propeller_adjustment + avionics_value
)

valuation_basis = effective_new_price + avionics_replacement_basis
estimated_value = min(valuation_basis, raw_estimated_value)
```

By default, `baseline_life_fraction` is `0.5`, which means the age baseline is interpreted as an aircraft with mid-time engines and propellers. A zero-time engine adds half the overhaul cost; a run-out engine deducts half the overhaul cost; a component beyond TBO is still treated as run-out, not worse than run-out.

The cap at `effective_new_price` keeps this as a stable-market depreciation model. It prevents low-hour or zero-time component premiums from valuing an ordinary used aircraft above the current new-price basis before inflation is applied.

When avionics are included, the cap becomes `effective_new_price + avionics_replacement_basis`. That keeps the airframe model bounded while allowing an older airframe with a materially newer panel to be worth more than the same airframe with obsolete avionics.

## Avionics Components

Avionics are not treated as variant text or one-off listing text. The database stores reference metadata on each `avionics_models` row:

- `introduced_year`: first public release, certification, or common market introduction year
- `estimated_unit_value_usd`: reference equipment value for one installed working unit or integrated suite
- `value_reference_year`: dollar basis for the reference value
- `value_source`: provenance for the reference value

The Rust admin command can ask Gemini to fill missing values:

```bash
GEMINI_API_KEY=... cargo run --bin aircost-admin -- enrich-avionics --limit 10 --dry-run
GEMINI_API_KEY=... cargo run --bin aircost-admin -- enrich-avionics --limit 10 --apply
```

The prompt requires non-null `introduced_year`, `estimated_unit_value_usd`, and `confidence`. Dry-run mode should be used before applying broad updates because avionics names parsed from listings can represent anything from one remote box to a whole integrated flight deck.

The `new_to_used_discount` term can model a front-loaded resale loss when a profile's validation data supports it. It is profile-specific and may be zero. The calibrated `light_piston` profile does not force a separate first-year discount because the current asking-price sample includes low-time aircraft whose nominal asking prices are close to, or occasionally above, their current new-price basis after market inflation and scarcity are reflected.

## Why This Model

Public aircraft valuation guidance consistently points to a market baseline plus adjustments:

- Aircraft Bluebook describes average retail values as used-aircraft market values for average or mid-time aircraft, then describes engine-time adjustments against TBO with run-out deductions capped at 100% TBO.
- AOPA Aviation Finance says aircraft valuations start with year/make/model, then compare total airframe time and engine time since major overhaul to the industry average for that aircraft.
- AOPA's Vref example indicates airframe hours matter less than engine status: in one Bonanza example, 20% above average airframe time reduced value by about 3%, while double average airframe time reduced value by 13%.
- AVITAS/ISTAT definitions distinguish stable-market base value from current market value, which matches this model's baseline-versus-market-adjustment structure.
- GlobalAir/Guardian Jet transaction analysis shows depreciation rates vary materially by model family, so a single universal age curve is not defensible.

The most accurate public-domain approach for this project is therefore not a single straight-line depreciation formula. It is a profile-specific residual curve plus component reserve adjustments, with all assumptions exposed so they can later be calibrated from observed listing or transaction data.

## Built-In Profiles

| Profile | Age Decay | New-to-Used Discount | Long-Run Residual | Annual Airframe Hours | Airframe Discount at 2x Avg |
| --- | ---: | ---: | ---: | ---: | ---: |
| `light_piston` | 7.0% | 0% | 16% | 240 | 13% |
| `complex_piston` | 4.5% | 11% | 34% | 140 | 15% |
| `turboprop` | 6.0% | 12% | 20% | 300 | 18% |
| `business_jet` | 7.5% | 15% | 8% | 360 | 20% |

These are starting profiles, not immutable facts. The `light_piston` profile is calibrated against the validation dataset in this repository, which currently focuses on fixed-gear piston singles. For a specific model, calibrate `age_decay_rate`, `long_run_residual_fraction`, and `annual_airframe_hours` against real comparables.

## Calibration Path

For the next iteration, collect comparable aircraft with:

- make/model/year
- asking price and, when available, sale price
- airframe total time
- engine time since major overhaul or since new
- propeller time since overhaul
- installed avionics models, quantities, introduction years, and reference values
- paint/interior condition
- damage/logbook flags
- market date

Then fit a hedonic regression or gradient-boosted model for the market baseline and keep the half-life reserve adjustment as a transparent maintenance submodel. This gives better accuracy while preserving explainable component deductions.

## Asking-Price Validation

Run the validation harness with:

```bash
python3 scripts/validate_depreciation_model.py
```

The validation data in `data/depreciation_validation_listings.json` contains current asking-price listings with enough age, airframe-hour, and component-hour information to run the depreciation estimator. Each case also includes the new/list-price basis used by the model so the report can compare both `asking / new` and `modeled / new`. If a listing uses an original historical new price, `new_price_basis_factor` can convert that price into the validation dollar basis.

The current `light_piston` constants were fit to reduce median percentage error across the included asking-price cases while leaving large avionics, condition, and market-location outliers visible. Asking prices are not sale prices, so large residuals can come from negotiation room, avionics, build quality, damage history, international market effects, and incomplete component data.

## Sources

- Aircraft Bluebook user guide: https://aircraftbluebook.com/user-guide
- AOPA Aviation Finance, "How are Airplanes Valued?": https://finance.aopa.org/resources/2024/march/01/how-are-airplanes-valued
- AOPA Pilot, "Airframe and Powerplant": https://www.aopa.org/news-and-media/all-news/2002/january/pilot/airframe-and-powerplant
- AVITAS value definitions: https://www.avitas.com/about/value-definitions/
- GlobalAir/Guardian Jet depreciation analysis: https://www.globalair.com/articles/what-drives-business-aircraft-depreciation-and-why-each-model-is-different/12076
