# aircost

Tools for modeling aircraft depreciation and ownership costs.

This first pass implements a market-value estimator from the aircraft's new purchase price, age, airframe hours, engine time, propeller time, and installed avionics. It is not a certified appraisal. Real appraisals need make/model/year comparable sales, logs, damage history, paint/interior, maintenance programs, and local market conditions.

## Model

The estimator uses a hybrid market-residual and maintenance-status model:

1. Convert the purchase price when new into the valuation currency basis. If the original purchase price is historical nominal dollars, pass an inflation or replacement-cost factor with `--new-price-basis-factor`.
2. Estimate an age-based baseline residual value using a profile-specific exponential decay curve with a long-run residual floor.
3. Adjust that baseline for airframe hours relative to expected fleet utilization. The default piston-aircraft curve is calibrated to the public AOPA/Vref example where 20% above average airframe time reduced value by about 3%, and double average time reduced value by about 13%.
4. Adjust engine and propeller value relative to the half-life convention used by aircraft price guides: a mid-time component is neutral, fresh overhaul adds value, run-out deducts value, and deduction is capped at run-out.
5. Add installed avionics as separate depreciating components. Each avionics model stores an introduction year and reference equipment value, then depreciates on its own electronics curve so a newer panel can lift the value of an older airframe.

The most accurate version of this model would use model-specific market baselines from Aircraft Bluebook, VREF, aircraft listing/sale data, or an appraiser's database. Without those comparables, this script exposes the assumptions as inputs so the curve can be calibrated per aircraft family.

## Example

```bash
python3 -m aircost.cli.estimate_aircraft_price \
  --profile light_piston \
  --purchase-price-new 520000 \
  --age-years 12 \
  --airframe-hours 3200 \
  --engine-hours 900 \
  --engine-tbo-hours 2000 \
  --engine-overhaul-cost 42000 \
  --propeller-hours 900 \
  --propeller-tbo-hours 2400 \
  --propeller-overhaul-cost 6000
```

Use `--json` for machine-readable output.

Validate the depreciation model against current asking-price listings:

```bash
python3 scripts/validate_depreciation_model.py
```

Validation data lives in `data/depreciation_validation_listings.json`. These are asking prices, not confirmed sale prices, so treat the output as a calibration diagnostic rather than appraisal accuracy. The current validation set covers Sling TSi, Cessna 172, Piper Archer, and Diamond DA40 listings from multiple marketplaces.

## Annual Cost Projection

Project yearly fixed costs, hourly costs, and depreciation:

```bash
python3 scripts/project_aircraft_costs.py \
  --aircraft-config config/aircraft.example.json \
  --cost-config config/costs.example.json \
  --annual-flight-hours 120
```

The normal output is itemized by year. Use `--summary` for a compact table and `--json` for machine-readable output. Details are in [docs/annual_cost_model.md](docs/annual_cost_model.md).

Aircraft-specific costs and operating assumptions, such as insurance, annual inspection, fuel burn, oil type/price, oil consumption, and maintenance rate, live in `config/aircraft.example.json`. Shared scenario/location costs, such as tie-down, property tax rate, fuel price, and average inflation rate, live in `config/costs.example.json`. Annual flight hours are a required command-line input so each scenario is explicit.

## Rental Cost Projection

Project yearly rental costs:

```bash
python3 scripts/project_rental_costs.py \
  --rental-config config/rental.example.json \
  --annual-flight-hours 120
```

Rental analysis includes fixed insurance and club costs, plus a rental rate per flight hour. Details are in [docs/rental_cost_model.md](docs/rental_cost_model.md).

## Investment Return Projection

Project how a given amount of money grows at a given return rate with reinvested semiannual payments:

```bash
python3 scripts/project_investment_returns.py \
  --investment-config config/investment.example.json \
  --initial-amount 90000
```

Investment analysis does not apply inflation. The investment config contains return assumptions only; standalone runs pass the invested principal with `--initial-amount`, and the purchase-vs-rent comparison uses the aircraft purchase price. It assumes municipal-bond-style semiannual dividend/coupon reinvestment by default. Details are in [docs/investment_return_model.md](docs/investment_return_model.md).

## Purchase vs Rent and Invest

Compare buying an aircraft against renting another aircraft while investing the purchase price:

```bash
python3 scripts/compare_purchase_rent_invest.py \
  --aircraft-config config/aircraft.example.json \
  --cost-config config/costs.example.json \
  --rental-config config/rental.example.json \
  --investment-config config/investment.example.json \
  --annual-flight-hours 120
```

The comparison reports the net position of each strategy over time. In the rent-and-invest case, yearly rental costs are withdrawn from the invested purchase price. Details are in [docs/purchase_vs_rent_invest.md](docs/purchase_vs_rent_invest.md).

## Web Application

Run the SQLx-backed Rust web app:

```bash
cargo run --bin aircost-web
```

The server uses axum, tokio, eoka, reqwest, and sqlx. It exposes listing
preview, listing CRUD, and Chrome extension submission endpoints. Details are in
[docs/webapp.md](docs/webapp.md).

The current database-backed aircraft value model is documented in
[docs/depreciation_model.md](docs/depreciation_model.md). The schema and
listing/plugin write lifecycle are documented in [docs/database.md](docs/database.md).
Gemini extraction, grounding, validation, and correction rules are documented in
[docs/llm_usage.md](docs/llm_usage.md).

Avionics metadata enrichment uses Gemini to fill missing avionics introduction
years and equipment values:

```bash
GEMINI_API_KEY=... cargo run --bin aircost-admin -- enrich-avionics --limit 10 --dry-run
GEMINI_API_KEY=... cargo run --bin aircost-admin -- enrich-avionics --limit 10 --apply
```

## Research Basis

- Aircraft Bluebook explains that published average retail values are market values for average/mid-time used aircraft and that engine-time adjustments are based on TBO, with run-out deductions capped at 100% TBO: https://aircraftbluebook.com/user-guide
- AOPA Aviation Finance describes lender valuation inputs as year/make/model, total airframe time, engine time since major overhaul, comparable sales, avionics, and condition: https://finance.aopa.org/resources/2024/march/01/how-are-airplanes-valued
- AOPA's Vref example shows airframe hours have a smaller effect than engine hours, with double average airframe time reducing value by 13% in that example: https://www.aopa.org/news-and-media/all-news/2002/january/pilot/airframe-and-powerplant
- AVITAS/ISTAT definitions distinguish stable-market base value from current market value and describe maintenance-adjusted full-life values: https://www.avitas.com/about/value-definitions/
- GlobalAir/Guardian Jet transaction analysis shows depreciation differs materially by model family, supporting profile-specific calibration rather than a single universal curve: https://www.globalair.com/articles/what-drives-business-aircraft-depreciation-and-why-each-model-is-different/12076
