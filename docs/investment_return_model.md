# Investment Return Model

This model projects how an amount of money grows at a given annual return rate. It does not apply inflation. It assumes dividend or coupon payments are reinvested immediately.

The investment config contains return assumptions only. The invested principal is not part of that config; in the purchase-vs-rent comparison it comes from the aircraft purchase price, and in the standalone investment script it is passed as `--initial-amount`.

Municipal bonds commonly pay interest semiannually, so the default schedule is two dividend payments per year:

```text
periodic_rate = annual_return_rate / dividend_payments_per_year
for each dividend payment:
  dividend = current_balance * periodic_rate
  current_balance += dividend
```

With the default semiannual schedule, a 4% annual return compounds as two 2% reinvested payments per year.

Programmatic callers can pass an annual withdrawal schedule. Withdrawals are applied after that year's reinvested dividend payments. If withdrawals drive the balance below zero, later returns accrue only on a positive balance.

## Example

```bash
python3 scripts/project_investment_returns.py \
  --investment-config config/investment.example.json \
  --initial-amount 90000
```

The default output is itemized by year. Use `--summary` for a compact table or `--json` for machine-readable output.

The same projection can be run entirely from flags:

```bash
python3 scripts/project_investment_returns.py \
  --years 30 \
  --initial-amount 90000 \
  --annual-return-rate 0.04
```
