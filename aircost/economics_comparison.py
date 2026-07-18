"""Compare aircraft ownership against renting and investing the purchase price."""

from __future__ import annotations

from dataclasses import dataclass

from .annual_costs import YearlyCost
from .investment_returns import YearlyInvestmentReturn
from .rental_costs import YearlyRentalCost


@dataclass(frozen=True)
class YearlyPurchaseRentalComparison:
    year: int
    purchase_price_usd: float
    ownership_cash_cost_usd: float
    ownership_economic_cost_usd: float
    aircraft_end_value_usd: float
    rental_cost_usd: float
    investment_dividends_reinvested_usd: float
    investment_withdrawal_usd: float
    investment_end_balance_usd: float
    cumulative_ownership_cash_cost_usd: float
    cumulative_rental_cost_usd: float
    cumulative_investment_return_usd: float
    cumulative_investment_withdrawals_usd: float
    purchase_net_position_usd: float
    rent_invest_net_position_usd: float
    purchase_advantage_usd: float
    better_option: str


def compare_purchase_vs_rent_and_invest(
    *,
    ownership_rows: list[YearlyCost],
    rental_rows: list[YearlyRentalCost],
    investment_rows: list[YearlyInvestmentReturn],
    purchase_price_usd: float,
) -> list[YearlyPurchaseRentalComparison]:
    """Compare buying against renting while investing the purchase price."""

    _validate_inputs(ownership_rows, rental_rows, investment_rows, purchase_price_usd)
    rows: list[YearlyPurchaseRentalComparison] = []
    cumulative_ownership_cash_cost = 0.0
    cumulative_rental_cost = 0.0

    for ownership, rental, investment in zip(
        ownership_rows,
        rental_rows,
        investment_rows,
        strict=True,
    ):
        cumulative_ownership_cash_cost += ownership.total_cash_cost_usd
        cumulative_rental_cost += rental.total_cost_usd
        purchase_net_position = (
            ownership.end_value_usd - cumulative_ownership_cash_cost
        )
        rent_invest_net_position = investment.end_balance_usd
        purchase_advantage = purchase_net_position - rent_invest_net_position

        rows.append(
            YearlyPurchaseRentalComparison(
                year=ownership.year,
                purchase_price_usd=purchase_price_usd,
                ownership_cash_cost_usd=ownership.total_cash_cost_usd,
                ownership_economic_cost_usd=ownership.total_cost_usd,
                aircraft_end_value_usd=ownership.end_value_usd,
                rental_cost_usd=rental.total_cost_usd,
                investment_dividends_reinvested_usd=investment.dividends_reinvested_usd,
                investment_withdrawal_usd=investment.withdrawal_usd,
                investment_end_balance_usd=investment.end_balance_usd,
                cumulative_ownership_cash_cost_usd=cumulative_ownership_cash_cost,
                cumulative_rental_cost_usd=cumulative_rental_cost,
                cumulative_investment_return_usd=investment.cumulative_return_usd,
                cumulative_investment_withdrawals_usd=(
                    investment.cumulative_withdrawals_usd
                ),
                purchase_net_position_usd=purchase_net_position,
                rent_invest_net_position_usd=rent_invest_net_position,
                purchase_advantage_usd=purchase_advantage,
                better_option=_better_option(purchase_advantage),
            )
        )

    return rows


def _validate_inputs(
    ownership_rows: list[YearlyCost],
    rental_rows: list[YearlyRentalCost],
    investment_rows: list[YearlyInvestmentReturn],
    purchase_price_usd: float,
) -> None:
    if purchase_price_usd < 0:
        raise ValueError("purchase_price_usd must be non-negative")
    lengths = {len(ownership_rows), len(rental_rows), len(investment_rows)}
    if lengths != {len(ownership_rows)}:
        raise ValueError("ownership, rental, and investment rows must have same length")
    if not ownership_rows:
        raise ValueError("at least one comparison year is required")

    cumulative_rental_cost = 0.0
    for ownership, rental, investment in zip(
        ownership_rows,
        rental_rows,
        investment_rows,
        strict=True,
    ):
        cumulative_rental_cost += rental.total_cost_usd
        if ownership.year != rental.year or ownership.year != investment.year:
            raise ValueError("ownership, rental, and investment years must align")
        if abs(investment.withdrawal_usd - rental.total_cost_usd) > 0.01:
            raise ValueError("investment withdrawals must match rental costs")
        if (
            abs(investment.cumulative_withdrawals_usd - cumulative_rental_cost)
            > 0.01
        ):
            raise ValueError(
                "cumulative investment withdrawals must match cumulative rental costs"
            )


def _better_option(purchase_advantage: float) -> str:
    if purchase_advantage > 0:
        return "purchase"
    if purchase_advantage < 0:
        return "rent_and_invest"
    return "tie"
