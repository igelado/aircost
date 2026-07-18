"""Estimate aircraft market value from age and utilization inputs."""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict

from aircost.depreciation import PROFILES, TimedComponent, estimate_aircraft_value


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Estimate aircraft value from new price, age, hours, and component status.",
    )
    parser.add_argument(
        "--profile",
        choices=sorted(PROFILES),
        default="light_piston",
        help="Aircraft family assumptions to use.",
    )
    parser.add_argument(
        "--purchase-price-new",
        type=float,
        required=True,
        help="Aircraft purchase price when new. Prefer current replacement-dollar basis.",
    )
    parser.add_argument(
        "--new-price-basis-factor",
        type=float,
        default=1.0,
        help="Multiplier to convert historical new price to the valuation dollar basis.",
    )
    parser.add_argument("--age-years", type=float, required=True)
    parser.add_argument("--airframe-hours", type=float, required=True)

    parser.add_argument("--engine-hours", type=float, default=None)
    parser.add_argument("--engine-tbo-hours", type=float, default=None)
    parser.add_argument("--engine-overhaul-cost", type=float, default=None)
    parser.add_argument("--engine-count", type=int, default=1)

    parser.add_argument("--propeller-hours", type=float, default=None)
    parser.add_argument("--propeller-tbo-hours", type=float, default=None)
    parser.add_argument("--propeller-overhaul-cost", type=float, default=None)
    parser.add_argument("--propeller-count", type=int, default=1)

    parser.add_argument(
        "--json",
        action="store_true",
        help="Print machine-readable JSON output.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    engine = _component_from_args(
        name="engine",
        hours=args.engine_hours,
        tbo_hours=args.engine_tbo_hours,
        overhaul_cost=args.engine_overhaul_cost,
        count=args.engine_count,
    )
    propeller = _component_from_args(
        name="propeller",
        hours=args.propeller_hours,
        tbo_hours=args.propeller_tbo_hours,
        overhaul_cost=args.propeller_overhaul_cost,
        count=args.propeller_count,
    )

    estimate = estimate_aircraft_value(
        purchase_price_new_usd=args.purchase_price_new,
        new_price_basis_factor=args.new_price_basis_factor,
        age_years=args.age_years,
        airframe_hours=args.airframe_hours,
        profile=args.profile,
        engine=engine,
        propeller=propeller,
    )

    if args.json:
        print(json.dumps(asdict(estimate), indent=2, sort_keys=True))
    else:
        print(_format_human_output(estimate))
    return 0


def _component_from_args(
    *,
    name: str,
    hours: float | None,
    tbo_hours: float | None,
    overhaul_cost: float | None,
    count: int,
) -> TimedComponent | None:
    values = [hours, tbo_hours, overhaul_cost]
    if all(value is None for value in values):
        return None
    if any(value is None for value in values):
        raise SystemExit(
            f"{name} adjustment requires --{name}-hours, --{name}-tbo-hours, "
            f"and --{name}-overhaul-cost"
        )
    return TimedComponent(
        name=name,
        hours_since_overhaul=hours,
        tbo_hours=tbo_hours,
        overhaul_cost_usd=overhaul_cost,
        count=count,
    )


def _format_human_output(estimate) -> str:
    b = estimate.breakdown
    lines = [
        f"Estimated value: {_money(estimate.estimated_value_usd)}",
        f"Depreciation: {_money(estimate.depreciation_usd)} ({estimate.depreciation_fraction:.1%})",
        "",
        f"Profile: {estimate.profile.name}",
        f"Effective new-price basis: {_money(b.effective_new_price_usd)}",
        f"Age baseline: {_money(b.age_baseline_value_usd)} ({b.age_residual_fraction:.1%} of new)",
        f"Expected airframe hours: {b.expected_airframe_hours:,.0f}",
        f"Airframe utilization factor: {b.airframe_factor:.3f}",
        f"High-time liquidity factor: {b.high_time_factor:.3f}",
        f"Utilization-adjusted baseline: {_money(b.utilization_adjusted_value_usd)}",
        f"Engine adjustment: {_money(b.engine_adjustment_usd)}",
        f"Propeller adjustment: {_money(b.propeller_adjustment_usd)}",
        f"Minimum modeled value: {_money(b.minimum_value_usd)}",
    ]
    return "\n".join(lines)


def _money(value: float) -> str:
    return f"${value:,.0f}"


if __name__ == "__main__":
    raise SystemExit(main())
