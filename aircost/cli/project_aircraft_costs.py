"""Project yearly aircraft ownership costs."""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

from aircost.annual_costs import (
    AircraftCostState,
    FixedCostInputs,
    HourlyCostInputs,
    project_yearly_costs,
)
from aircost.depreciation import PROFILES


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Project yearly aircraft fixed costs, hourly costs, and depreciation.",
    )
    parser.add_argument(
        "--aircraft-config",
        type=Path,
        help="JSON file with aircraft-specific starting values.",
    )
    parser.add_argument(
        "--cost-config",
        type=Path,
        help="JSON file with common fixed, hourly, and inflation assumptions.",
    )
    parser.add_argument("--years", type=int, default=None)
    parser.add_argument("--profile", choices=sorted(PROFILES), default=None)
    parser.add_argument("--purchase-price-new", type=float, default=None)
    parser.add_argument("--new-price-basis-factor", type=float, default=None)
    parser.add_argument("--age-years", type=float, default=None)
    parser.add_argument("--airframe-hours", type=float, default=None)

    parser.add_argument("--engine-hours", type=float, default=None)
    parser.add_argument("--engine-tbo-hours", type=float, default=None)
    parser.add_argument("--engine-overhaul-cost", type=float, default=None)
    parser.add_argument("--engine-count", type=int, default=None)
    parser.add_argument(
        "--engine-value-baseline-life-fraction",
        type=float,
        default=None,
    )
    parser.add_argument("--propeller-hours", type=float, default=None)
    parser.add_argument("--propeller-tbo-hours", type=float, default=None)
    parser.add_argument("--propeller-overhaul-cost", type=float, default=None)
    parser.add_argument("--propeller-count", type=int, default=None)
    parser.add_argument(
        "--propeller-value-baseline-life-fraction",
        type=float,
        default=None,
    )

    parser.add_argument("--annual-flight-hours", type=float, required=True)
    parser.add_argument("--tie-down-annual", type=float, default=None)
    parser.add_argument("--insurance-annual", type=float, default=None)
    parser.add_argument("--property-tax-annual", type=float, default=None)
    parser.add_argument(
        "--property-tax-rate",
        type=float,
        default=None,
        help="Annual property tax rate as a decimal, e.g. 0.01 for 1%%.",
    )
    parser.add_argument("--annual-inspection", type=float, default=None)

    parser.add_argument("--fuel-burn-gph", type=float, default=None)
    parser.add_argument("--fuel-price-per-gallon", type=float, default=None)
    parser.add_argument("--oil-quarts-per-hour", type=float, default=None)
    parser.add_argument("--oil-price-per-quart", type=float, default=None)
    parser.add_argument("--other-maintenance-per-hour", type=float, default=None)

    parser.add_argument(
        "--average-inflation-rate",
        type=float,
        default=None,
        help="Annual inflation rate applied to all future cost adjustments.",
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
    initial_state, fixed_costs, hourly_costs, average_inflation_rate, years = (
        _inputs_from_args(args)
    )
    rows = project_yearly_costs(
        initial_state=initial_state,
        fixed_costs=fixed_costs,
        hourly_costs=hourly_costs,
        average_inflation_rate=average_inflation_rate,
        years=years,
    )

    if args.json:
        print(json.dumps([asdict(row) for row in rows], indent=2, sort_keys=True))
    elif args.summary:
        print(_format_summary_table(rows))
    else:
        print(_format_itemized(rows))
    return 0


def _inputs_from_args(args):
    aircraft_config = (
        _load_config(args.aircraft_config, "aircraft") if args.aircraft_config else {}
    )
    cost_config = _load_config(args.cost_config, "cost") if args.cost_config else {}
    _reject_unknown_keys(
        aircraft_config,
        {
            "profile",
            "purchase_price_new_usd",
            "new_price_basis_factor",
            "age_years",
            "airframe_hours",
            "engine_hours",
            "engine_tbo_hours",
            "engine_overhaul_cost_usd",
            "engine_count",
            "engine_value_baseline_life_fraction",
            "propeller_hours",
            "propeller_tbo_hours",
            "propeller_overhaul_cost_usd",
            "propeller_count",
            "propeller_value_baseline_life_fraction",
            "insurance_annual_usd",
            "annual_inspection_usd",
            "fuel_burn_gph",
            "oil_quarts_per_hour",
            "oil_price_per_quart",
            "other_maintenance_per_hour",
        },
        "aircraft config",
    )
    _reject_unknown_keys(
        cost_config,
        {"years", "fixed_costs", "hourly_costs", "average_inflation_rate"},
        "cost config",
    )

    fixed_config = _nested_config(cost_config, "fixed_costs")
    hourly_config = _nested_config(cost_config, "hourly_costs")

    _reject_unknown_keys(
        fixed_config,
        {
            "tie_down_annual_usd",
            "property_tax_annual_usd",
            "property_tax_rate",
        },
        "cost config fixed_costs",
    )
    _reject_unknown_keys(
        hourly_config,
        {
            "fuel_price_per_gallon",
        },
        "cost config hourly_costs",
    )
    years = _value(
        config=cost_config,
        key="years",
        args=args,
        attr="years",
        required=True,
    )
    initial_state = AircraftCostState(
        profile=_value(
            config=aircraft_config,
            key="profile",
            args=args,
            attr="profile",
            default="light_piston",
        ),
        purchase_price_new_usd=_value(
            config=aircraft_config,
            key="purchase_price_new_usd",
            args=args,
            attr="purchase_price_new",
            required=True,
        ),
        new_price_basis_factor=_value(
            config=aircraft_config,
            key="new_price_basis_factor",
            args=args,
            attr="new_price_basis_factor",
            default=1.0,
        ),
        age_years=_value(
            config=aircraft_config,
            key="age_years",
            args=args,
            attr="age_years",
            required=True,
        ),
        airframe_hours=_value(
            config=aircraft_config,
            key="airframe_hours",
            args=args,
            attr="airframe_hours",
            required=True,
        ),
        engine_hours=_value(
            config=aircraft_config,
            key="engine_hours",
            args=args,
            attr="engine_hours",
            required=True,
        ),
        engine_tbo_hours=_value(
            config=aircraft_config,
            key="engine_tbo_hours",
            args=args,
            attr="engine_tbo_hours",
            required=True,
        ),
        engine_overhaul_cost_usd=_value(
            config=aircraft_config,
            key="engine_overhaul_cost_usd",
            args=args,
            attr="engine_overhaul_cost",
            required=True,
        ),
        engine_count=_value(
            config=aircraft_config,
            key="engine_count",
            args=args,
            attr="engine_count",
            default=1,
        ),
        engine_value_baseline_life_fraction=_value(
            config=aircraft_config,
            key="engine_value_baseline_life_fraction",
            args=args,
            attr="engine_value_baseline_life_fraction",
            default=0.5,
        ),
        propeller_hours=_value(
            config=aircraft_config,
            key="propeller_hours",
            args=args,
            attr="propeller_hours",
            required=True,
        ),
        propeller_tbo_hours=_value(
            config=aircraft_config,
            key="propeller_tbo_hours",
            args=args,
            attr="propeller_tbo_hours",
            required=True,
        ),
        propeller_overhaul_cost_usd=_value(
            config=aircraft_config,
            key="propeller_overhaul_cost_usd",
            args=args,
            attr="propeller_overhaul_cost",
            required=True,
        ),
        propeller_count=_value(
            config=aircraft_config,
            key="propeller_count",
            args=args,
            attr="propeller_count",
            default=1,
        ),
        propeller_value_baseline_life_fraction=_value(
            config=aircraft_config,
            key="propeller_value_baseline_life_fraction",
            args=args,
            attr="propeller_value_baseline_life_fraction",
            default=0.5,
        ),
    )
    fixed_costs = FixedCostInputs(
        tie_down_annual_usd=_value(
            config=fixed_config,
            key="tie_down_annual_usd",
            args=args,
            attr="tie_down_annual",
            default=0.0,
        ),
        insurance_annual_usd=_value(
            config=aircraft_config,
            key="insurance_annual_usd",
            args=args,
            attr="insurance_annual",
            default=0.0,
        ),
        property_tax_annual_usd=_value(
            config=fixed_config,
            key="property_tax_annual_usd",
            args=args,
            attr="property_tax_annual",
            default=0.0,
        ),
        property_tax_rate=_value(
            config=fixed_config,
            key="property_tax_rate",
            args=args,
            attr="property_tax_rate",
            default=0.0,
        ),
        annual_inspection_usd=_value(
            config=aircraft_config,
            key="annual_inspection_usd",
            args=args,
            attr="annual_inspection",
            default=0.0,
        ),
    )
    hourly_costs = HourlyCostInputs(
        annual_flight_hours=_value(
            config={},
            key="annual_flight_hours",
            args=args,
            attr="annual_flight_hours",
            required=True,
        ),
        fuel_burn_gph=_value(
            config=aircraft_config,
            key="fuel_burn_gph",
            args=args,
            attr="fuel_burn_gph",
            default=0.0,
        ),
        fuel_price_per_gallon=_value(
            config=hourly_config,
            key="fuel_price_per_gallon",
            args=args,
            attr="fuel_price_per_gallon",
            default=0.0,
        ),
        oil_quarts_per_hour=_value(
            config=aircraft_config,
            key="oil_quarts_per_hour",
            args=args,
            attr="oil_quarts_per_hour",
            default=0.0,
        ),
        oil_price_per_quart=_value(
            config=aircraft_config,
            key="oil_price_per_quart",
            args=args,
            attr="oil_price_per_quart",
            default=0.0,
        ),
        other_maintenance_per_hour=_value(
            config=aircraft_config,
            key="other_maintenance_per_hour",
            args=args,
            attr="other_maintenance_per_hour",
            default=0.0,
        ),
    )
    average_inflation_rate = _value(
        config=cost_config,
        key="average_inflation_rate",
        args=args,
        attr="average_inflation_rate",
        default=0.0,
    )
    return initial_state, fixed_costs, hourly_costs, average_inflation_rate, years


def _load_config(path: Path, label: str) -> dict:
    try:
        with path.open(encoding="utf-8") as file:
            data = json.load(file)
    except OSError as exc:
        raise SystemExit(f"could not read {label} config {path}: {exc}") from exc
    except json.JSONDecodeError as exc:
        raise SystemExit(f"invalid JSON in {label} config {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise SystemExit(f"{label} config {path} must contain a JSON object")
    return data


def _nested_config(config: dict, key: str) -> dict:
    value = config.get(key, {})
    if not isinstance(value, dict):
        raise SystemExit(f"cost config {key} must contain a JSON object")
    return value


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
                    f"  Start value: {_money(row.start_value_usd)}",
                    "  End value before inflation: "
                    f"{_money(row.end_value_before_inflation_usd)}",
                    f"  Inflation adjustment: {_money(row.inflation_adjustment_usd)}",
                    f"  End value: {_money(row.end_value_usd)}",
                    "  Fixed costs:",
                    f"    Tie-down: {_money(row.fixed_costs.tie_down_usd)}",
                    f"    Insurance: {_money(row.fixed_costs.insurance_usd)}",
                    f"    Property tax: {_money(row.fixed_costs.property_tax_usd)}",
                    f"    Annual inspection: {_money(row.fixed_costs.annual_inspection_usd)}",
                    f"    Depreciation: {_money(row.fixed_costs.depreciation_usd)}",
                    f"    Fixed total: {_money(row.fixed_costs.total_fixed_usd)}",
                    "  Per-hour costs:",
                    f"    Fuel: {_money(row.variable_costs.fuel_usd)}",
                    f"    Oil: {_money(row.variable_costs.oil_usd)}",
                    "    Engine overhaul reserve: "
                    f"{_money(row.variable_costs.engine_overhaul_reserve_usd)}",
                    "    Propeller overhaul reserve: "
                    f"{_money(row.variable_costs.propeller_overhaul_reserve_usd)}",
                    "    Other maintenance: "
                    f"{_money(row.variable_costs.other_maintenance_usd)}",
                    f"    Per-hour total: {_money(row.variable_costs.total_variable_usd)}",
                    "  Totals:",
                    f"    Total cost: {_money(row.total_cost_usd)}",
                    f"    Cash cost excluding depreciation: {_money(row.total_cash_cost_usd)}",
                    f"    Cost per flight hour: {_money(row.cost_per_hour_usd)}",
                    "    Cash cost per flight hour: "
                    f"{_money(row.cash_cost_per_hour_usd)}",
                ]
            )
        )
    return "\n\n".join(sections)


def _format_summary_table(rows) -> str:
    headers = [
        "Year",
        "Hours",
        "Start Value",
        "Pre-Infl End",
        "End Value",
        "Fixed",
        "Variable",
        "Depreciation",
        "Total",
        "$/hr",
    ]
    lines = ["  ".join(f"{header:>13}" for header in headers)]
    for row in rows:
        lines.append(
            "  ".join(
                [
                    f"{row.year:>13}",
                    f"{row.annual_flight_hours:>13,.0f}",
                    f"{_money(row.start_value_usd):>13}",
                    f"{_money(row.end_value_before_inflation_usd):>13}",
                    f"{_money(row.end_value_usd):>13}",
                    f"{_money(row.fixed_costs.total_fixed_usd):>13}",
                    f"{_money(row.variable_costs.total_variable_usd):>13}",
                    f"{_money(row.fixed_costs.depreciation_usd):>13}",
                    f"{_money(row.total_cost_usd):>13}",
                    f"{_money(row.cost_per_hour_usd):>13}",
                ]
            )
        )
    return "\n".join(lines)


def _money(value: float) -> str:
    return f"${value:,.0f}"


if __name__ == "__main__":
    raise SystemExit(main())
