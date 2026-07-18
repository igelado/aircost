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
from aircost.cli.project_aircraft_costs import main as project_costs_main


class AnnualCostProjectionTests(unittest.TestCase):
    def test_projects_fixed_variable_and_depreciation_costs(self):
        rows = project_yearly_costs(
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
            fixed_costs=FixedCostInputs(
                tie_down_annual_usd=3000,
                insurance_annual_usd=5000,
                property_tax_rate=0.01,
                annual_inspection_usd=2500,
            ),
            hourly_costs=HourlyCostInputs(
                annual_flight_hours=100,
                fuel_burn_gph=10,
                fuel_price_per_gallon=6,
                oil_quarts_per_hour=0.05,
                oil_price_per_quart=12,
                other_maintenance_per_hour=35,
            ),
            years=1,
        )

        row = rows[0]
        expected_fuel = 100 * 10 * 6
        expected_oil = 100 * 0.05 * 12
        expected_engine_reserve = 100 * 42000 / 2000
        expected_prop_reserve = 100 * 6000 / 2400
        expected_other_maintenance = 100 * 35

        self.assertEqual(row.year, 1)
        self.assertEqual(row.end_state.age_years, 11)
        self.assertEqual(row.end_state.airframe_hours, 1700)
        self.assertAlmostEqual(row.variable_costs.fuel_usd, expected_fuel)
        self.assertAlmostEqual(row.variable_costs.oil_usd, expected_oil)
        self.assertAlmostEqual(
            row.variable_costs.engine_overhaul_reserve_usd,
            expected_engine_reserve,
        )
        self.assertAlmostEqual(
            row.variable_costs.propeller_overhaul_reserve_usd,
            expected_prop_reserve,
        )
        self.assertAlmostEqual(
            row.variable_costs.other_maintenance_usd,
            expected_other_maintenance,
        )
        self.assertAlmostEqual(
            row.fixed_costs.depreciation_usd,
            max(0, row.start_value_usd - row.end_value_before_inflation_usd),
        )
        self.assertAlmostEqual(
            row.total_cost_usd,
            row.fixed_costs.total_fixed_usd + row.variable_costs.total_variable_usd,
        )
        self.assertAlmostEqual(row.cost_per_hour_usd, row.total_cost_usd / 100)

    def test_average_inflation_rate_escalates_all_cost_adjustments(self):
        rows = project_yearly_costs(
            initial_state=AircraftCostState(
                purchase_price_new_usd=500000,
                age_years=10,
                airframe_hours=1600,
                engine_hours=500,
                engine_tbo_hours=2000,
                engine_overhaul_cost_usd=40000,
                propeller_hours=500,
                propeller_tbo_hours=2000,
                propeller_overhaul_cost_usd=5000,
            ),
            fixed_costs=FixedCostInputs(
                tie_down_annual_usd=1000,
                insurance_annual_usd=2000,
                property_tax_annual_usd=3000,
                property_tax_rate=0.01,
                annual_inspection_usd=4000,
            ),
            hourly_costs=HourlyCostInputs(
                annual_flight_hours=100,
                fuel_burn_gph=10,
                fuel_price_per_gallon=5,
                oil_quarts_per_hour=1,
                oil_price_per_quart=10,
                other_maintenance_per_hour=20,
            ),
            average_inflation_rate=0.10,
            years=2,
        )

        self.assertAlmostEqual(rows[1].fixed_costs.tie_down_usd, 1100)
        self.assertAlmostEqual(rows[1].fixed_costs.insurance_usd, 2200)
        self.assertAlmostEqual(
            rows[1].fixed_costs.property_tax_usd,
            3300
            + 0.01
            * ((rows[1].start_value_usd + rows[1].end_value_usd) / 2.0),
        )
        self.assertAlmostEqual(rows[1].fixed_costs.annual_inspection_usd, 4400)
        self.assertAlmostEqual(rows[1].variable_costs.fuel_usd, 5500)
        self.assertAlmostEqual(rows[1].variable_costs.oil_usd, 1100)
        self.assertAlmostEqual(rows[1].variable_costs.other_maintenance_usd, 2200)
        self.assertAlmostEqual(
            rows[1].variable_costs.engine_overhaul_reserve_usd,
            2200,
        )
        self.assertGreater(
            rows[0].end_value_usd,
            rows[0].end_value_before_inflation_usd,
        )
        self.assertAlmostEqual(
            rows[0].fixed_costs.depreciation_usd,
            max(
                0,
                rows[0].start_value_usd
                - rows[0].end_value_before_inflation_usd,
            ),
        )

    def test_inflation_is_applied_after_depreciation(self):
        rows = project_yearly_costs(
            initial_state=AircraftCostState(
                purchase_price_new_usd=520000,
                age_years=12,
                airframe_hours=3200,
                engine_hours=900,
                engine_tbo_hours=2000,
                engine_overhaul_cost_usd=42000,
                propeller_hours=900,
                propeller_tbo_hours=2400,
                propeller_overhaul_cost_usd=6000,
            ),
            fixed_costs=FixedCostInputs(),
            hourly_costs=HourlyCostInputs(annual_flight_hours=120),
            years=1,
            average_inflation_rate=0.03,
        )

        row = rows[0]
        self.assertLess(row.end_value_before_inflation_usd, row.start_value_usd)
        self.assertGreater(row.end_value_usd, row.end_value_before_inflation_usd)
        self.assertAlmostEqual(
            row.end_value_usd,
            row.end_value_before_inflation_usd * 1.03,
        )
        self.assertAlmostEqual(
            row.fixed_costs.depreciation_usd,
            row.start_value_usd - row.end_value_before_inflation_usd,
        )

    def test_historical_purchase_price_can_inflate_above_original_nominal_price(self):
        rows = project_yearly_costs(
            initial_state=AircraftCostState(
                purchase_price_new_usd=35000,
                new_price_basis_factor=465000 / 35000,
                age_years=44,
                airframe_hours=4800,
                engine_hours=900,
                engine_tbo_hours=2000,
                engine_overhaul_cost_usd=42000,
                propeller_hours=900,
                propeller_tbo_hours=2400,
                propeller_overhaul_cost_usd=6000,
            ),
            fixed_costs=FixedCostInputs(),
            hourly_costs=HourlyCostInputs(annual_flight_hours=80),
            years=1,
        )

        self.assertGreater(rows[0].start_value_usd, 35000)
        self.assertGreater(rows[0].start_value_usd, 80000)

    def test_new_aircraft_value_can_inflate_when_depreciation_is_flat(self):
        rows = project_yearly_costs(
            initial_state=AircraftCostState(
                purchase_price_new_usd=600000,
                age_years=0,
                airframe_hours=0,
                engine_hours=0,
                engine_tbo_hours=2000,
                engine_overhaul_cost_usd=42000,
                engine_value_baseline_life_fraction=0,
                propeller_hours=0,
                propeller_tbo_hours=2400,
                propeller_overhaul_cost_usd=6000,
                propeller_value_baseline_life_fraction=0,
            ),
            fixed_costs=FixedCostInputs(),
            hourly_costs=HourlyCostInputs(annual_flight_hours=120),
            years=1,
            average_inflation_rate=0.03,
        )

        row = rows[0]
        self.assertEqual(row.start_value_usd, 600000)
        self.assertEqual(row.end_value_before_inflation_usd, row.start_value_usd)
        self.assertGreater(row.end_value_usd, row.start_value_usd)
        self.assertEqual(row.fixed_costs.depreciation_usd, 0)
        self.assertGreater(row.inflation_adjustment_usd, 0)

    def test_rejects_invalid_average_inflation_rate(self):
        with self.assertRaises(ValueError):
            project_yearly_costs(
                initial_state=AircraftCostState(
                    purchase_price_new_usd=500000,
                    age_years=10,
                    airframe_hours=1600,
                    engine_hours=500,
                    engine_tbo_hours=2000,
                    engine_overhaul_cost_usd=40000,
                    propeller_hours=500,
                    propeller_tbo_hours=2000,
                    propeller_overhaul_cost_usd=5000,
                ),
                fixed_costs=FixedCostInputs(),
                hourly_costs=HourlyCostInputs(annual_flight_hours=100),
                average_inflation_rate=-1.0,
                years=1,
            )

    def test_component_hours_roll_over_at_tbo(self):
        rows = project_yearly_costs(
            initial_state=AircraftCostState(
                purchase_price_new_usd=500000,
                age_years=10,
                airframe_hours=1600,
                engine_hours=1950,
                engine_tbo_hours=2000,
                engine_overhaul_cost_usd=40000,
                propeller_hours=1900,
                propeller_tbo_hours=2000,
                propeller_overhaul_cost_usd=5000,
            ),
            fixed_costs=FixedCostInputs(),
            hourly_costs=HourlyCostInputs(annual_flight_hours=125),
            years=1,
        )

        self.assertEqual(rows[0].end_state.engine_hours, 75)
        self.assertEqual(rows[0].end_state.propeller_hours, 25)

    def test_cli_loads_configs_and_prints_itemized_costs(self):
        with tempfile.TemporaryDirectory() as directory:
            aircraft_config = Path(directory) / "aircraft.json"
            cost_config = Path(directory) / "costs.json"
            aircraft_config.write_text(
                json.dumps(
                    {
                        "profile": "light_piston",
                        "purchase_price_new_usd": 500000,
                        "age_years": 10,
                        "airframe_hours": 1600,
                        "engine_hours": 900,
                        "engine_tbo_hours": 2000,
                        "engine_overhaul_cost_usd": 42000,
                        "propeller_hours": 500,
                        "propeller_tbo_hours": 2400,
                        "propeller_overhaul_cost_usd": 6000,
                        "insurance_annual_usd": 5000,
                        "annual_inspection_usd": 2500,
                        "fuel_burn_gph": 10,
                        "oil_quarts_per_hour": 0.05,
                        "oil_price_per_quart": 12,
                        "other_maintenance_per_hour": 35,
                    }
                ),
                encoding="utf-8",
            )
            cost_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "average_inflation_rate": 0.03,
                        "fixed_costs": {
                            "tie_down_annual_usd": 3000,
                            "property_tax_rate": 0.01,
                        },
                        "hourly_costs": {
                            "fuel_price_per_gallon": 6,
                        },
                    }
                ),
                encoding="utf-8",
            )

            output = io.StringIO()
            with redirect_stdout(output):
                exit_code = project_costs_main(
                    [
                        "--aircraft-config",
                        str(aircraft_config),
                        "--cost-config",
                        str(cost_config),
                        "--annual-flight-hours",
                        "100",
                    ]
                )

        text = output.getvalue()
        self.assertEqual(exit_code, 0)
        self.assertIn("Year 1", text)
        self.assertIn("Tie-down:", text)
        self.assertIn("Insurance:", text)
        self.assertIn("Property tax:", text)
        self.assertIn("Annual inspection:", text)
        self.assertIn("Depreciation:", text)
        self.assertIn("Fuel:", text)
        self.assertIn("Oil:", text)
        self.assertIn("Engine overhaul reserve:", text)
        self.assertIn("Propeller overhaul reserve:", text)
        self.assertIn("Other maintenance:", text)
        self.assertIn("Cash cost excluding depreciation:", text)

    def test_cli_args_override_config_values(self):
        with tempfile.TemporaryDirectory() as directory:
            aircraft_config = Path(directory) / "aircraft.json"
            cost_config = Path(directory) / "costs.json"
            aircraft_config.write_text(
                json.dumps(
                    {
                        "purchase_price_new_usd": 500000,
                        "age_years": 10,
                        "airframe_hours": 1600,
                        "engine_hours": 900,
                        "engine_tbo_hours": 2000,
                        "engine_overhaul_cost_usd": 42000,
                        "propeller_hours": 500,
                        "propeller_tbo_hours": 2400,
                        "propeller_overhaul_cost_usd": 6000,
                        "fuel_burn_gph": 10,
                        "oil_price_per_quart": 12,
                    }
                ),
                encoding="utf-8",
            )
            cost_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "average_inflation_rate": 0.03,
                        "hourly_costs": {
                            "fuel_price_per_gallon": 6,
                        },
                    }
                ),
                encoding="utf-8",
            )

            output = io.StringIO()
            with redirect_stdout(output):
                exit_code = project_costs_main(
                    [
                        "--aircraft-config",
                        str(aircraft_config),
                        "--cost-config",
                        str(cost_config),
                        "--annual-flight-hours",
                        "100",
                        "--json",
                    ]
                )

        rows = json.loads(output.getvalue())
        self.assertEqual(exit_code, 0)
        self.assertEqual(rows[0]["annual_flight_hours"], 100)
        self.assertEqual(rows[0]["end_state"]["airframe_hours"], 1700)

    def test_common_cost_config_rejects_aircraft_specific_values(self):
        with tempfile.TemporaryDirectory() as directory:
            aircraft_config = Path(directory) / "aircraft.json"
            cost_config = Path(directory) / "costs.json"
            aircraft_config.write_text(
                json.dumps(
                    {
                        "purchase_price_new_usd": 500000,
                        "age_years": 10,
                        "airframe_hours": 1600,
                        "engine_hours": 900,
                        "engine_tbo_hours": 2000,
                        "engine_overhaul_cost_usd": 42000,
                        "propeller_hours": 500,
                        "propeller_tbo_hours": 2400,
                        "propeller_overhaul_cost_usd": 6000,
                    }
                ),
                encoding="utf-8",
            )
            cost_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "fixed_costs": {"insurance_annual_usd": 5000},
                        "hourly_costs": {
                            "annual_flight_hours": 100,
                            "fuel_burn_gph": 10,
                            "oil_price_per_quart": 12,
                        },
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaises(SystemExit) as context:
                project_costs_main(
                    [
                        "--aircraft-config",
                        str(aircraft_config),
                        "--cost-config",
                        str(cost_config),
                        "--annual-flight-hours",
                        "100",
                    ]
                )

        self.assertIn("unknown key", str(context.exception))

    def test_common_cost_config_rejects_annual_flight_hours(self):
        with tempfile.TemporaryDirectory() as directory:
            aircraft_config = Path(directory) / "aircraft.json"
            cost_config = Path(directory) / "costs.json"
            aircraft_config.write_text(
                json.dumps(
                    {
                        "purchase_price_new_usd": 500000,
                        "age_years": 10,
                        "airframe_hours": 1600,
                        "engine_hours": 900,
                        "engine_tbo_hours": 2000,
                        "engine_overhaul_cost_usd": 42000,
                        "propeller_hours": 500,
                        "propeller_tbo_hours": 2400,
                        "propeller_overhaul_cost_usd": 6000,
                    }
                ),
                encoding="utf-8",
            )
            cost_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "hourly_costs": {"annual_flight_hours": 100},
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaises(SystemExit) as context:
                project_costs_main(
                    [
                        "--aircraft-config",
                        str(aircraft_config),
                        "--cost-config",
                        str(cost_config),
                        "--annual-flight-hours",
                        "100",
                    ]
                )

        self.assertIn("unknown key", str(context.exception))

    def test_common_cost_config_rejects_old_escalation_block(self):
        with tempfile.TemporaryDirectory() as directory:
            aircraft_config = Path(directory) / "aircraft.json"
            cost_config = Path(directory) / "costs.json"
            aircraft_config.write_text(
                json.dumps(
                    {
                        "purchase_price_new_usd": 500000,
                        "age_years": 10,
                        "airframe_hours": 1600,
                        "engine_hours": 900,
                        "engine_tbo_hours": 2000,
                        "engine_overhaul_cost_usd": 42000,
                        "propeller_hours": 500,
                        "propeller_tbo_hours": 2400,
                        "propeller_overhaul_cost_usd": 6000,
                    }
                ),
                encoding="utf-8",
            )
            cost_config.write_text(
                json.dumps(
                    {
                        "years": 1,
                        "hourly_costs": {},
                        "escalation": {"fuel_price_rate": 0.03},
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaises(SystemExit) as context:
                project_costs_main(
                    [
                        "--aircraft-config",
                        str(aircraft_config),
                        "--cost-config",
                        str(cost_config),
                        "--annual-flight-hours",
                        "100",
                    ]
                )

        self.assertIn("unknown key", str(context.exception))


if __name__ == "__main__":
    unittest.main()
