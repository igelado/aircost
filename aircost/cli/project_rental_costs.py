"""Project yearly aircraft rental costs."""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

from aircost.rental_costs import RentalCostInputs, project_yearly_rental_costs


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Project yearly aircraft rental fixed costs and rental-rate costs.",
    )
    parser.add_argument(
        "--rental-config",
        type=Path,
        help="JSON file with rental cost assumptions.",
    )
    parser.add_argument("--years", type=int, default=None)
    parser.add_argument("--annual-flight-hours", type=float, required=True)
    parser.add_argument("--insurance-annual", type=float, default=None)
    parser.add_argument("--club-annual", type=float, default=None)
    parser.add_argument("--club-monthly", type=float, default=None)
    parser.add_argument("--rental-rate-per-hour", type=float, default=None)
    parser.add_argument("--average-inflation-rate", type=float, default=None)
    parser.add_argument("--json", action="store_true")
    parser.add_argument(
        "--summary",
        action="store_true",
        help="Print one compact row per year instead of itemized yearly sections.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    rental_costs, years, average_inflation_rate = _inputs_from_args(args)
    rows = project_yearly_rental_costs(
        rental_costs=rental_costs,
        years=years,
        average_inflation_rate=average_inflation_rate,
    )

    if args.json:
        print(json.dumps([asdict(row) for row in rows], indent=2, sort_keys=True))
    elif args.summary:
        print(_format_summary_table(rows))
    else:
        print(_format_itemized(rows))
    return 0


def _inputs_from_args(args):
    config = _load_config(args.rental_config) if args.rental_config else {}
    _reject_unknown_keys(
        config,
        {
            "years",
            "insurance_annual_usd",
            "club_annual_usd",
            "club_monthly_usd",
            "rental_rate_per_hour",
            "average_inflation_rate",
        },
        "rental config",
    )
    years = _value(
        config=config,
        key="years",
        args=args,
        attr="years",
        required=True,
    )
    rental_costs = RentalCostInputs(
        annual_flight_hours=_value(
            config={},
            key="annual_flight_hours",
            args=args,
            attr="annual_flight_hours",
            required=True,
        ),
        insurance_annual_usd=_value(
            config=config,
            key="insurance_annual_usd",
            args=args,
            attr="insurance_annual",
            default=0.0,
        ),
        club_annual_usd=_value(
            config=config,
            key="club_annual_usd",
            args=args,
            attr="club_annual",
            default=0.0,
        ),
        club_monthly_usd=_value(
            config=config,
            key="club_monthly_usd",
            args=args,
            attr="club_monthly",
            default=0.0,
        ),
        rental_rate_per_hour=_value(
            config=config,
            key="rental_rate_per_hour",
            args=args,
            attr="rental_rate_per_hour",
            required=True,
        ),
    )
    average_inflation_rate = _value(
        config=config,
        key="average_inflation_rate",
        args=args,
        attr="average_inflation_rate",
        default=0.0,
    )
    return rental_costs, years, average_inflation_rate


def _load_config(path: Path) -> dict:
    try:
        with path.open(encoding="utf-8") as file:
            data = json.load(file)
    except OSError as exc:
        raise SystemExit(f"could not read rental config {path}: {exc}") from exc
    except json.JSONDecodeError as exc:
        raise SystemExit(f"invalid JSON in rental config {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise SystemExit(f"rental config {path} must contain a JSON object")
    return data


def _reject_unknown_keys(config: dict, allowed: set[str], label: str) -> None:
    unknown = sorted(set(config) - allowed)
    if unknown:
        joined = ", ".join(unknown)
        raise SystemExit(f"unknown key(s) in {label}: {joined}")


def _value(
    *,
    config: dict,
    key: str,
    args,
    attr: str,
    default=None,
    required: bool = False,
):
    arg_value = getattr(args, attr)
    if arg_value is not None:
        return arg_value
    if key in config:
        return config[key]
    if required:
        flag = attr.replace("_", "-")
        raise SystemExit(
            f"missing required value {key!r}; set it in config or pass --{flag}"
        )
    return default


def _format_itemized(rows) -> str:
    sections = []
    for row in rows:
        sections.append(
            "\n".join(
                [
                    f"Year {row.year}",
                    f"  Flight hours: {row.annual_flight_hours:,.0f}",
                    "  Fixed costs:",
                    f"    Insurance: {_money(row.fixed_costs.insurance_usd)}",
                    f"    Club costs: {_money(row.fixed_costs.club_usd)}",
                    f"    Fixed total: {_money(row.fixed_costs.total_fixed_usd)}",
                    "  Per-hour costs:",
                    f"    Rental: {_money(row.variable_costs.rental_usd)}",
                    f"    Per-hour total: {_money(row.variable_costs.total_variable_usd)}",
                    "  Totals:",
                    f"    Total cost: {_money(row.total_cost_usd)}",
                    f"    Cost per flight hour: {_money(row.cost_per_hour_usd)}",
                ]
            )
        )
    return "\n\n".join(sections)


def _format_summary_table(rows) -> str:
    headers = ["Year", "Hours", "Fixed", "Rental", "Total", "$/hr"]
    lines = ["  ".join(f"{header:>12}" for header in headers)]
    for row in rows:
        lines.append(
            "  ".join(
                [
                    f"{row.year:>12}",
                    f"{row.annual_flight_hours:>12,.0f}",
                    f"{_money(row.fixed_costs.total_fixed_usd):>12}",
                    f"{_money(row.variable_costs.total_variable_usd):>12}",
                    f"{_money(row.total_cost_usd):>12}",
                    f"{_money(row.cost_per_hour_usd):>12}",
                ]
            )
        )
    return "\n".join(lines)


def _money(value: float) -> str:
    return f"${value:,.0f}"


if __name__ == "__main__":
    raise SystemExit(main())
