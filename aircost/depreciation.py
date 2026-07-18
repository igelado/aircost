"""Aircraft market depreciation model.

The model estimates a stable-market aircraft value from replacement/new price,
age, utilization, and major timed-component status. It intentionally separates
the age curve from engine/propeller reserve adjustments because appraisal
guides normalize published values around average or mid-life maintenance status.
"""

from __future__ import annotations

from dataclasses import dataclass
from math import exp, log


@dataclass(frozen=True)
class AircraftProfile:
    """Assumptions for an aircraft family or use case."""

    name: str
    age_decay_rate: float
    long_run_residual_fraction: float
    new_to_used_discount_fraction: float
    new_to_used_discount_years: float
    annual_airframe_hours: float
    airframe_doubling_discount: float
    max_airframe_premium: float
    max_airframe_discount: float
    minimum_value_fraction: float
    high_time_threshold_hours: float | None = None
    high_time_discount_at_double_threshold: float = 0.0


@dataclass(frozen=True)
class TimedComponent:
    """Major component valued by time since overhaul and overhaul cost."""

    name: str
    hours_since_overhaul: float
    tbo_hours: float
    overhaul_cost_usd: float
    count: int = 1
    baseline_life_fraction: float = 0.5


@dataclass(frozen=True)
class EstimateBreakdown:
    effective_new_price_usd: float
    age_residual_fraction: float
    age_baseline_value_usd: float
    expected_airframe_hours: float
    airframe_factor: float
    high_time_factor: float
    utilization_adjusted_value_usd: float
    engine_adjustment_usd: float
    propeller_adjustment_usd: float
    minimum_value_usd: float


@dataclass(frozen=True)
class PriceEstimate:
    estimated_value_usd: float
    depreciation_usd: float
    depreciation_fraction: float
    profile: AircraftProfile
    breakdown: EstimateBreakdown


PROFILES: dict[str, AircraftProfile] = {
    "light_piston": AircraftProfile(
        name="light_piston",
        age_decay_rate=0.07,
        long_run_residual_fraction=0.16,
        new_to_used_discount_fraction=0.0,
        new_to_used_discount_years=1.0,
        annual_airframe_hours=240.0,
        airframe_doubling_discount=0.13,
        max_airframe_premium=0.12,
        max_airframe_discount=0.25,
        minimum_value_fraction=0.06,
        high_time_threshold_hours=10_000.0,
        high_time_discount_at_double_threshold=0.10,
    ),
    "complex_piston": AircraftProfile(
        name="complex_piston",
        age_decay_rate=0.045,
        long_run_residual_fraction=0.34,
        new_to_used_discount_fraction=0.11,
        new_to_used_discount_years=1.0,
        annual_airframe_hours=140.0,
        airframe_doubling_discount=0.15,
        max_airframe_premium=0.12,
        max_airframe_discount=0.28,
        minimum_value_fraction=0.06,
        high_time_threshold_hours=10_000.0,
        high_time_discount_at_double_threshold=0.12,
    ),
    "turboprop": AircraftProfile(
        name="turboprop",
        age_decay_rate=0.06,
        long_run_residual_fraction=0.20,
        new_to_used_discount_fraction=0.12,
        new_to_used_discount_years=1.0,
        annual_airframe_hours=300.0,
        airframe_doubling_discount=0.18,
        max_airframe_premium=0.10,
        max_airframe_discount=0.35,
        minimum_value_fraction=0.05,
        high_time_threshold_hours=None,
    ),
    "business_jet": AircraftProfile(
        name="business_jet",
        age_decay_rate=0.075,
        long_run_residual_fraction=0.08,
        new_to_used_discount_fraction=0.15,
        new_to_used_discount_years=1.0,
        annual_airframe_hours=360.0,
        airframe_doubling_discount=0.20,
        max_airframe_premium=0.08,
        max_airframe_discount=0.40,
        minimum_value_fraction=0.04,
        high_time_threshold_hours=None,
    ),
}


def get_profile(name: str) -> AircraftProfile:
    """Return a built-in aircraft profile."""

    try:
        return PROFILES[name]
    except KeyError as exc:
        available = ", ".join(sorted(PROFILES))
        raise ValueError(f"unknown profile {name!r}; available profiles: {available}") from exc


