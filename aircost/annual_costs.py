"""Yearly aircraft ownership cost projection."""

from __future__ import annotations

from dataclasses import dataclass

from .depreciation import TimedComponent, estimate_aircraft_value


@dataclass(frozen=True)
class AircraftCostState:
    """Aircraft state at the beginning of a cost projection."""

    purchase_price_new_usd: float
    age_years: float
    airframe_hours: float
    engine_hours: float
    engine_tbo_hours: float
    engine_overhaul_cost_usd: float
    propeller_hours: float
    propeller_tbo_hours: float
    propeller_overhaul_cost_usd: float
    engine_count: int = 1
    propeller_count: int = 1
    engine_value_baseline_life_fraction: float = 0.5
    propeller_value_baseline_life_fraction: float = 0.5
    profile: str = "light_piston"
    new_price_basis_factor: float = 1.0


@dataclass(frozen=True)
class FixedCostInputs:
    """Annual fixed costs before depreciation."""

    tie_down_annual_usd: float = 0.0
    insurance_annual_usd: float = 0.0
    property_tax_annual_usd: float = 0.0
    property_tax_rate: float = 0.0
    annual_inspection_usd: float = 0.0


@dataclass(frozen=True)
class HourlyCostInputs:
    """Per-hour operating-cost assumptions."""

    annual_flight_hours: float
    fuel_burn_gph: float = 0.0
    fuel_price_per_gallon: float = 0.0
    oil_quarts_per_hour: float = 0.0
    oil_price_per_quart: float = 0.0
    other_maintenance_per_hour: float = 0.0


@dataclass(frozen=True)
class YearAircraftState:
    age_years: float
    airframe_hours: float
    engine_hours: float
    propeller_hours: float
    new_price_basis_factor: float


@dataclass(frozen=True)
class FixedCostBreakdown:
    tie_down_usd: float
    insurance_usd: float
    property_tax_usd: float
    annual_inspection_usd: float
    depreciation_usd: float
    total_fixed_usd: float


@dataclass(frozen=True)
class VariableCostBreakdown:
    fuel_usd: float
    oil_usd: float
    engine_overhaul_reserve_usd: float
    propeller_overhaul_reserve_usd: float
    other_maintenance_usd: float
    total_variable_usd: float


@dataclass(frozen=True)
class YearlyCost:
    year: int
    annual_flight_hours: float
    start_state: YearAircraftState
    end_state: YearAircraftState
    start_value_usd: float
    end_value_before_inflation_usd: float
    inflation_adjustment_usd: float
    end_value_usd: float
    fixed_costs: FixedCostBreakdown
    variable_costs: VariableCostBreakdown
    total_cost_usd: float
    total_cash_cost_usd: float
    cost_per_hour_usd: float
    cash_cost_per_hour_usd: float


def project_yearly_costs(
    *,
    initial_state: AircraftCostState,
    fixed_costs: FixedCostInputs,
    hourly_costs: HourlyCostInputs,
    years: int,
    average_inflation_rate: float = 0.0,
) -> list[YearlyCost]:
    """Project yearly aircraft costs from an initial state."""

    _validate_projection_inputs(initial_state, fixed_costs, hourly_costs, years)
    _validate_average_inflation_rate(average_inflation_rate)

    rows: list[YearlyCost] = []
    state = YearAircraftState(
        age_years=initial_state.age_years,
        airframe_hours=initial_state.airframe_hours,
        engine_hours=initial_state.engine_hours,
        propeller_hours=initial_state.propeller_hours,
        new_price_basis_factor=initial_state.new_price_basis_factor,
    )

    for year in range(1, years + 1):
        row = _project_one_year(
            year=year,
            state=state,
            initial_state=initial_state,
            fixed_costs=fixed_costs,
            hourly_costs=hourly_costs,
            average_inflation_rate=average_inflation_rate,
        )
        rows.append(row)
        state = row.end_state

    return rows


