"""Validate depreciation estimates against aircraft asking-price data."""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

from .depreciation import TimedComponent, estimate_aircraft_value


@dataclass(frozen=True)
class ValidationComponent:
    """Timed component assumptions for a validation listing."""

    hours: float
    tbo_hours: float
    overhaul_cost_usd: float
    count: int = 1
    baseline_life_fraction: float = 0.0


@dataclass(frozen=True)
class DepreciationValidationCase:
    """Current-market listing used to validate depreciation estimates."""

    label: str
    model: str
    asking_price_usd: float
    purchase_price_new_usd: float
    age_years: float
    airframe_hours: float
    profile: str = "light_piston"
    new_price_basis_factor: float = 1.0
    source_url: str = ""
    new_price_basis_source_url: str = ""
    new_price_basis_notes: str = ""
    notes: str = ""
    engine: ValidationComponent | None = None
    propeller: ValidationComponent | None = None


@dataclass(frozen=True)
class DepreciationValidationResult:
    """Modeled value versus one asking-price listing."""

    label: str
    model: str
    asking_price_usd: float
    purchase_price_new_usd: float
    new_price_basis_factor: float
    effective_new_price_basis_usd: float
    estimated_value_usd: float
    asking_to_new_fraction: float
    estimated_to_new_fraction: float
    error_usd: float
    error_fraction: float
    absolute_error_fraction: float
    estimate_to_asking_ratio: float
    source_url: str
    new_price_basis_source_url: str
    new_price_basis_notes: str
    notes: str


@dataclass(frozen=True)
class DepreciationValidationSummary:
    """Aggregate validation error metrics."""

    count: int
    mean_error_usd: float
    mean_absolute_error_usd: float
    mean_error_fraction: float
    mean_absolute_error_fraction: float
    median_absolute_error_fraction: float


def load_validation_cases(path: Path) -> list[DepreciationValidationCase]:
    """Load validation cases from JSON."""

    with path.open(encoding="utf-8") as file:
        data = json.load(file)
    if not isinstance(data, list):
        raise ValueError(f"{path} must contain a JSON array")
    return [_case_from_dict(item) for item in data]


def validate_depreciation_cases(
    cases: list[DepreciationValidationCase],
) -> list[DepreciationValidationResult]:
    """Validate all cases against the depreciation model."""

    return [validate_depreciation_case(case) for case in cases]


def validate_depreciation_case(
    case: DepreciationValidationCase,
) -> DepreciationValidationResult:
    """Validate one asking-price listing against the depreciation model."""

    estimate = estimate_aircraft_value(
        purchase_price_new_usd=case.purchase_price_new_usd,
        age_years=case.age_years,
        airframe_hours=case.airframe_hours,
        profile=case.profile,
        new_price_basis_factor=case.new_price_basis_factor,
        engine=_timed_component("engine", case.engine),
        propeller=_timed_component("propeller", case.propeller),
    )
    error = estimate.estimated_value_usd - case.asking_price_usd
    error_fraction = error / case.asking_price_usd if case.asking_price_usd else 0.0
    effective_new_price = case.purchase_price_new_usd * case.new_price_basis_factor
    asking_to_new_fraction = (
        case.asking_price_usd / effective_new_price
        if effective_new_price
        else 0.0
    )
    estimated_to_new_fraction = (
        estimate.estimated_value_usd / effective_new_price
        if effective_new_price
        else 0.0
    )
    estimate_to_asking_ratio = (
        estimate.estimated_value_usd / case.asking_price_usd
        if case.asking_price_usd
        else 0.0
    )

    return DepreciationValidationResult(
        label=case.label,
        model=case.model,
        asking_price_usd=case.asking_price_usd,
        purchase_price_new_usd=case.purchase_price_new_usd,
        new_price_basis_factor=case.new_price_basis_factor,
        effective_new_price_basis_usd=effective_new_price,
        estimated_value_usd=estimate.estimated_value_usd,
        asking_to_new_fraction=asking_to_new_fraction,
        estimated_to_new_fraction=estimated_to_new_fraction,
        error_usd=error,
        error_fraction=error_fraction,
        absolute_error_fraction=abs(error_fraction),
        estimate_to_asking_ratio=estimate_to_asking_ratio,
        source_url=case.source_url,
        new_price_basis_source_url=case.new_price_basis_source_url,
        new_price_basis_notes=case.new_price_basis_notes,
        notes=case.notes,
    )


def summarize_validation_results(
    results: list[DepreciationValidationResult],
) -> DepreciationValidationSummary:
    """Summarize validation errors."""

    if not results:
        return DepreciationValidationSummary(
            count=0,
            mean_error_usd=0.0,
            mean_absolute_error_usd=0.0,
            mean_error_fraction=0.0,
            mean_absolute_error_fraction=0.0,
            median_absolute_error_fraction=0.0,
        )

    count = len(results)
    abs_error_fractions = sorted(result.absolute_error_fraction for result in results)
    return DepreciationValidationSummary(
        count=count,
        mean_error_usd=sum(result.error_usd for result in results) / count,
        mean_absolute_error_usd=(
            sum(abs(result.error_usd) for result in results) / count
        ),
        mean_error_fraction=sum(result.error_fraction for result in results) / count,
        mean_absolute_error_fraction=sum(abs_error_fractions) / count,
        median_absolute_error_fraction=_median(abs_error_fractions),
    )


def _case_from_dict(data: dict) -> DepreciationValidationCase:
    if not isinstance(data, dict):
        raise ValueError("each validation case must be a JSON object")
    return DepreciationValidationCase(
        label=data["label"],
        model=data["model"],
        asking_price_usd=data["asking_price_usd"],
        purchase_price_new_usd=data["purchase_price_new_usd"],
        age_years=data["age_years"],
        airframe_hours=data["airframe_hours"],
        profile=data.get("profile", "light_piston"),
        new_price_basis_factor=data.get("new_price_basis_factor", 1.0),
        source_url=data.get("source_url", ""),
        new_price_basis_source_url=data.get("new_price_basis_source_url", ""),
        new_price_basis_notes=data.get("new_price_basis_notes", ""),
        notes=data.get("notes", ""),
        engine=_component_from_dict(data.get("engine")),
        propeller=_component_from_dict(data.get("propeller")),
    )


def _component_from_dict(data: dict | None) -> ValidationComponent | None:
    if data is None:
        return None
    if not isinstance(data, dict):
        raise ValueError("component must be a JSON object")
    return ValidationComponent(
        hours=data["hours"],
        tbo_hours=data["tbo_hours"],
        overhaul_cost_usd=data["overhaul_cost_usd"],
        count=data.get("count", 1),
        baseline_life_fraction=data.get("baseline_life_fraction", 0.0),
    )


def _timed_component(
    name: str,
    component: ValidationComponent | None,
) -> TimedComponent | None:
    if component is None:
        return None
    return TimedComponent(
        name=name,
        hours_since_overhaul=component.hours,
        tbo_hours=component.tbo_hours,
        overhaul_cost_usd=component.overhaul_cost_usd,
        count=component.count,
        baseline_life_fraction=component.baseline_life_fraction,
    )


def _median(values: list[float]) -> float:
    count = len(values)
    midpoint = count // 2
    if count % 2:
        return values[midpoint]
    return (values[midpoint - 1] + values[midpoint]) / 2.0
