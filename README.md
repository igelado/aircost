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

The database-backed implementation combines listing-derived comparable and
structural models with explicit maintenance and equipment adjustments. Model
artifacts, validation evidence, and activation state are versioned in the
database.

## Running AirCost

Run the SQLx-backed Rust web app:

```bash
cargo run --bin aircost-web
```

Run administrative workflows with:

```bash
cargo run --bin aircost-admin -- --help
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