def _project_one_year(
    *,
    year: int,
    state: YearAircraftState,
    initial_state: AircraftCostState,
    fixed_costs: FixedCostInputs,
    hourly_costs: HourlyCostInputs,
    average_inflation_rate: float,
) -> YearlyCost:
    annual_hours = hourly_costs.annual_flight_hours
    year_index = year - 1

    start_value = _estimate_value(
        initial_state,
        state,
        year_index,
        average_inflation_rate,
    )

    end_state_before_inflation = YearAircraftState(
        age_years=state.age_years + 1.0,
        airframe_hours=state.airframe_hours + annual_hours,
        engine_hours=_advance_component_hours(
            state.engine_hours,
            annual_hours,
            initial_state.engine_tbo_hours,
        ),
        propeller_hours=_advance_component_hours(
            state.propeller_hours,
            annual_hours,
            initial_state.propeller_tbo_hours,
        ),
        new_price_basis_factor=state.new_price_basis_factor,
    )
    end_value_before_inflation = _estimate_value(
        initial_state,
        end_state_before_inflation,
        year_index,
        average_inflation_rate,
    )
    end_state = YearAircraftState(
        age_years=end_state_before_inflation.age_years,
        airframe_hours=end_state_before_inflation.airframe_hours,
        engine_hours=end_state_before_inflation.engine_hours,
        propeller_hours=end_state_before_inflation.propeller_hours,
        new_price_basis_factor=state.new_price_basis_factor
        * _inflation_multiplier(average_inflation_rate, 1),
    )
    end_value = _estimate_value(
        initial_state,
        end_state,
        year,
        average_inflation_rate,
    )
    inflation_adjustment = end_value - end_value_before_inflation
    depreciation = max(0.0, start_value - end_value_before_inflation)

    fixed = _fixed_cost_breakdown(
        fixed_costs=fixed_costs,
        year_index=year_index,
        start_value=start_value,
        end_value=end_value,
        depreciation=depreciation,
        average_inflation_rate=average_inflation_rate,
    )
    variable = _variable_cost_breakdown(
        initial_state=initial_state,
        hourly_costs=hourly_costs,
        year_index=year_index,
        average_inflation_rate=average_inflation_rate,
    )

    total_cost = fixed.total_fixed_usd + variable.total_variable_usd
    total_cash_cost = total_cost - depreciation
    cost_per_hour = _per_hour(total_cost, annual_hours)
    cash_cost_per_hour = _per_hour(total_cash_cost, annual_hours)

    return YearlyCost(
        year=year,
        annual_flight_hours=annual_hours,
        start_state=state,
        end_state=end_state,
        start_value_usd=start_value,
        end_value_before_inflation_usd=end_value_before_inflation,
        inflation_adjustment_usd=inflation_adjustment,
        end_value_usd=end_value,
        fixed_costs=fixed,
        variable_costs=variable,
        total_cost_usd=total_cost,
        total_cash_cost_usd=total_cash_cost,
        cost_per_hour_usd=cost_per_hour,
        cash_cost_per_hour_usd=cash_cost_per_hour,
    )


def _estimate_value(
    initial_state: AircraftCostState,
    state: YearAircraftState,
    year_index: int,
    average_inflation_rate: float,
) -> float:
    inflation_multiplier = _inflation_multiplier(average_inflation_rate, year_index)
    estimate = estimate_aircraft_value(
        purchase_price_new_usd=initial_state.purchase_price_new_usd,
        age_years=state.age_years,
        airframe_hours=state.airframe_hours,
        profile=initial_state.profile,
        new_price_basis_factor=state.new_price_basis_factor,
        engine=TimedComponent(
            name="engine",
            hours_since_overhaul=state.engine_hours,
            tbo_hours=initial_state.engine_tbo_hours,
            overhaul_cost_usd=initial_state.engine_overhaul_cost_usd
            * inflation_multiplier,
            count=initial_state.engine_count,
            baseline_life_fraction=initial_state.engine_value_baseline_life_fraction,
        ),
        propeller=TimedComponent(
            name="propeller",
            hours_since_overhaul=state.propeller_hours,
            tbo_hours=initial_state.propeller_tbo_hours,
            overhaul_cost_usd=initial_state.propeller_overhaul_cost_usd
            * inflation_multiplier,
            count=initial_state.propeller_count,
            baseline_life_fraction=(
                initial_state.propeller_value_baseline_life_fraction
            ),
        ),
    )
    return estimate.estimated_value_usd


def _fixed_cost_breakdown(
    *,
    fixed_costs: FixedCostInputs,
    year_index: int,
    start_value: float,
    end_value: float,
    depreciation: float,
    average_inflation_rate: float,
) -> FixedCostBreakdown:
    multiplier = _inflation_multiplier(average_inflation_rate, year_index)
    property_tax = fixed_costs.property_tax_annual_usd * multiplier
    property_tax += fixed_costs.property_tax_rate * ((start_value + end_value) / 2.0)
    tie_down = fixed_costs.tie_down_annual_usd * multiplier
    insurance = fixed_costs.insurance_annual_usd * multiplier
    annual_inspection = fixed_costs.annual_inspection_usd * multiplier
    total_fixed = tie_down + insurance + property_tax + annual_inspection + depreciation

    return FixedCostBreakdown(
        tie_down_usd=tie_down,
        insurance_usd=insurance,
        property_tax_usd=property_tax,
        annual_inspection_usd=annual_inspection,
        depreciation_usd=depreciation,
        total_fixed_usd=total_fixed,
    )


