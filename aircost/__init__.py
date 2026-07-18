"""Aircraft cost and valuation models."""

from .annual_costs import (
    AircraftCostState,
    FixedCostBreakdown,
    FixedCostInputs,
    HourlyCostInputs,
    VariableCostBreakdown,
    YearAircraftState,
    YearlyCost,
    project_yearly_costs,
)
from .depreciation import (
    AircraftProfile,
    EstimateBreakdown,
    PriceEstimate,
    TimedComponent,
    estimate_aircraft_value,
    get_profile,
)
from .depreciation_validation import (
    DepreciationValidationCase,
    DepreciationValidationResult,
    DepreciationValidationSummary,
    ValidationComponent,
    load_validation_cases,
    summarize_validation_results,
    validate_depreciation_case,
    validate_depreciation_cases,
)
from .economics_comparison import (
    YearlyPurchaseRentalComparison,
    compare_purchase_vs_rent_and_invest,
)
from .investment_returns import (
    InvestmentInputs,
    YearlyInvestmentReturn,
    project_investment_returns,
)
from .rental_costs import (
    RentalCostInputs,
    RentalFixedCostBreakdown,
    RentalVariableCostBreakdown,
    YearlyRentalCost,
    project_yearly_rental_costs,
)

__all__ = [
    "AircraftCostState",
    "AircraftProfile",
    "DepreciationValidationCase",
    "DepreciationValidationResult",
    "DepreciationValidationSummary",
    "EstimateBreakdown",
    "FixedCostBreakdown",
    "FixedCostInputs",
    "HourlyCostInputs",
    "InvestmentInputs",
    "PriceEstimate",
    "RentalCostInputs",
    "RentalFixedCostBreakdown",
    "RentalVariableCostBreakdown",
    "TimedComponent",
    "ValidationComponent",
    "VariableCostBreakdown",
    "YearAircraftState",
    "YearlyInvestmentReturn",
    "YearlyPurchaseRentalComparison",
    "YearlyCost",
    "YearlyRentalCost",
    "compare_purchase_vs_rent_and_invest",
    "estimate_aircraft_value",
    "get_profile",
    "load_validation_cases",
    "project_investment_returns",
    "project_yearly_costs",
    "project_yearly_rental_costs",
    "summarize_validation_results",
    "validate_depreciation_case",
    "validate_depreciation_cases",
]
