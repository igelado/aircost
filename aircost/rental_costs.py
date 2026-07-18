"""Yearly aircraft rental cost projection."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class RentalCostInputs:
    """Inputs for projecting aircraft rental costs."""

    annual_flight_hours: float
    rental_rate_per_hour: float
    insurance_annual_usd: float = 0.0
    club_annual_usd: float = 0.0
    club_monthly_usd: float = 0.0


@dataclass(frozen=True)
class RentalFixedCostBreakdown:
    insurance_usd: float
    club_usd: float
    total_fixed_usd: float


@dataclass(frozen=True)
class RentalVariableCostBreakdown:
    rental_usd: float
    total_variable_usd: float


@dataclass(frozen=True)
class YearlyRentalCost:
    year: int
    annual_flight_hours: float
    fixed_costs: RentalFixedCostBreakdown
    variable_costs: RentalVariableCostBreakdown
    total_cost_usd: float
    cost_per_hour_usd: float


def project_yearly_rental_costs(
    *,
    rental_costs: RentalCostInputs,
    years: int,
    average_inflation_rate: float = 0.0,
) -> list[YearlyRentalCost]:
    """Project yearly aircraft rental costs."""

    _validate_inputs(rental_costs, years, average_inflation_rate)
    rows: list[YearlyRentalCost] = []

    for year in range(1, years + 1):
        year_index = year - 1
        multiplier = _inflation_multiplier(average_inflation_rate, year_index)
        insurance = rental_costs.insurance_annual_usd * multiplier
        club = (
            rental_costs.club_annual_usd + rental_costs.club_monthly_usd * 12.0
        ) * multiplier
        rental = (
            rental_costs.annual_flight_hours
            * rental_costs.rental_rate_per_hour
            * multiplier
        )
        fixed = RentalFixedCostBreakdown(
            insurance_usd=insurance,
            club_usd=club,
            total_fixed_usd=insurance + club,
        )
        variable = RentalVariableCostBreakdown(
            rental_usd=rental,
            total_variable_usd=rental,
        )
        total_cost = fixed.total_fixed_usd + variable.total_variable_usd

        rows.append(
            YearlyRentalCost(
                year=year,
                annual_flight_hours=rental_costs.annual_flight_hours,
                fixed_costs=fixed,
                variable_costs=variable,
                total_cost_usd=total_cost,
                cost_per_hour_usd=_per_hour(
                    total_cost,
                    rental_costs.annual_flight_hours,
                ),
            )
        )

    return rows


def _inflation_multiplier(average_inflation_rate: float, year_index: int) -> float:
    return (1.0 + average_inflation_rate) ** year_index


def _per_hour(cost: float, hours: float) -> float:
    if hours == 0:
        return 0.0
    return cost / hours


def _validate_inputs(
    rental_costs: RentalCostInputs,
    years: int,
    average_inflation_rate: float,
) -> None:
    if years < 1:
        raise ValueError("years must be at least 1")
    if average_inflation_rate <= -1.0:
        raise ValueError("average_inflation_rate must be greater than -1.0")

    for name, value in (
        ("annual_flight_hours", rental_costs.annual_flight_hours),
        ("rental_rate_per_hour", rental_costs.rental_rate_per_hour),
        ("insurance_annual_usd", rental_costs.insurance_annual_usd),
        ("club_annual_usd", rental_costs.club_annual_usd),
        ("club_monthly_usd", rental_costs.club_monthly_usd),
    ):
        if value < 0:
            raise ValueError(f"{name} must be non-negative")
