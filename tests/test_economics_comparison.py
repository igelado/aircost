import unittest
from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import tempfile

from aircost.annual_costs import (
    AircraftCostState,
    FixedCostInputs,
    HourlyCostInputs,
    project_yearly_costs,
)
from aircost.cli.compare_purchase_rent_invest import main as compare_main
from aircost.economics_comparison import compare_purchase_vs_rent_and_invest
from aircost.investment_returns import InvestmentInputs, project_investment_returns
from aircost.rental_costs import RentalCostInputs, project_yearly_rental_costs


class EconomicsComparisonTests(unittest.TestCase):
    def test_compares_net_positions(self):
        ownership_rows = project_yearly_costs(
            initial_state=AircraftCostState(
                purchase_price_new_usd=500000,
                age_years=10,
                airframe_hours=1600,
                engine_hours=900,
                engine_tbo_hours=2000,
                engine_overhaul_cost_usd=42000,
                propeller_hours=500,
                propeller_tbo_hours=2400,
                propeller_overhaul_cost_usd=6000,
            ),
            fixed_costs=FixedCostInputs(insurance_annual_usd=5000),
            hourly_costs=HourlyCostInputs(annual_flight_hours=100),
            years=1,
        )
        purchase_price = ownership_rows[0].start_value_usd
        rental_rows = project_yearly_rental_costs(
            rental_costs=RentalCostInputs(
                annual_flight_hours=100,
                rental_rate_per_hour=150,
            ),
            years=1,
        )
        investment_rows = project_investment_returns(
            investment=InvestmentInputs(
                initial_amount_usd=purchase_price,
                annual_return_rate=0.04,
            ),
            years=1,
            annual_withdrawals_usd=[rental_rows[0].total_cost_usd],
        )

        rows = compare_purchase_vs_rent_and_invest(
            ownership_rows=ownership_rows,
            rental_rows=rental_rows,
            investment_rows=investment_rows,
            purchase_price_usd=purchase_price,
        )

        row = rows[0]
        self.assertAlmostEqual(
            row.purchase_net_position_usd,
            ownership_rows[0].end_value_usd - ownership_rows[0].total_cash_cost_usd,
        )
        self.assertAlmostEqual(
            row.rent_invest_net_position_usd,
            investment_rows[0].end_balance_usd,
        )
        self.assertAlmostEqual(
            row.investment_withdrawal_usd,
            rental_rows[0].total_cost_usd,
        )
        self.assertAlmostEqual(
            row.cumulative_investment_withdrawals_usd,
            rental_rows[0].total_cost_usd,
        )
        self.assertAlmostEqual(
            row.purchase_advantage_usd,
            row.purchase_net_position_usd - row.rent_invest_net_position_usd,
        )

    def test_rejects_mismatched_horizons(self):
        ownership_rows = project_yearly_costs(
            initial_state=AircraftCostState(
                purchase_price_new_usd=500000,
                age_years=10,
                airframe_hours=1600,
                engine_hours=900,
                engine_tbo_hours=2000,
                engine_overhaul_cost_usd=42000,
                propeller_hours=500,
                propeller_tbo_hours=2400,
                propeller_overhaul_cost_usd=6000,
            ),
            fixed_costs=FixedCostInputs(),
            hourly_costs=HourlyCostInputs(annual_flight_hours=100),
            years=2,
        )
        rental_rows = project_yearly_rental_costs(
            rental_costs=RentalCostInputs(
                annual_flight_hours=100,
                rental_rate_per_hour=150,
            ),
            years=1,
        )
        investment_rows = project_investment_returns(
            investment=InvestmentInputs(
                initial_amount_usd=ownership_rows[0].start_value_usd,
                annual_return_rate=0.04,
            ),
            years=1,
        )

        with self.assertRaises(ValueError):
            compare_purchase_vs_rent_and_invest(
                ownership_rows=ownership_rows,
                rental_rows=rental_rows,
                investment_rows=investment_rows,
                purchase_price_usd=ownership_rows[0].start_value_usd,
            )

    def test_cli_loads_configs_and_prints_summary(self):
        output = io.StringIO()
        with redirect_stdout(output):
            exit_code = compare_main(
                [
                    "--aircraft-config",
                    "config/aircraft.example.json",
                    "--cost-config",
                    "config/costs.example.json",
                    "--rental-config",
                    "config/rental.example.json",
                    "--investment-config",
                    "config/investment.example.json",
                    "--annual-flight-hours",
                    "100",
                    "--years",
                    "1",
                    "--summary",
                ]
            )

        text = output.getvalue()
        self.assertEqual(exit_code, 0)
        self.assertIn("Buy Net", text)
        self.assertIn("Rent+Inv Net", text)
        self.assertIn("Better", text)

    def test_cli_defaults_invested_amount_to_ownership_start_value(self):
        output = io.StringIO()
        with redirect_stdout(output):
            exit_code = compare_main(
                [
                    "--aircraft-config",
                    "config/aircraft.example.json",
                    "--cost-config",
                    "config/costs.example.json",
                    "--rental-config",
                    "config/rental.example.json",
                    "--investment-config",
                    "config/investment.example.json",
                    "--annual-flight-hours",
                    "100",
                    "--years",
                    "1",
                    "--json",
                ]
            )

        rows = json.loads(output.getvalue())
        self.assertEqual(exit_code, 0)
        self.assertAlmostEqual(
            rows[0]["purchase_price_usd"],
            268928.69509110344,
        )

    def test_cli_purchase_price_override_controls_invested_amount(self):
        output = io.StringIO()
        with redirect_stdout(output):
            exit_code = compare_main(
                [
                    "--aircraft-config",
                    "config/aircraft.example.json",
                    "--cost-config",
                    "config/costs.example.json",
                    "--rental-config",
                    "config/rental.example.json",
                    "--investment-config",
                    "config/investment.example.json",
                    "--annual-flight-hours",
                    "100",
                    "--years",
                    "1",
                    "--purchase-price",
                    "90000",
                    "--json",
                ]
            )

        rows = json.loads(output.getvalue())
        self.assertEqual(exit_code, 0)
        self.assertEqual(rows[0]["purchase_price_usd"], 90000)
        self.assertAlmostEqual(rows[0]["investment_end_balance_usd"], 51556)
        self.assertAlmostEqual(rows[0]["investment_withdrawal_usd"], 42080)

    def test_cli_itemized_output_names_advantage(self):
        output = io.StringIO()
        with redirect_stdout(output):
            exit_code = compare_main(
                [
                    "--aircraft-config",
                    "config/aircraft.example.json",
                    "--cost-config",
                    "config/costs.example.json",
                    "--rental-config",
                    "config/rental.example.json",
                    "--investment-config",
                    "config/investment.example.json",
                    "--annual-flight-hours",
                    "100",
                    "--years",
                    "1",
                ]
            )

        text = output.getvalue()
        self.assertEqual(exit_code, 0)
        self.assertIn("Purchase option:", text)
        self.assertIn("Rent and invest option:", text)
        self.assertIn("Advantage:", text)


if __name__ == "__main__":
    unittest.main()
