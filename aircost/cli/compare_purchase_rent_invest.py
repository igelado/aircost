"""Compare aircraft purchase economics against renting and investing."""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path
from types import SimpleNamespace

from aircost.cli.project_aircraft_costs import _inputs_from_args as aircraft_inputs
from aircost.cli.project_investment_returns import _inputs_from_args as investment_inputs
from aircost.cli.project_rental_costs import _inputs_from_args as rental_inputs
from aircost.economics_comparison import compare_purchase_vs_rent_and_invest
from aircost.investment_returns import project_investment_returns
from aircost.annual_costs import project_yearly_costs
from aircost.rental_costs import project_yearly_rental_costs


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Compare purchasing an aircraft against renting another aircraft "
            "while investing the purchase price."
        ),
    )
    parser.add_argument("--aircraft-config", type=Path, required=True)
    parser.add_argument("--cost-config", type=Path, required=True)
    parser.add_argument("--rental-config", type=Path, required=True)
    parser.add_argument("--investment-config", type=Path, required=True)
    parser.add_argument("--annual-flight-hours", type=float, required=True)
    parser.add_argument(
        "--purchase-price",
        type=float,
        default=None,
        help="Actual aircraft purchase price. Defaults to modeled year-1 start value.",
    )
    parser.add_argument(
        "--years",
        type=int,
        default=None,
        help="Comparison horizon. Defaults to the ownership cost config years.",
    )
    parser.add_argument("--json", action="store_true")
    parser.add_argument(
        "--summary",
        action="store_true",
        help="Print one compact row per year instead of itemized yearly sections.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    ownership_rows, rental_rows, investment_rows, purchase_price = _project_inputs(args)
    rows = compare_purchase_vs_rent_and_invest(
        ownership_rows=ownership_rows,
        rental_rows=rental_rows,
        investment_rows=investment_rows,
        purchase_price_usd=purchase_price,
    )

    if args.json:
        print(json.dumps([asdict(row) for row in rows], indent=2, sort_keys=True))
    elif args.summary:
        print(_format_summary_table(rows))
    else:
        print(_format_itemized(rows))
    return 0


def _project_inputs(args):
    aircraft_ns = _aircraft_namespace(
        aircraft_config=args.aircraft_config,
        cost_config=args.cost_config,
        years=args.years,
        annual_flight_hours=args.annual_flight_hours,
    )
    initial_state, fixed_costs, hourly_costs, average_inflation_rate, years = (
        aircraft_inputs(aircraft_ns)
    )
    ownership_rows = project_yearly_costs(
        initial_state=initial_state,
        fixed_costs=fixed_costs,
        hourly_costs=hourly_costs,
        years=years,
        average_inflation_rate=average_inflation_rate,
    )
    purchase_price = (
        args.purchase_price
        if args.purchase_price is not None
        else ownership_rows[0].start_value_usd
    )

    rental_costs, rental_years, rental_inflation = rental_inputs(
        _rental_namespace(args.rental_config, years, args.annual_flight_hours),
    )
    investment, investment_years = investment_inputs(
        _investment_namespace(args.investment_config, years, purchase_price),
    )
    if rental_years != years or investment_years != years:
        raise SystemExit("comparison inputs must use the same number of years")

    rental_rows = project_yearly_rental_costs(
        rental_costs=rental_costs,
        years=years,
        average_inflation_rate=rental_inflation,
    )
    investment_rows = project_investment_returns(
        investment=investment,
        years=years,
        annual_withdrawals_usd=[row.total_cost_usd for row in rental_rows],
    )
    return ownership_rows, rental_rows, investment_rows, purchase_price


def _aircraft_namespace(
    *,
    aircraft_config: Path,
    cost_config: Path,
    years: int | None,
    annual_flight_hours: float,
) -> SimpleNamespace:
    attrs = {
        "aircraft_config": aircraft_config,
        "cost_config": cost_config,
        "years": years,
        "profile": None,
        "purchase_price_new": None,
        "new_price_basis_factor": None,
        "age_years": None,
        "airframe_hours": None,
        "engine_hours": None,
        "engine_tbo_hours": None,
        "engine_overhaul_cost": None,
        "engine_count": None,
        "engine_value_baseline_life_fraction": None,
        "propeller_hours": None,
        "propeller_tbo_hours": None,
        "propeller_overhaul_cost": None,
        "propeller_count": None,
        "propeller_value_baseline_life_fraction": None,
        "annual_flight_hours": annual_flight_hours,
        "tie_down_annual": None,
        "insurance_annual": None,
        "property_tax_annual": None,
        "property_tax_rate": None,
        "annual_inspection": None,
        "fuel_burn_gph": None,
        "fuel_price_per_gallon": None,
        "oil_quarts_per_hour": None,
        "oil_price_per_quart": None,
        "other_maintenance_per_hour": None,
        "average_inflation_rate": None,
    }
    return SimpleNamespace(**attrs)


def _rental_namespace(
    rental_config: Path,
    years: int,
    annual_flight_hours: float,
) -> SimpleNamespace:
    attrs = {
        "rental_config": rental_config,
        "years": years,
        "annual_flight_hours": annual_flight_hours,
        "insurance_annual": None,
        "club_annual": None,
        "club_monthly": None,
        "rental_rate_per_hour": None,
        "average_inflation_rate": None,
    }
    return SimpleNamespace(**attrs)


def _investment_namespace(
    investment_config: Path,
    years: int,
    purchase_price: float,
) -> SimpleNamespace:
    attrs = {
        "investment_config": investment_config,
        "years": years,
        "initial_amount": purchase_price,
        "annual_return_rate": None,
        "dividend_payments_per_year": None,
    }
    return SimpleNamespace(**attrs)


def _format_itemized(rows) -> str:
    sections = []
    for row in rows:
        sections.append(
            "\n".join(
                [
                    f"Year {row.year}",
                    f"  Purchase price invested alternative: {_money(row.purchase_price_usd)}",
                    "  Purchase option:",
                    f"    Ownership cash cost: {_money(row.ownership_cash_cost_usd)}",
                    f"    Ownership economic cost: {_money(row.ownership_economic_cost_usd)}",
                    f"    Aircraft end value: {_money(row.aircraft_end_value_usd)}",
                    f"    Net position: {_money(row.purchase_net_position_usd)}",
                    "  Rent and invest option:",
                    f"    Rental cost: {_money(row.rental_cost_usd)}",
                    "    Reinvested dividends: "
                    f"{_money(row.investment_dividends_reinvested_usd)}",
                    "    Rental withdrawal from investment: "
                    f"{_money(row.investment_withdrawal_usd)}",
                    "    Investment end balance after withdrawal: "
                    f"{_money(row.investment_end_balance_usd)}",
                    f"    Net position: {_money(row.rent_invest_net_position_usd)}",
                    "  Cumulative:",
                    "    Ownership cash costs: "
                    f"{_money(row.cumulative_ownership_cash_cost_usd)}",
                    f"    Rental costs: {_money(row.cumulative_rental_cost_usd)}",
                    "    Investment return: "
                    f"{_money(row.cumulative_investment_return_usd)}",
                    "    Investment withdrawals: "
                    f"{_money(row.cumulative_investment_withdrawals_usd)}",
                    f"    Advantage: {_advantage(row)}",
                ]
            )
        )
    return "\n\n".join(sections)


def _format_summary_table(rows) -> str:
    headers = [
        "Year",
        "Buy Net",
        "Rent+Inv Net",
        "Advantage",
        "Better",
    ]
    lines = ["  ".join(f"{header:>14}" for header in headers)]
    for row in rows:
        lines.append(
            "  ".join(
                [
                    f"{row.year:>14}",
                    f"{_money(row.purchase_net_position_usd):>14}",
                    f"{_money(row.rent_invest_net_position_usd):>14}",
                    f"{_money(abs(row.purchase_advantage_usd)):>14}",
                    f"{row.better_option:>14}",
                ]
            )
        )
    return "\n".join(lines)


def _advantage(row) -> str:
    if row.better_option == "purchase":
        return f"purchase by {_money(row.purchase_advantage_usd)}"
    if row.better_option == "rent_and_invest":
        return f"rent and invest by {_money(abs(row.purchase_advantage_usd))}"
    return "$0 tie"


def _money(value: float) -> str:
    return f"${value:,.0f}"


if __name__ == "__main__":
    raise SystemExit(main())
