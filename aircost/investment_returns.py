"""Investment return projection with municipal-bond-style reinvestment."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class InvestmentInputs:
    """Inputs for projecting reinvested investment returns."""

    initial_amount_usd: float
    annual_return_rate: float
    dividend_payments_per_year: int = 2


@dataclass(frozen=True)
class YearlyInvestmentReturn:
    year: int
    start_balance_usd: float
    dividends_reinvested_usd: float
    withdrawal_usd: float
    end_balance_usd: float
    cumulative_return_usd: float
    cumulative_withdrawals_usd: float
    cumulative_return_fraction: float


def project_investment_returns(
    *,
    investment: InvestmentInputs,
    years: int,
    annual_withdrawals_usd: list[float] | None = None,
) -> list[YearlyInvestmentReturn]:
    """Project investment value assuming every dividend payment is reinvested."""

    _validate_inputs(investment, years, annual_withdrawals_usd)
    withdrawals = (
        annual_withdrawals_usd if annual_withdrawals_usd is not None else [0.0] * years
    )
    rows: list[YearlyInvestmentReturn] = []
    balance = investment.initial_amount_usd
    cumulative_dividends = 0.0
    cumulative_withdrawals = 0.0
    periodic_rate = investment.annual_return_rate / investment.dividend_payments_per_year

    for year in range(1, years + 1):
        start_balance = balance
        dividends_reinvested = 0.0
        for _ in range(investment.dividend_payments_per_year):
            dividend = max(balance, 0.0) * periodic_rate
            balance += dividend
            dividends_reinvested += dividend

        withdrawal = withdrawals[year - 1]
        balance -= withdrawal
        cumulative_dividends += dividends_reinvested
        cumulative_withdrawals += withdrawal
        cumulative_return_fraction = (
            cumulative_dividends / investment.initial_amount_usd
            if investment.initial_amount_usd > 0
            else 0.0
        )
        rows.append(
            YearlyInvestmentReturn(
                year=year,
                start_balance_usd=start_balance,
                dividends_reinvested_usd=dividends_reinvested,
                withdrawal_usd=withdrawal,
                end_balance_usd=balance,
                cumulative_return_usd=cumulative_dividends,
                cumulative_withdrawals_usd=cumulative_withdrawals,
                cumulative_return_fraction=cumulative_return_fraction,
            )
        )

    return rows


def _validate_inputs(
    investment: InvestmentInputs,
    years: int,
    annual_withdrawals_usd: list[float] | None,
) -> None:
    if years < 1:
        raise ValueError("years must be at least 1")
    if investment.initial_amount_usd < 0:
        raise ValueError("initial_amount_usd must be non-negative")
    if investment.annual_return_rate < 0:
        raise ValueError("annual_return_rate must be non-negative")
    if investment.dividend_payments_per_year < 1:
        raise ValueError("dividend_payments_per_year must be at least 1")
    if annual_withdrawals_usd is None:
        return
    if len(annual_withdrawals_usd) != years:
        raise ValueError("annual_withdrawals_usd must have one value per year")
    for withdrawal in annual_withdrawals_usd:
        if withdrawal < 0:
            raise ValueError("annual_withdrawals_usd values must be non-negative")
