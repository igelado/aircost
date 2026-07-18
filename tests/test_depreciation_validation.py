import json
from pathlib import Path
import tempfile
import unittest

from aircost.depreciation_validation import (
    DepreciationValidationCase,
    ValidationComponent,
    load_validation_cases,
    summarize_validation_results,
    validate_depreciation_case,
    validate_depreciation_cases,
)


class DepreciationValidationTests(unittest.TestCase):
    def test_validates_case_against_asking_price(self):
        result = validate_depreciation_case(
            DepreciationValidationCase(
                label="Example",
                model="Example 100",
                asking_price_usd=100000,
                purchase_price_new_usd=200000,
                new_price_basis_factor=1.5,
                age_years=5,
                airframe_hours=500,
                engine=ValidationComponent(
                    hours=500,
                    tbo_hours=2000,
                    overhaul_cost_usd=40000,
                ),
            )
        )

        self.assertEqual(result.label, "Example")
        self.assertGreater(result.estimated_value_usd, 0)
        self.assertEqual(result.purchase_price_new_usd, 200000)
        self.assertEqual(result.new_price_basis_factor, 1.5)
        self.assertEqual(result.effective_new_price_basis_usd, 300000)
        self.assertAlmostEqual(result.asking_to_new_fraction, 100000 / 300000)
        self.assertAlmostEqual(
            result.estimated_to_new_fraction,
            result.estimated_value_usd / 300000,
        )
        self.assertAlmostEqual(
            result.error_usd,
            result.estimated_value_usd - 100000,
        )
        self.assertAlmostEqual(
            result.estimate_to_asking_ratio,
            result.estimated_value_usd / 100000,
        )

    def test_summarizes_results(self):
        cases = [
            DepreciationValidationCase(
                label="Low",
                model="Example",
                asking_price_usd=100000,
                purchase_price_new_usd=200000,
                age_years=10,
                airframe_hours=1000,
            ),
            DepreciationValidationCase(
                label="High",
                model="Example",
                asking_price_usd=200000,
                purchase_price_new_usd=300000,
                age_years=2,
                airframe_hours=200,
            ),
        ]

        summary = summarize_validation_results(validate_depreciation_cases(cases))

        self.assertEqual(summary.count, 2)
        self.assertGreater(summary.mean_absolute_error_usd, 0)
        self.assertGreaterEqual(summary.median_absolute_error_fraction, 0)

    def test_loads_validation_cases_from_json(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "cases.json"
            path.write_text(
                json.dumps(
                    [
                        {
                            "label": "Example",
                            "model": "Example 100",
                            "asking_price_usd": 100000,
                            "purchase_price_new_usd": 200000,
                            "age_years": 5,
                            "airframe_hours": 500,
                            "engine": {
                                "hours": 500,
                                "tbo_hours": 2000,
                                "overhaul_cost_usd": 40000,
                            },
                        }
                    ]
                ),
                encoding="utf-8",
            )

            cases = load_validation_cases(path)

        self.assertEqual(len(cases), 1)
        self.assertEqual(cases[0].label, "Example")
        self.assertEqual(cases[0].engine.hours, 500)


if __name__ == "__main__":
    unittest.main()
