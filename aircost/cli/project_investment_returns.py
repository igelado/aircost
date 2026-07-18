"""Project reinvested investment returns."""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

from aircost.investment_returns import InvestmentInputs, project_investment_returns


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Project investment value with reinvested dividend payments.",
    )
    parser.add_argument(
        "--investment-config",
        type=Path,
        help="JSON file with investment return assumptions.",
    )
    parser.add_argument("--years", type=int, default=None)
    parser.add_argument("--initial-amount", type=float, default=None)
    parser.add_argument("--annual-return-rate", type=float, default=None)
    parser.add_argument("--dividend-payments-per-year", type=int, default=None)
    parser.add_argument("--json", action="store_true")
    parser.add_argument(
        "--summary",
        action="store_true",
        help="Print one compact row per year instead of itemized yearly sections.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    investment, years = _inputs_from_args(args)
    rows = project_investment_returns(investment=investment, years=years)

    if args.json:
        print(json.dumps([asdict(row) for row in rows], indent=2, sort_keys=True))
    elif args.summary:
        print(_format_summary_table(rows))
    else:
        print(_format_itemized(rows))
    return 0


def _inputs_from_args(args):
    config = _load_config(args.investment_config) if args.investment_config else {}
    _reject_unknown_keys(
        config,
        {
            "years",
            "annual_return_rate",
            "dividend_payments_per_year",
        },
        "investment config",
    )
    years = _value(
        config=config,
        key="years",
        args=args,
        attr="years",
        required=True,
    )
    investment = InvestmentInputs(
        initial_amount_usd=_required_arg(args, "initial_amount"),
        annual_return_rate=_value(
            config=config,
            key="annual_return_rate",
            args=args,
            attr="annual_return_rate",
            required=True,
        ),
        dividend_payments_per_year=_value(
            config=config,
            key="dividend_payments_per_year",
            args=args,
            attr="dividend_payments_per_year",
            default=2,
        ),
    )
    return investment, years


def _load_config(path: Path) -> dict:
    try:
        with path.open(encoding="utf-8") as file:
            data = json.load(file)
    except OSError as exc:
        raise SystemExit(f"could not read investment config {path}: {exc}") from exc
    except json.JSONDecodeError as exc:
        raise SystemExit(f"invalid JSON in investment config {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise SystemExit(f"investment config {path} must contain a JSON object")
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


def _required_arg(args, attr: str):
    value = getattr(args, attr)
    if value is not None:
        return value
    flag = attr.replace("_", "-")
    raise SystemExit(f"missing required value; pass --{flag}")


def _format_itemized(rows) -> str:
    sections = []
    for row in rows:
        sections.append(
            "\n".join(
                [
                    f"Year {row.year}",
                    f"  Start balance: {_money(row.start_balance_usd)}",
                    "  Reinvested dividends: "
                    f"{_money(row.dividends_reinvested_usd)}",
                    f"  End balance: {_money(row.end_balance_usd)}",
                    f"  Cumulative return: {_money(row.cumulative_return_usd)}",
                    f"  Cumulative return rate: {row.cumulative_return_fraction:.2%}",
                ]
            )
        )
    return "\n\n".join(sections)


def _format_summary_table(rows) -> str:
    headers = ["Year", "Start", "Reinvested", "End", "Cum Return", "Cum %"]
    lines = ["  ".join(f"{header:>13}" for header in headers)]
    for row in rows:
        lines.append(
            "  ".join(
                [
                    f"{row.year:>13}",
                    f"{_money(row.start_balance_usd):>13}",
                    f"{_money(row.dividends_reinvested_usd):>13}",
                    f"{_money(row.end_balance_usd):>13}",
                    f"{_money(row.cumulative_return_usd):>13}",
                    f"{row.cumulative_return_fraction:>12.2%}",
                ]
            )
        )
    return "\n".join(lines)


def _money(value: float) -> str:
    return f"${value:,.0f}"


if __name__ == "__main__":
    raise SystemExit(main())
