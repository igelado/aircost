# Purchase vs Rent and Invest Comparison

This comparison combines the ownership, rental, and investment models.

It compares two choices:

- purchase an aircraft and pay ownership cash costs
- rent another aircraft and invest the aircraft purchase price

The comparison uses a balance-sheet view:

```text
purchase_net_position =
  aircraft_end_value - cumulative_ownership_cash_costs

rent_invest_net_position =
  investment_end_balance_after_rental_withdrawals

purchase_advantage =
  purchase_net_position - rent_invest_net_position
```

If `purchase_advantage` is positive, purchasing is ahead. If it is negative, renting and investing is ahead.

In the rent-and-invest case, the invested principal starts as the aircraft purchase price. Each model year compounds the investment return first using the configured dividend schedule, then withdraws that same year's inflated rental cost from the investment balance. This means rental costs are not subtracted a second time from the final investment balance.

If withdrawals exhaust the investment balance, the balance can go negative to show the outside cash shortfall. Future investment returns are earned only on a positive balance; the model does not assume borrowing interest on a negative balance.

By default, the purchase price invested in the rent-and-invest case is the ownership model's year-1 starting aircraft value. Pass `--purchase-price` when the actual purchase price is known. The aircraft's ongoing value still comes from the ownership/depreciation model, so if the actual purchase price differs materially from the modeled year-1 aircraft value, make sure the aircraft config reflects the market value assumptions you want to compare.

The investment config should not include an initial amount. It only defines return assumptions such as `annual_return_rate` and `dividend_payments_per_year`; the comparison injects the purchase price as the invested principal.

## Example

```bash
python3 scripts/compare_purchase_rent_invest.py \
  --aircraft-config config/aircraft.example.json \
  --cost-config config/costs.example.json \
  --rental-config config/rental.example.json \
  --investment-config config/investment.example.json \
  --annual-flight-hours 120
```

Use `--summary` for a compact table or `--json` for machine-readable output.
