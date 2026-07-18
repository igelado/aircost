import unittest

from aircost.depreciation import (
    TimedComponent,
    age_residual_fraction,
    airframe_utilization_factor,
    estimate_aircraft_value,
    timed_component_adjustment,
)


class DepreciationModelTests(unittest.TestCase):
    def test_component_adjustment_uses_half_life_baseline(self):
        engine = TimedComponent(
            name="engine",
            hours_since_overhaul=0,
            tbo_hours=2000,
            overhaul_cost_usd=40000,
        )
        self.assertEqual(timed_component_adjustment(engine), 20000)

        runout = TimedComponent(
            name="engine",
            hours_since_overhaul=2500,
            tbo_hours=2000,
            overhaul_cost_usd=40000,
        )
        self.assertEqual(timed_component_adjustment(runout), -20000)

    def test_airframe_factor_matches_aopa_vref_order_of_magnitude(self):
        factor_20_percent_high = airframe_utilization_factor(
            actual_hours=120,
            expected_hours=100,
            doubling_discount=0.13,
            max_premium=0.12,
            max_discount=0.25,
        )
        factor_double = airframe_utilization_factor(
            actual_hours=200,
            expected_hours=100,
            doubling_discount=0.13,
            max_premium=0.12,
            max_discount=0.25,
        )

        self.assertAlmostEqual(factor_20_percent_high, 0.964, places=3)
        self.assertAlmostEqual(factor_double, 0.87, places=3)

    def test_estimate_combines_age_hours_and_components(self):
        estimate = estimate_aircraft_value(
            purchase_price_new_usd=500000,
            age_years=10,
            airframe_hours=1800,
            profile="light_piston",
            engine=TimedComponent(
                name="engine",
                hours_since_overhaul=1800,
                tbo_hours=2000,
                overhaul_cost_usd=42000,
            ),
            propeller=TimedComponent(
                name="propeller",
                hours_since_overhaul=300,
                tbo_hours=2400,
                overhaul_cost_usd=6000,
            ),
        )

        self.assertGreater(estimate.estimated_value_usd, 0)
        self.assertLess(estimate.estimated_value_usd, 500000)
        self.assertLess(estimate.breakdown.engine_adjustment_usd, 0)
        self.assertGreater(estimate.breakdown.propeller_adjustment_usd, 0)

    def test_stable_market_estimate_is_capped_at_effective_new_price(self):
        estimate = estimate_aircraft_value(
            purchase_price_new_usd=600000,
            age_years=0,
            airframe_hours=0,
            profile="light_piston",
            engine=TimedComponent(
                name="engine",
                hours_since_overhaul=0,
                tbo_hours=2000,
                overhaul_cost_usd=42000,
            ),
            propeller=TimedComponent(
                name="propeller",
                hours_since_overhaul=0,
                tbo_hours=2400,
                overhaul_cost_usd=6000,
            ),
        )

        self.assertEqual(estimate.estimated_value_usd, 600000)

    def test_new_to_used_discount_creates_front_loaded_depreciation(self):
        no_discount = age_residual_fraction(
            age_years=1,
            decay_rate=0.035,
            long_run_residual_fraction=0.42,
        )
        with_discount = age_residual_fraction(
            age_years=1,
            decay_rate=0.035,
            long_run_residual_fraction=0.42,
            new_to_used_discount_fraction=0.10,
            new_to_used_discount_years=1.0,
        )

        self.assertAlmostEqual(with_discount, no_discount * 0.90)


if __name__ == "__main__":
    unittest.main()
