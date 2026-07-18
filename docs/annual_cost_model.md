# Annual Aircraft Cost Model

This model projects yearly ownership cost from an initial aircraft state. Each projected year advances:

- aircraft age
- total airframe hours
- engine hours since overhaul/new
- propeller hours since overhaul/new
- optional new-price basis factor

The model reports fixed costs, variable costs, depreciation, total cost, cash cost, and cost per flight hour for each year.

## Fixed Costs

Fixed costs are annual costs that do not scale directly with flight hours:

- tie-down or parking
- insurance
- property tax as either a flat annual amount, a rate applied to average market value, or both
- annual inspection
- depreciation for that specific projected year

Yearly depreciation is computed before inflation is added:

```text
end_value_before_inflation =
  value after advancing age, airframe hours, engine hours, and propeller hours
  while staying in start-of-year dollars

depreciation = max(0, start_of_year_value - end_value_before_inflation)

end_of_year_value =
  end_value_before_inflation * (1 + average_inflation_rate)
```

Start and end values come from the aircraft depreciation model in `aircost.depreciation`, using the current projected age, airframe hours, engine hours, and propeller hours. The pre-inflation end value is used for the depreciation cost. The inflated end value is used as the nominal market value carried into the next year.

Engine and propeller valuation adjustments default to a mid-life guide baseline. For a new aircraft where `purchase_price_new_usd` already includes zero-time components, set `engine_value_baseline_life_fraction` and `propeller_value_baseline_life_fraction` to `0.0`; otherwise the valuation model will add a zero-time component premium on top of the stated purchase price.

## Per-Hour Costs

Variable costs scale with projected annual flight hours:

```text
fuel = annual_hours * fuel_burn_gph * fuel_price_per_gallon
oil = annual_hours * oil_quarts_per_hour * oil_price_per_quart
engine_reserve = annual_hours * engine_overhaul_cost * engine_count / engine_tbo_hours
propeller_reserve = annual_hours * propeller_overhaul_cost * propeller_count / propeller_tbo_hours
other_maintenance = annual_hours * other_maintenance_per_hour
```

Engine and propeller overhaul costs are treated as hourly reserves. When projected component hours cross TBO, the component hour clock rolls over for the following valuation. That means the cost projection assumes overhaul reserves fund the overhaul event, rather than showing a large cash spike in the overhaul year.

## Inflation

Inflation is the annual growth rate used to project costs into future years. It is not a separate cost item. For example, if fuel is `$6.75/gal` and `average_inflation_rate` is `0.03`, year 1 uses `$6.75`, year 2 uses `$6.95`, and year 3 uses `$7.16`.

For simplicity, the projection applies one average inflation rate to:

- fixed costs
- fuel price
- oil price, starting from the aircraft-specific oil price
- other maintenance
- overhaul costs
- new-price basis used by the depreciation model

The inflation rate is a decimal rate. For example, `0.03` means 3% per year.

For aircraft bought new many years ago, `purchase_price_new_usd` can remain the actual nominal purchase price from that year if `new_price_basis_factor` converts that original dollar basis to the current valuation basis. This is why an older aircraft, such as a 1980 C172, can have a current modeled value much higher than its original 1980 nominal purchase price: the model first converts the original price into current dollars, then applies age, hour, engine, and propeller depreciation.

## Example

With configuration files:

```bash
python3 scripts/project_aircraft_costs.py \
  --aircraft-config config/aircraft.example.json \
  --cost-config config/costs.example.json \
  --annual-flight-hours 120
```

The aircraft config contains aircraft-specific starting state and aircraft-specific operating assumptions. Values belong here when they change with the aircraft rather than with the airport, market, or projection scenario:

```json
{
  "profile": "light_piston",
  "purchase_price_new_usd": 520000,
  "new_price_basis_factor": 1.0,
  "age_years": 12,
  "airframe_hours": 3200,
  "engine_hours": 900,
  "engine_tbo_hours": 2000,
  "engine_overhaul_cost_usd": 42000,
  "engine_count": 1,
  "propeller_hours": 900,
  "propeller_tbo_hours": 2400,
  "propeller_overhaul_cost_usd": 6000,
  "propeller_count": 1,
  "insurance_annual_usd": 5200,
  "annual_inspection_usd": 2800,
  "fuel_burn_gph": 10.5,
  "oil_quarts_per_hour": 0.05,
  "oil_price_per_quart": 12,
  "other_maintenance_per_hour": 45
}
```

The common costs config contains shared scenario, location, market price, and inflation assumptions. Values belong here when they can be reused across aircraft:

```json
{
  "years": 5,
  "fixed_costs": {
    "tie_down_annual_usd": 3600,
    "property_tax_annual_usd": 0,
    "property_tax_rate": 0.01
  },
  "hourly_costs": {
    "fuel_price_per_gallon": 6.75
  },
  "average_inflation_rate": 0.03
}
```

The common costs file intentionally rejects aircraft-specific fields such as `insurance_annual_usd`, `annual_inspection_usd`, `fuel_burn_gph`, `oil_quarts_per_hour`, `oil_price_per_quart`, and `other_maintenance_per_hour`. It also rejects `annual_flight_hours`; flight hours are a required command-line scenario input.

Set flight hours explicitly for each scenario:

```bash
python3 scripts/project_aircraft_costs.py \
  --aircraft-config config/aircraft.example.json \
  --cost-config config/costs.example.json \
  --annual-flight-hours 180
```

The default human output is itemized by year. Use `--summary` for a compact table or `--json` for machine-readable output.

The same projection can still be run entirely from flags:

```bash
python3 scripts/project_aircraft_costs.py \
  --years 5 \
  --profile light_piston \
  --purchase-price-new 520000 \
  --age-years 12 \
  --airframe-hours 3200 \
  --engine-hours 900 \
  --engine-tbo-hours 2000 \
  --engine-overhaul-cost 42000 \
  --propeller-hours 900 \
  --propeller-tbo-hours 2400 \
  --propeller-overhaul-cost 6000 \
  --annual-flight-hours 120 \
  --tie-down-annual 3600 \
  --insurance-annual 5200 \
  --property-tax-rate 0.01 \
  --annual-inspection 2800 \
  --fuel-burn-gph 10.5 \
  --fuel-price-per-gallon 6.75 \
  --oil-quarts-per-hour 0.05 \
  --oil-price-per-quart 12 \
  --other-maintenance-per-hour 45 \
  --average-inflation-rate 0.03
```

Use `--json` for machine-readable output.
