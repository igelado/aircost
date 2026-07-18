import unittest
from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import tempfile

from aircost.cli.project_investment_returns import main as project_investment_main
from aircost.investment_returns import (
    InvestmentInputs,
    project_investment_returns,
)


class InvestmentReturnProjectionTests(unittest.TestCase):
    def test_projects_semiannual_reinvestment(self):
        rows = project_investment_returns(
            investment=InvestmentInputs(
                initial_amount_usd=10000,
                annual_return_rate=0.04,
            ),
            years=1,
        )

        row = rows[0]
        self.assertEqual(row.year, 1)
        self.assertEqual(row.start_balance_usd, 10000)
        self.assertAlmostEqual(row.dividends_reinvested_usd, 404)
        self.assertAlmostEqual(row.end_balance_usd, 10404)
        self.assertAlmostEqual(row.cumulative_return_usd, 404)
        self.assertAlmostEqual(row.cumulative_return_fraction, 0.0404)

    def test_compounds_across_years(self):
        rows = project_investment_returns(
            investment=InvestmentInputs(
                initial_amount_usd=10000,
                annual_return_rate=0.04,
            ),
            years=2,
        )

        self.assertAlmostEqual(rows[1].start_balance_usd, 10404)
        self.assertAlmostEqual(rows[1].end_balance_usd, 10824.3216)

    def test_applies_withdrawals_after_reinvested_dividends(self):
        rows = project_investment_returns(
            investment=InvestmentInputs(
                initial_amount_usd=10000,
                annual_return_rate=0.04,
            ),
            years=2,
            annual_withdrawals_usd=[1000, 1000],
        )

        self.assertAlmostEqual(rows[0].dividends_reinvested_usd, 404)
        self.assertEqual(rows[0].withdrawal_usd, 1000)
        self.assertAlmostEqual(rows[0].end_balance_usd, 9404)
        self.assertEqual(rows[0].cumulative_withdrawals_usd, 1000)
        self.assertAlmostEqual(rows[0].cumulative_return_usd, 404)
        self.assertAlmostEqual(rows[1].start_balance_usd, 9404)
        self.assertAlmostEqual(rows[1].end_balance_usd, 8783.9216)
        self.assertAlmostEqual(rows[1].cumulative_return_usd, 783.9216)

    def test_withdrawn_down_balance_does_not_earn_negative_return(self):
        rows = project_investment_returns(
            investment=InvestmentInputs(
                initial_amount_usd=10000,
                annual_return_rate=0.04,
            ),
            years=2,
            annual_withdrawals_usd=[12000, 1000],
        )

        self.assertAlmostEqual(rows[0].end_balance_usd, -1596)
        self.assertEqual(rows[1].dividends_reinvested_usd, 0)
        self.assertAlmostEqual(rows[1].end_balance_usd, -2596)
        self.assertAlmostEqual(rows[1].cumulative_return_usd, 404)

    def test_rejects_invalid_values(self):
        with self.assertRaises(ValueError):
            project_investment_returns(
                investment=InvestmentInputs(
                    initial_amount_usd=10000,
                    annual_return_rate=0.04,
                ),
                years=0,
            )

        with self.assertRaises(ValueError):
            project_investment_returns(
                investment=InvestmentInputs(
                    initial_amount_usd=10000,
                    annual_return_rate=-0.01,
                ),
                years=1,
            )

        with self.assertRaises(ValueError):
            project_investment_returns(
                investment=InvestmentInputs(
                    initial_amount_usd=10000,
                    annual_return_rate=0.04,
                ),
                years=1,
                annual_withdrawals_usd=[],
            )

        with self.assertRaises(ValueError):
            project_investment_returns(
                investment=InvestmentInputs(
                    initial_amount_usd=10000,
                    annual_return_rate=0.04,
                ),
                years=1,
                annual_withdrawals_usd=[-1],
            )

        with self.assertRaises(ValueError):
            project_investment_returns(
                investment=InvestmentInputs(
                    initial_amount_usd=10000,
                    annual_return_rate=0.04,
                    dividend_payments_per_year=0,
                ),
                years=1,
            )

    def test_cli_loads_config_and_prints_itemized_returns(self):
        with tempfile.TemporaryDirectory() as directory:
            investment_config = Path(directory) / "investment.json"
            investment_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "annual_return_rate": 0.04,
                        "dividend_payments_per_year": 2,
                    }
                ),
                encoding="utf-8",
            )

            output = io.StringIO()
            with redirect_stdout(output):
                exit_code = project_investment_main(
                    [
                        "--investment-config",
                        str(investment_config),
                        "--initial-amount",
                        "10000",
                    ]
                )

        text = output.getvalue()
        self.assertEqual(exit_code, 0)
        self.assertIn("Year 1", text)
        self.assertIn("Start balance:", text)
        self.assertIn("Reinvested dividends:", text)
        self.assertIn("End balance:", text)
        self.assertIn("Cumulative return:", text)

    def test_cli_args_override_config_values(self):
        with tempfile.TemporaryDirectory() as directory:
            investment_config = Path(directory) / "investment.json"
            investment_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "annual_return_rate": 0.04,
                    }
                ),
                encoding="utf-8",
            )

            output = io.StringIO()
            with redirect_stdout(output):
                exit_code = project_investment_main(
                    [
                        "--investment-config",
                        str(investment_config),
                        "--initial-amount",
                        "20000",
                        "--json",
                    ]
                )

        rows = json.loads(output.getvalue())
        self.assertEqual(exit_code, 0)
        self.assertEqual(rows[0]["start_balance_usd"], 20000)
        self.assertAlmostEqual(rows[0]["end_balance_usd"], 20808)

    def test_cli_rejects_unknown_config_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            investment_config = Path(directory) / "investment.json"
            investment_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "initial_amount_usd": 10000,
                        "annual_return_rate": 0.04,
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaises(SystemExit) as context:
                project_investment_main(
                    ["--investment-config", str(investment_config)]
                )

        self.assertIn("unknown key", str(context.exception))

    def test_cli_requires_initial_amount_outside_config(self):
        with tempfile.TemporaryDirectory() as directory:
            investment_config = Path(directory) / "investment.json"
            investment_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "annual_return_rate": 0.04,
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaises(SystemExit) as context:
                project_investment_main(
                    ["--investment-config", str(investment_config)]
                )

        self.assertIn("--initial-amount", str(context.exception))


if __name__ == "__main__":
    unittest.main()
