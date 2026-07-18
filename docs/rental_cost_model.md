# Aircraft Rental Cost Model

This model projects yearly aircraft rental cost. It is intentionally simpler than the ownership model because the renter is not responsible for depreciation, fuel, oil, maintenance reserves, inspections, or hangar/tie-down unless those are built into the rental or club agreement.

## Costs

Fixed costs:

- renter insurance
- club costs

Per-hour costs:

- aircraft rental rate

The yearly calculation is:

```text
fixed_total = insurance_annual + club_annual + club_monthly * 12
rental_total = annual_flight_hours * rental_rate_per_hour
total_cost = fixed_total + rental_total
cost_per_hour = total_cost / annual_flight_hours
```

## Inflation

`average_inflation_rate` is applied to insurance, club costs, and rental rate in future years. For example, `0.03` means 3% per year.

## Example

```bash
python3 scripts/project_rental_costs.py \
  --rental-config config/rental.example.json \
  --annual-flight-hours 120
```

The default output is itemized by year. Use `--summary` for a compact table or `--json` for machine-readable output.

The same projection can be run entirely from flags:

```bash
python3 scripts/project_rental_costs.py \
  --years 5 \
  --annual-flight-hours 120 \
  --insurance-annual 2000 \
  --club-monthly 90 \
  --rental-rate-per-hour 390 \
  --average-inflation-rate 0.03
```