def estimate_aircraft_value(
    *,
    purchase_price_new_usd: float,
    age_years: float,
    airframe_hours: float,
    profile: AircraftProfile | str = "light_piston",
    engine: TimedComponent | None = None,
    propeller: TimedComponent | None = None,
    new_price_basis_factor: float = 1.0,
) -> PriceEstimate:
    """Estimate current stable-market aircraft value.

    ``purchase_price_new_usd`` should be in current replacement dollars. If only
    the historical nominal new price is known, pass a CPI/replacement-cost factor
    via ``new_price_basis_factor``.
    """

    if isinstance(profile, str):
        profile = get_profile(profile)

    _require_non_negative("purchase_price_new_usd", purchase_price_new_usd)
    _require_non_negative("age_years", age_years)
    _require_non_negative("airframe_hours", airframe_hours)
    _require_positive("new_price_basis_factor", new_price_basis_factor)

    effective_new_price = purchase_price_new_usd * new_price_basis_factor
    age_fraction = age_residual_fraction(
        age_years=age_years,
        decay_rate=profile.age_decay_rate,
        long_run_residual_fraction=profile.long_run_residual_fraction,
        new_to_used_discount_fraction=profile.new_to_used_discount_fraction,
        new_to_used_discount_years=profile.new_to_used_discount_years,
    )
    age_baseline = effective_new_price * age_fraction

    expected_hours = expected_airframe_hours(age_years, profile)
    airframe_factor = airframe_utilization_factor(
        actual_hours=airframe_hours,
        expected_hours=expected_hours,
        doubling_discount=profile.airframe_doubling_discount,
        max_premium=profile.max_airframe_premium,
        max_discount=profile.max_airframe_discount,
    )
    high_time_factor = high_time_liquidity_factor(airframe_hours, profile)
    utilization_adjusted = age_baseline * airframe_factor * high_time_factor

    engine_adjustment = timed_component_adjustment(engine)
    propeller_adjustment = timed_component_adjustment(propeller)
    minimum_value = effective_new_price * profile.minimum_value_fraction
    raw_estimated_value = max(
        minimum_value,
        utilization_adjusted + engine_adjustment + propeller_adjustment,
    )
    estimated_value = min(effective_new_price, raw_estimated_value)

    depreciation = max(0.0, effective_new_price - estimated_value)
    depreciation_fraction = (
        depreciation / effective_new_price if effective_new_price > 0 else 0.0
    )

    return PriceEstimate(
        estimated_value_usd=estimated_value,
        depreciation_usd=depreciation,
        depreciation_fraction=depreciation_fraction,
        profile=profile,
        breakdown=EstimateBreakdown(
            effective_new_price_usd=effective_new_price,
            age_residual_fraction=age_fraction,
            age_baseline_value_usd=age_baseline,
            expected_airframe_hours=expected_hours,
            airframe_factor=airframe_factor,
            high_time_factor=high_time_factor,
            utilization_adjusted_value_usd=utilization_adjusted,
            engine_adjustment_usd=engine_adjustment,
            propeller_adjustment_usd=propeller_adjustment,
            minimum_value_usd=minimum_value,
        ),
    )


def age_residual_fraction(
    *,
    age_years: float,
    decay_rate: float,
    long_run_residual_fraction: float,
    new_to_used_discount_fraction: float = 0.0,
    new_to_used_discount_years: float = 1.0,
) -> float:
    """Residual fraction from a bounded exponential age curve."""

    _require_non_negative("age_years", age_years)
    _require_positive("decay_rate", decay_rate)
    if not 0 <= long_run_residual_fraction < 1:
        raise ValueError("long_run_residual_fraction must be in [0, 1)")
    if not 0 <= new_to_used_discount_fraction < 1:
        raise ValueError("new_to_used_discount_fraction must be in [0, 1)")
    _require_positive("new_to_used_discount_years", new_to_used_discount_years)

    base_fraction = long_run_residual_fraction + (
        1.0 - long_run_residual_fraction
    ) * exp(-decay_rate * age_years)
    discount_progress = min(age_years / new_to_used_discount_years, 1.0)
    new_to_used_factor = 1.0 - new_to_used_discount_fraction * discount_progress
    return base_fraction * new_to_used_factor


def expected_airframe_hours(age_years: float, profile: AircraftProfile) -> float:
    """Expected fleet-average airframe hours for a given age."""

    if age_years <= 0:
        return 0.0
    return age_years * profile.annual_airframe_hours


def airframe_utilization_factor(
    *,
    actual_hours: float,
    expected_hours: float,
    doubling_discount: float,
    max_premium: float,
    max_discount: float,
) -> float:
    """Value multiplier for total airframe time versus expected fleet average."""

    _require_non_negative("actual_hours", actual_hours)
    _require_non_negative("expected_hours", expected_hours)
    if not 0 <= doubling_discount < 1:
        raise ValueError("doubling_discount must be in [0, 1)")
    if not 0 <= max_premium < 1:
        raise ValueError("max_premium must be in [0, 1)")
    if not 0 <= max_discount < 1:
        raise ValueError("max_discount must be in [0, 1)")
    if expected_hours <= 0:
        return 1.0

    ratio = max(actual_hours, 1.0) / expected_hours
    exponent = log(1.0 - doubling_discount) / log(2.0)
    raw_factor = ratio**exponent
    return min(1.0 + max_premium, max(1.0 - max_discount, raw_factor))


def high_time_liquidity_factor(
    airframe_hours: float,
    profile: AircraftProfile,
) -> float:
    """Additional discount when hours cross a known financing/liquidity threshold."""

    threshold = profile.high_time_threshold_hours
    if threshold is None or airframe_hours <= threshold:
        return 1.0
    if threshold <= 0:
        raise ValueError("high_time_threshold_hours must be positive")

    capped_ratio = min(2.0, airframe_hours / threshold)
    severity = capped_ratio - 1.0
    discount = profile.high_time_discount_at_double_threshold * severity**1.2
    return max(0.0, 1.0 - discount)


def timed_component_adjustment(component: TimedComponent | None) -> float:
    """Return value add/deduct versus a mid-life component baseline."""

    if component is None:
        return 0.0
    _require_non_negative(f"{component.name}.hours_since_overhaul", component.hours_since_overhaul)
    _require_positive(f"{component.name}.tbo_hours", component.tbo_hours)
    _require_non_negative(f"{component.name}.overhaul_cost_usd", component.overhaul_cost_usd)
    if component.count < 1:
        raise ValueError(f"{component.name}.count must be at least 1")
    if not 0 <= component.baseline_life_fraction <= 1:
        raise ValueError(f"{component.name}.baseline_life_fraction must be in [0, 1]")

    consumed_fraction = min(component.hours_since_overhaul / component.tbo_hours, 1.0)
    per_component_adjustment = (
        component.baseline_life_fraction - consumed_fraction
    ) * component.overhaul_cost_usd
    return per_component_adjustment * component.count


def _require_non_negative(name: str, value: float) -> None:
    if value < 0:
        raise ValueError(f"{name} must be non-negative")


def _require_positive(name: str, value: float) -> None:
    if value <= 0:
        raise ValueError(f"{name} must be positive")
