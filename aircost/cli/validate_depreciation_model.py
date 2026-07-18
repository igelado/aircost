"""Validate depreciation estimates against current asking-price data."""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

from aircost.depreciation_validation import (
    load_validation_cases,
    summarize_validation_results,
    validate_depreciation_cases,
)


DEFAULT_VALIDATION_DATA = Path("data/depreciation_validation_listings.json")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Validate depreciation estimates against aircraft asking prices.",
    )
    parser.add_argument(
        "--validation-data",
        type=Path,
        default=DEFAULT_VALIDATION_DATA,
        help="JSON file containing current aircraft listing data.",
    )
    parser.add_argument("--json", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    cases = load_validation_cases(args.validation_data)
    results = validate_depreciation_cases(cases)
    summary = summarize_validation_results(results)

    if args.json:
        print(
            json.dumps(
                {
                    "summary": asdict(summary),
                    "results": [asdict(result) for result in results],
                },
                indent=2,
                sort_keys=True,
            )
        )
    else:
        print(_format_human_output(summary, results))
    return 0


def _format_human_output(summary, results) -> str:
    headers = [
        "Listing",
        "Model",
        "Ask",
        "Ask/New",
        "Estimate",
        "Model/New",
        "Error",
        "Error %",
        "Est/Ask",
    ]
    lines = [
        "Depreciation validation against asking prices",
        "",
        "  ".join(f"{header:>16}" for header in headers),
    ]
    for result in results:
        lines.append(
            "  ".join(
                [
                    f"{_short_label(result.label):>16}",
                    f"{_short_label(result.model):>16}",
                    f"{_money(result.asking_price_usd):>16}",
                    f"{result.asking_to_new_fraction:>15.1%}",
                    f"{_money(result.estimated_value_usd):>16}",
                    f"{result.estimated_to_new_fraction:>15.1%}",
                    f"{_money(result.error_usd):>16}",
                    f"{result.error_fraction:>15.1%}",
                    f"{result.estimate_to_asking_ratio:>16.2f}",
                ]
            )
        )

    lines.extend(
        [
            "",
            f"Count: {summary.count}",
            f"Mean error: {_money(summary.mean_error_usd)} ({summary.mean_error_fraction:.1%})",
            "Mean absolute error: "
            f"{_money(summary.mean_absolute_error_usd)} "
            f"({summary.mean_absolute_error_fraction:.1%})",
            "Median absolute error: "
            f"{summary.median_absolute_error_fraction:.1%}",
        ]
    )
    grouped = _group_results_by_model(results)
    if len(grouped) > 1:
        lines.extend(["", "By model:"])
        for model in sorted(grouped):
            model_summary = summarize_validation_results(grouped[model])
            lines.append(
                f"  {_short_label(model):<16} "
                f"n={model_summary.count:<3} "
                f"mean abs={model_summary.mean_absolute_error_fraction:.1%} "
                f"median abs={model_summary.median_absolute_error_fraction:.1%}"
            )
    return "\n".join(lines)


def _group_results_by_model(results) -> dict[str, list]:
    grouped: dict[str, list] = {}
    for result in results:
        grouped.setdefault(result.model, []).append(result)
    return grouped


def _short_label(value: str) -> str:
    if len(value) <= 16:
        return value
    return value[:13] + "..."


def _money(value: float) -> str:
    return f"${value:,.0f}"


if __name__ == "__main__":
    raise SystemExit(main())
