import unittest
from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import tempfile

from aircost.cli.project_rental_costs import main as project_rental_main
from aircost.rental_costs import RentalCostInputs, project_yearly_rental_costs


class RentalCostProjectionTests(unittest.TestCase):
    def test_projects_fixed_rental_and_total_costs(self):
        rows = project_yearly_rental_costs(
            rental_costs=RentalCostInputs(
                annual_flight_hours=100,
                insurance_annual_usd=500,
                club_annual_usd=900,
                club_monthly_usd=50,
                rental_rate_per_hour=160,
            ),
            years=1,
        )

        row = rows[0]
        self.assertEqual(row.year, 1)
        self.assertEqual(row.fixed_costs.insurance_usd, 500)
        self.assertEqual(row.fixed_costs.club_usd, 1500)
        self.assertEqual(row.fixed_costs.total_fixed_usd, 2000)
        self.assertEqual(row.variable_costs.rental_usd, 16000)
        self.assertEqual(row.variable_costs.total_variable_usd, 16000)
        self.assertEqual(row.total_cost_usd, 18000)
        self.assertEqual(row.cost_per_hour_usd, 180)

    def test_monthly_club_fees_are_annualized(self):
        rows = project_yearly_rental_costs(
            rental_costs=RentalCostInputs(
                annual_flight_hours=120,
                insurance_annual_usd=2000,
                club_monthly_usd=90,
                rental_rate_per_hour=390,
            ),
            years=1,
        )

        self.assertEqual(rows[0].fixed_costs.insurance_usd, 2000)
        self.assertEqual(rows[0].fixed_costs.club_usd, 1080)
        self.assertEqual(rows[0].fixed_costs.total_fixed_usd, 3080)
        self.assertEqual(rows[0].variable_costs.rental_usd, 46800)
        self.assertEqual(rows[0].total_cost_usd, 49880)

    def test_average_inflation_rate_applies_to_future_years(self):
        rows = project_yearly_rental_costs(
            rental_costs=RentalCostInputs(
                annual_flight_hours=100,
                insurance_annual_usd=500,
                club_annual_usd=900,
                rental_rate_per_hour=160,
            ),
            years=2,
            average_inflation_rate=0.10,
        )

        self.assertEqual(rows[0].total_cost_usd, 17400)
        self.assertAlmostEqual(rows[1].fixed_costs.insurance_usd, 550)
        self.assertAlmostEqual(rows[1].fixed_costs.club_usd, 990)
        self.assertAlmostEqual(rows[1].variable_costs.rental_usd, 17600)
        self.assertAlmostEqual(rows[1].total_cost_usd, 19140)

    def test_rejects_invalid_values(self):
        with self.assertRaises(ValueError):
            project_yearly_rental_costs(
                rental_costs=RentalCostInputs(
                    annual_flight_hours=100,
                    rental_rate_per_hour=160,
                ),
                years=0,
            )

        with self.assertRaises(ValueError):
            project_yearly_rental_costs(
                rental_costs=RentalCostInputs(
                    annual_flight_hours=100,
                    rental_rate_per_hour=160,
                ),
                years=1,
                average_inflation_rate=-1.0,
            )

    def test_cli_loads_config_and_prints_itemized_costs(self):
        with tempfile.TemporaryDirectory() as directory:
            rental_config = Path(directory) / "rental.json"
            rental_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "insurance_annual_usd": 500,
                        "club_monthly_usd": 75,
                        "rental_rate_per_hour": 160,
                        "average_inflation_rate": 0.03,
                    }
                ),
                encoding="utf-8",
            )

            output = io.StringIO()
            with redirect_stdout(output):
                exit_code = project_rental_main(
                    [
                        "--rental-config",
                        str(rental_config),
                        "--annual-flight-hours",
                        "100",
                    ]
                )

        text = output.getvalue()
        self.assertEqual(exit_code, 0)
        self.assertIn("Year 1", text)
        self.assertIn("Insurance:", text)
        self.assertIn("Club costs:", text)
        self.assertIn("Rental:", text)
        self.assertIn("Cost per flight hour:", text)

    def test_cli_args_override_config_values(self):
        with tempfile.TemporaryDirectory() as directory:
            rental_config = Path(directory) / "rental.json"
            rental_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "rental_rate_per_hour": 160,
                    }
                ),
                encoding="utf-8",
            )

            output = io.StringIO()
            with redirect_stdout(output):
                exit_code = project_rental_main(
                    [
                        "--rental-config",
                        str(rental_config),
                        "--annual-flight-hours",
                        "100",
                        "--json",
                    ]
                )

        rows = json.loads(output.getvalue())
        self.assertEqual(exit_code, 0)
        self.assertEqual(rows[0]["annual_flight_hours"], 100)
        self.assertEqual(rows[0]["variable_costs"]["rental_usd"], 16000)

    def test_cli_rejects_unknown_config_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            rental_config = Path(directory) / "rental.json"
            rental_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "rental_rate_per_hour": 160,
                        "fuel_price_per_gallon": 6,
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaises(SystemExit) as context:
                project_rental_main(
                    [
                        "--rental-config",
                        str(rental_config),
                        "--annual-flight-hours",
                        "100",
                    ]
                )

        self.assertIn("unknown key", str(context.exception))

    def test_cli_rejects_annual_flight_hours_in_config(self):
        with tempfile.TemporaryDirectory() as directory:
            rental_config = Path(directory) / "rental.json"
            rental_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "annual_flight_hours": 100,
                        "rental_rate_per_hour": 160,
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaises(SystemExit) as context:
                project_rental_main(
                    [
                        "--rental-config",
                        str(rental_config),
                        "--annual-flight-hours",
                        "100",
                    ]
                )

        self.assertIn("unknown key", str(context.exception))


if __name__ == "__main__":
    unittest.main()