def _variable_cost_breakdown(
    *,
    initial_state: AircraftCostState,
    hourly_costs: HourlyCostInputs,
    year_index: int,
    average_inflation_rate: float,
) -> VariableCostBreakdown:
    hours = hourly_costs.annual_flight_hours
    inflation_multiplier = _inflation_multiplier(average_inflation_rate, year_index)
    fuel_price = hourly_costs.fuel_price_per_gallon * inflation_multiplier
    oil_price = hourly_costs.oil_price_per_quart * inflation_multiplier
    maintenance_rate = hourly_costs.other_maintenance_per_hour * inflation_multiplier

    fuel = hours * hourly_costs.fuel_burn_gph * fuel_price
    oil = hours * hourly_costs.oil_quarts_per_hour * oil_price
    engine_reserve = hours * (
        initial_state.engine_overhaul_cost_usd
        * initial_state.engine_count
        * inflation_multiplier
        / initial_state.engine_tbo_hours
    )
    propeller_reserve = hours * (
        initial_state.propeller_overhaul_cost_usd
        * initial_state.propeller_count
        * inflation_multiplier
        / initial_state.propeller_tbo_hours
    )
    other_maintenance = hours * maintenance_rate
    total_variable = fuel + oil + engine_reserve + propeller_reserve + other_maintenance

    return VariableCostBreakdown(
        fuel_usd=fuel,
        oil_usd=oil,
        engine_overhaul_reserve_usd=engine_reserve,
        propeller_overhaul_reserve_usd=propeller_reserve,
        other_maintenance_usd=other_maintenance,
        total_variable_usd=total_variable,
    )


def _advance_component_hours(
    current_hours: float,
    annual_hours: float,
    tbo_hours: float,
) -> float:
    next_hours = current_hours + annual_hours
    if next_hours < tbo_hours:
        return next_hours
    return next_hours % tbo_hours


def _per_hour(cost: float, hours: float) -> float:
    if hours == 0:
        return 0.0
    return cost / hours


def _inflation_multiplier(average_inflation_rate: float, year_index: int) -> float:
    return (1.0 + average_inflation_rate) ** year_index


def _validate_projection_inputs(
    initial_state: AircraftCostState,
    fixed_costs: FixedCostInputs,
    hourly_costs: HourlyCostInputs,
    years: int,
) -> None:
    if years < 1:
        raise ValueError("years must be at least 1")
    _require_non_negative("purchase_price_new_usd", initial_state.purchase_price_new_usd)
    _require_non_negative("age_years", initial_state.age_years)
    _require_non_negative("airframe_hours", initial_state.airframe_hours)
    _require_non_negative("engine_hours", initial_state.engine_hours)
    _require_positive("engine_tbo_hours", initial_state.engine_tbo_hours)
    _require_non_negative(
        "engine_overhaul_cost_usd",
        initial_state.engine_overhaul_cost_usd,
    )
    _require_non_negative("propeller_hours", initial_state.propeller_hours)
    _require_positive("propeller_tbo_hours", initial_state.propeller_tbo_hours)
    _require_non_negative(
        "propeller_overhaul_cost_usd",
        initial_state.propeller_overhaul_cost_usd,
    )
    _require_positive("new_price_basis_factor", initial_state.new_price_basis_factor)
    if initial_state.engine_count < 1:
        raise ValueError("engine_count must be at least 1")
    if initial_state.propeller_count < 1:
        raise ValueError("propeller_count must be at least 1")
    _require_fraction(
        "engine_value_baseline_life_fraction",
        initial_state.engine_value_baseline_life_fraction,
    )
    _require_fraction(
        "propeller_value_baseline_life_fraction",
        initial_state.propeller_value_baseline_life_fraction,
    )

    for name, value in (
        ("tie_down_annual_usd", fixed_costs.tie_down_annual_usd),
        ("insurance_annual_usd", fixed_costs.insurance_annual_usd),
        ("property_tax_annual_usd", fixed_costs.property_tax_annual_usd),
        ("annual_inspection_usd", fixed_costs.annual_inspection_usd),
        ("annual_flight_hours", hourly_costs.annual_flight_hours),
        ("fuel_burn_gph", hourly_costs.fuel_burn_gph),
        ("fuel_price_per_gallon", hourly_costs.fuel_price_per_gallon),
        ("oil_quarts_per_hour", hourly_costs.oil_quarts_per_hour),
        ("oil_price_per_quart", hourly_costs.oil_price_per_quart),
        ("other_maintenance_per_hour", hourly_costs.other_maintenance_per_hour),
    ):
        _require_non_negative(name, value)
    _require_non_negative("property_tax_rate", fixed_costs.property_tax_rate)


def _validate_average_inflation_rate(value: float) -> None:
    if value <= -1.0:
        raise ValueError("average_inflation_rate must be greater than -1.0")


def _require_non_negative(name: str, value: float) -> None:
    if value < 0:
        raise ValueError(f"{name} must be non-negative")


def _require_positive(name: str, value: float) -> None:
    if value <= 0:
        raise ValueError(f"{name} must be positive")


def _require_fraction(name: str, value: float) -> None:
    if not 0 <= value <= 1:
        raise ValueError(f"{name} must be in [0, 1]")
