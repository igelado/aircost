use std::f64::consts::LN_2;

pub const DEFAULT_ANNUAL_AIRFRAME_HOURS: f64 = 200.0;

#[derive(Clone, Debug, PartialEq)]
pub struct AircraftProfile {
    pub name: String,
    pub age_decay_rate: f64,
    pub long_run_residual_fraction: f64,
    pub new_to_used_discount_fraction: f64,
    pub new_to_used_discount_years: f64,
    pub airframe_doubling_discount: f64,
    pub max_airframe_premium: f64,
    pub max_airframe_discount: f64,
    pub replacement_floor_fraction: f64,
    pub minimum_value_fraction: f64,
    pub high_time_threshold_hours: Option<f64>,
    pub high_time_discount_at_double_threshold: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedComponent {
    pub name: String,
    pub hours_since_overhaul: f64,
    pub tbo_hours: f64,
    pub overhaul_cost_usd: f64,
    pub value_reference_year: i64,
    pub valuation_year: i64,
    pub average_inflation_rate: f64,
    pub count: i64,
    pub baseline_life_fraction: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AvionicsProfile {
    pub name: String,
    pub age_decay_rate: f64,
    pub long_run_residual_fraction: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AvionicsComponent {
    pub name: String,
    pub introduced_year: i64,
    pub valuation_year: i64,
    pub value_reference_year: i64,
    pub average_inflation_rate: f64,
    pub unit_replacement_cost_usd: f64,
    pub quantity: i64,
    pub profile: AvionicsProfile,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DollarBasis {
    pub value_reference_year: i64,
    pub valuation_year: i64,
    pub average_inflation_rate: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EstimateBreakdown {
    pub effective_new_price_usd: f64,
    pub value_reference_year: i64,
    pub valuation_year: i64,
    pub average_inflation_rate: f64,
    pub dollar_basis_factor: f64,
    pub age_residual_fraction: f64,
    pub age_baseline_value_usd: f64,
    pub expected_airframe_hours: f64,
    pub airframe_factor: f64,
    pub high_time_factor: f64,
    pub airframe_value_usd: f64,
    pub replacement_floor_basis_usd: f64,
    pub replacement_floor_value_usd: f64,
    pub engine_adjustment_usd: f64,
    pub propeller_adjustment_usd: f64,
    pub avionics_value_usd: f64,
    pub avionics_replacement_basis_usd: f64,
    pub minimum_value_usd: f64,
    pub raw_estimated_value_usd: f64,
    pub valuation_basis_usd: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PriceEstimate {
    pub estimated_value_usd: f64,
    pub depreciation_usd: f64,
    pub depreciation_fraction: f64,
    pub profile: AircraftProfile,
    pub breakdown: EstimateBreakdown,
}

pub fn builtin_aircraft_profiles() -> Vec<AircraftProfile> {
    vec![
        AircraftProfile {
            name: "light_piston".to_string(),
            age_decay_rate: 0.07,
            long_run_residual_fraction: 0.16,
            new_to_used_discount_fraction: 0.0,
            new_to_used_discount_years: 1.0,
            airframe_doubling_discount: 0.13,
            max_airframe_premium: 0.12,
            max_airframe_discount: 0.25,
            replacement_floor_fraction: 0.0,
            minimum_value_fraction: 0.06,
            high_time_threshold_hours: Some(10_000.0),
            high_time_discount_at_double_threshold: 0.10,
        },
        AircraftProfile {
            name: "complex_piston".to_string(),
            age_decay_rate: 0.045,
            long_run_residual_fraction: 0.34,
            new_to_used_discount_fraction: 0.11,
            new_to_used_discount_years: 1.0,
            airframe_doubling_discount: 0.15,
            max_airframe_premium: 0.12,
            max_airframe_discount: 0.28,
            replacement_floor_fraction: 0.0,
            minimum_value_fraction: 0.06,
            high_time_threshold_hours: Some(10_000.0),
            high_time_discount_at_double_threshold: 0.12,
        },
        AircraftProfile {
            name: "turboprop".to_string(),
            age_decay_rate: 0.06,
            long_run_residual_fraction: 0.20,
            new_to_used_discount_fraction: 0.12,
            new_to_used_discount_years: 1.0,
            airframe_doubling_discount: 0.18,
            max_airframe_premium: 0.10,
            max_airframe_discount: 0.35,
            replacement_floor_fraction: 0.0,
            minimum_value_fraction: 0.05,
            high_time_threshold_hours: None,
            high_time_discount_at_double_threshold: 0.0,
        },
        AircraftProfile {
            name: "business_jet".to_string(),
            age_decay_rate: 0.075,
            long_run_residual_fraction: 0.08,
            new_to_used_discount_fraction: 0.15,
            new_to_used_discount_years: 1.0,
            airframe_doubling_discount: 0.20,
            max_airframe_premium: 0.08,
            max_airframe_discount: 0.40,
            replacement_floor_fraction: 0.0,
            minimum_value_fraction: 0.04,
            high_time_threshold_hours: None,
            high_time_discount_at_double_threshold: 0.0,
        },
    ]
}

pub fn get_aircraft_profile(name: &str) -> Result<AircraftProfile, String> {
    builtin_aircraft_profiles()
        .into_iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| {
            let available = builtin_aircraft_profiles()
                .into_iter()
                .map(|profile| profile.name)
                .collect::<Vec<_>>()
                .join(", ");
            format!("unknown profile {name:?}; available profiles: {available}")
        })
}

pub fn default_avionics_profile() -> AvionicsProfile {
    AvionicsProfile {
        name: "panel_avionics".to_string(),
        age_decay_rate: 0.12,
        long_run_residual_fraction: 0.18,
    }
}

pub fn estimate_aircraft_value(
    purchase_price_new_usd: f64,
    age_years: f64,
    airframe_hours: f64,
    profile: AircraftProfile,
    engine: Option<TimedComponent>,
    propeller: Option<TimedComponent>,
    avionics: &[AvionicsComponent],
) -> Result<PriceEstimate, String> {
    estimate_aircraft_value_in_year(
        purchase_price_new_usd,
        age_years,
        airframe_hours,
        DEFAULT_ANNUAL_AIRFRAME_HOURS,
        profile,
        engine,
        propeller,
        avionics,
        None,
        DollarBasis {
            value_reference_year: 0,
            valuation_year: 0,
            average_inflation_rate: 0.0,
        },
    )
}

pub fn estimate_aircraft_value_in_year(
    purchase_price_new_usd: f64,
    age_years: f64,
    airframe_hours: f64,
    annual_airframe_hours: f64,
    profile: AircraftProfile,
    engine: Option<TimedComponent>,
    propeller: Option<TimedComponent>,
    avionics: &[AvionicsComponent],
    replacement_floor_basis_usd: Option<f64>,
    dollar_basis: DollarBasis,
) -> Result<PriceEstimate, String> {
    require_non_negative("purchase_price_new_usd", purchase_price_new_usd)?;
    require_non_negative("age_years", age_years)?;
    require_non_negative("airframe_hours", airframe_hours)?;
    require_non_negative("annual_airframe_hours", annual_airframe_hours)?;

    let dollar_basis_factor = nominal_dollar_factor(
        dollar_basis.value_reference_year,
        dollar_basis.valuation_year,
        dollar_basis.average_inflation_rate,
    )?;
    let effective_new_price = purchase_price_new_usd * dollar_basis_factor;
    let age_fraction = age_residual_fraction(
        age_years,
        profile.age_decay_rate,
        profile.long_run_residual_fraction,
        profile.new_to_used_discount_fraction,
        profile.new_to_used_discount_years,
    )?;
    let age_baseline = effective_new_price * age_fraction;
    let expected_hours = expected_airframe_hours(age_years, annual_airframe_hours);
    let airframe_factor = airframe_utilization_factor(
        airframe_hours,
        expected_hours,
        profile.airframe_doubling_discount,
        profile.max_airframe_premium,
        profile.max_airframe_discount,
    )?;
    let high_time_factor = high_time_liquidity_factor(airframe_hours, &profile)?;
    let airframe_age_value = age_baseline * airframe_factor * high_time_factor;
    let replacement_floor_basis = replacement_floor_basis_usd.unwrap_or(0.0).max(0.0);
    let replacement_floor_value = replacement_floor_basis * profile.replacement_floor_fraction;
    let airframe_value = airframe_age_value.max(replacement_floor_value);
    let engine_adjustment = timed_component_adjustment(engine.as_ref())?;
    let propeller_adjustment = timed_component_adjustment(propeller.as_ref())?;
    let avionics_value = avionics_total_value(avionics)?;
    let avionics_replacement_basis = avionics_replacement_basis(avionics)?;
    let minimum_value = effective_new_price * profile.minimum_value_fraction;
    let raw_estimated_value =
        (airframe_value + engine_adjustment + propeller_adjustment + avionics_value)
            .max(minimum_value);
    let airframe_valuation_basis = effective_new_price.max(replacement_floor_value);
    let valuation_basis = airframe_valuation_basis + avionics_replacement_basis;
    let estimated_value = raw_estimated_value.min(valuation_basis);
    let depreciation = (valuation_basis - estimated_value).max(0.0);
    let depreciation_fraction = if valuation_basis > 0.0 {
        depreciation / valuation_basis
    } else {
        0.0
    };

    Ok(PriceEstimate {
        estimated_value_usd: estimated_value,
        depreciation_usd: depreciation,
        depreciation_fraction,
        profile,
        breakdown: EstimateBreakdown {
            effective_new_price_usd: effective_new_price,
            value_reference_year: dollar_basis.value_reference_year,
            valuation_year: dollar_basis.valuation_year,
            average_inflation_rate: dollar_basis.average_inflation_rate,
            dollar_basis_factor,
            age_residual_fraction: age_fraction,
            age_baseline_value_usd: age_baseline,
            expected_airframe_hours: expected_hours,
            airframe_factor,
            high_time_factor,
            airframe_value_usd: airframe_value,
            replacement_floor_basis_usd: replacement_floor_basis,
            replacement_floor_value_usd: replacement_floor_value,
            engine_adjustment_usd: engine_adjustment,
            propeller_adjustment_usd: propeller_adjustment,
            avionics_value_usd: avionics_value,
            avionics_replacement_basis_usd: avionics_replacement_basis,
            minimum_value_usd: minimum_value,
            raw_estimated_value_usd: raw_estimated_value,
            valuation_basis_usd: valuation_basis,
        },
    })
}

pub fn nominal_dollar_factor(
    value_reference_year: i64,
    valuation_year: i64,
    average_inflation_rate: f64,
) -> Result<f64, String> {
    require_non_negative("average_inflation_rate", average_inflation_rate)?;
    let year_delta = valuation_year - value_reference_year;
    Ok((1.0 + average_inflation_rate).powf(year_delta as f64))
}

pub fn age_residual_fraction(
    age_years: f64,
    decay_rate: f64,
    long_run_residual_fraction: f64,
    new_to_used_discount_fraction: f64,
    new_to_used_discount_years: f64,
) -> Result<f64, String> {
    require_non_negative("age_years", age_years)?;
    require_positive("decay_rate", decay_rate)?;
    require_unit_interval_left_closed("long_run_residual_fraction", long_run_residual_fraction)?;
    require_unit_interval_left_closed(
        "new_to_used_discount_fraction",
        new_to_used_discount_fraction,
    )?;
    require_positive("new_to_used_discount_years", new_to_used_discount_years)?;

    let base_fraction = long_run_residual_fraction
        + (1.0 - long_run_residual_fraction) * (-decay_rate * age_years).exp();
    let discount_progress = (age_years / new_to_used_discount_years).min(1.0);
    let new_to_used_factor = 1.0 - new_to_used_discount_fraction * discount_progress;
    Ok(base_fraction * new_to_used_factor)
}

pub fn expected_airframe_hours(age_years: f64, annual_airframe_hours: f64) -> f64 {
    if age_years <= 0.0 {
        0.0
    } else {
        age_years * annual_airframe_hours
    }
}

pub fn airframe_utilization_factor(
    actual_hours: f64,
    expected_hours: f64,
    doubling_discount: f64,
    max_premium: f64,
    max_discount: f64,
) -> Result<f64, String> {
    require_non_negative("actual_hours", actual_hours)?;
    require_non_negative("expected_hours", expected_hours)?;
    require_unit_interval_left_closed("doubling_discount", doubling_discount)?;
    require_unit_interval_left_closed("max_premium", max_premium)?;
    require_unit_interval_left_closed("max_discount", max_discount)?;
    if expected_hours <= 0.0 {
        return Ok(1.0);
    }

    let ratio = actual_hours.max(1.0) / expected_hours;
    let exponent = (1.0 - doubling_discount).ln() / LN_2;
    let raw_factor = ratio.powf(exponent);
    Ok((1.0 + max_premium).min((1.0 - max_discount).max(raw_factor)))
}

pub fn high_time_liquidity_factor(
    airframe_hours: f64,
    profile: &AircraftProfile,
) -> Result<f64, String> {
    let Some(threshold) = profile.high_time_threshold_hours else {
        return Ok(1.0);
    };
    if airframe_hours <= threshold {
        return Ok(1.0);
    }
    require_positive("high_time_threshold_hours", threshold)?;

    let capped_ratio = (airframe_hours / threshold).min(2.0);
    let severity = capped_ratio - 1.0;
    let discount = profile.high_time_discount_at_double_threshold * severity.powf(1.2);
    Ok((1.0 - discount).max(0.0))
}

pub fn timed_component_adjustment(component: Option<&TimedComponent>) -> Result<f64, String> {
    let Some(component) = component else {
        return Ok(0.0);
    };
    require_non_negative(
        &format!("{}.hours_since_overhaul", component.name),
        component.hours_since_overhaul,
    )?;
    require_positive(
        &format!("{}.tbo_hours", component.name),
        component.tbo_hours,
    )?;
    require_non_negative(
        &format!("{}.overhaul_cost_usd", component.name),
        component.overhaul_cost_usd,
    )?;
    if component.count < 1 {
        return Err(format!("{}.count must be at least 1", component.name));
    }
    if !(0.0..=1.0).contains(&component.baseline_life_fraction) {
        return Err(format!(
            "{}.baseline_life_fraction must be in [0, 1]",
            component.name
        ));
    }

    let consumed_fraction = (component.hours_since_overhaul / component.tbo_hours).min(1.0);
    let dollar_basis_factor = nominal_dollar_factor(
        component.value_reference_year,
        component.valuation_year,
        component.average_inflation_rate,
    )?;
    let nominal_overhaul_cost = component.overhaul_cost_usd * dollar_basis_factor;
    let per_component_adjustment =
        (component.baseline_life_fraction - consumed_fraction) * nominal_overhaul_cost;
    Ok(per_component_adjustment * component.count as f64)
}

pub fn avionics_component_value(component: &AvionicsComponent) -> Result<f64, String> {
    require_non_negative(
        &format!("{}.unit_replacement_cost_usd", component.name),
        component.unit_replacement_cost_usd,
    )?;
    if component.quantity < 1 {
        return Err(format!("{}.quantity must be at least 1", component.name));
    }
    require_unit_interval_left_closed(
        &format!("{}.long_run_residual_fraction", component.profile.name),
        component.profile.long_run_residual_fraction,
    )?;
    require_positive(
        &format!("{}.age_decay_rate", component.profile.name),
        component.profile.age_decay_rate,
    )?;

    let age_years = (component.valuation_year - component.value_reference_year).max(0) as f64;
    let dollar_basis_factor = nominal_dollar_factor(
        component.value_reference_year,
        component.valuation_year,
        component.average_inflation_rate,
    )?;
    let nominal_replacement_cost = component.unit_replacement_cost_usd * dollar_basis_factor;
    let residual_fraction = component.profile.long_run_residual_fraction
        + (1.0 - component.profile.long_run_residual_fraction)
            * (-component.profile.age_decay_rate * age_years).exp();
    Ok(nominal_replacement_cost * residual_fraction * component.quantity as f64)
}

pub fn avionics_total_value(components: &[AvionicsComponent]) -> Result<f64, String> {
    components
        .iter()
        .map(avionics_component_value)
        .try_fold(0.0, |sum, value| value.map(|value| sum + value))
}

pub fn avionics_replacement_basis(components: &[AvionicsComponent]) -> Result<f64, String> {
    components
        .iter()
        .map(|component| {
            require_non_negative(
                &format!("{}.unit_replacement_cost_usd", component.name),
                component.unit_replacement_cost_usd,
            )?;
            if component.quantity < 1 {
                return Err(format!("{}.quantity must be at least 1", component.name));
            }
            let dollar_basis_factor = nominal_dollar_factor(
                component.value_reference_year,
                component.valuation_year,
                component.average_inflation_rate,
            )?;
            Ok(component.unit_replacement_cost_usd
                * dollar_basis_factor
                * component.quantity as f64)
        })
        .try_fold(0.0, |sum, value| value.map(|value| sum + value))
}

fn require_non_negative(name: &str, value: f64) -> Result<(), String> {
    if value < 0.0 {
        Err(format!("{name} must be non-negative"))
    } else {
        Ok(())
    }
}

fn require_positive(name: &str, value: f64) -> Result<(), String> {
    if value <= 0.0 {
        Err(format!("{name} must be positive"))
    } else {
        Ok(())
    }
}

fn require_unit_interval_left_closed(name: &str, value: f64) -> Result<(), String> {
    if (0.0..1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name} must be in [0, 1)"))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        airframe_utilization_factor, avionics_component_value, default_avionics_profile,
        estimate_aircraft_value, estimate_aircraft_value_in_year, get_aircraft_profile,
        nominal_dollar_factor, timed_component_adjustment, AvionicsComponent, DollarBasis,
        TimedComponent, DEFAULT_ANNUAL_AIRFRAME_HOURS,
    };

    #[test]
    fn component_adjustment_uses_half_life_baseline() {
        let engine = TimedComponent {
            name: "engine".to_string(),
            hours_since_overhaul: 0.0,
            tbo_hours: 2000.0,
            overhaul_cost_usd: 40_000.0,
            value_reference_year: 0,
            valuation_year: 0,
            average_inflation_rate: 0.0,
            count: 1,
            baseline_life_fraction: 0.5,
        };
        assert_eq!(timed_component_adjustment(Some(&engine)).unwrap(), 20_000.0);

        let runout = TimedComponent {
            hours_since_overhaul: 2500.0,
            ..engine
        };
        assert_eq!(
            timed_component_adjustment(Some(&runout)).unwrap(),
            -20_000.0
        );
    }

    #[test]
    fn component_adjustment_uses_component_dollar_basis() {
        let engine = TimedComponent {
            name: "engine".to_string(),
            hours_since_overhaul: 0.0,
            tbo_hours: 2000.0,
            overhaul_cost_usd: 40_000.0,
            value_reference_year: 2026,
            valuation_year: 2027,
            average_inflation_rate: 0.10,
            count: 1,
            baseline_life_fraction: 0.5,
        };

        assert!((timed_component_adjustment(Some(&engine)).unwrap() - 22_000.0).abs() < 0.01);
    }

    #[test]
    fn airframe_factor_matches_reference_order_of_magnitude() {
        let factor_20_percent_high =
            airframe_utilization_factor(120.0, 100.0, 0.13, 0.12, 0.25).unwrap();
        let factor_double = airframe_utilization_factor(200.0, 100.0, 0.13, 0.12, 0.25).unwrap();

        assert!((factor_20_percent_high - 0.964).abs() < 0.001);
        assert!((factor_double - 0.87).abs() < 0.001);
    }

    #[test]
    fn estimate_combines_airframe_timed_components_and_avionics() {
        let estimate = estimate_aircraft_value(
            500_000.0,
            10.0,
            1800.0,
            get_aircraft_profile("light_piston").unwrap(),
            Some(TimedComponent {
                name: "engine".to_string(),
                hours_since_overhaul: 1800.0,
                tbo_hours: 2000.0,
                overhaul_cost_usd: 42_000.0,
                value_reference_year: 0,
                valuation_year: 0,
                average_inflation_rate: 0.0,
                count: 1,
                baseline_life_fraction: 0.5,
            }),
            Some(TimedComponent {
                name: "propeller".to_string(),
                hours_since_overhaul: 300.0,
                tbo_hours: 2400.0,
                overhaul_cost_usd: 6000.0,
                value_reference_year: 0,
                valuation_year: 0,
                average_inflation_rate: 0.0,
                count: 1,
                baseline_life_fraction: 0.5,
            }),
            &[AvionicsComponent {
                name: "Garmin GTN 750Xi".to_string(),
                introduced_year: 2020,
                valuation_year: 2026,
                value_reference_year: 2026,
                average_inflation_rate: 0.0,
                unit_replacement_cost_usd: 20_000.0,
                quantity: 1,
                profile: default_avionics_profile(),
            }],
        )
        .unwrap();

        assert!(estimate.estimated_value_usd > 0.0);
        assert!(estimate.estimated_value_usd < estimate.breakdown.valuation_basis_usd);
        assert!(estimate.breakdown.engine_adjustment_usd < 0.0);
        assert!(estimate.breakdown.propeller_adjustment_usd > 0.0);
        assert!(estimate.breakdown.avionics_value_usd > 0.0);
    }

    #[test]
    fn avionics_reference_year_value_is_not_double_depreciated_from_release_year() {
        let profile = get_aircraft_profile("light_piston").unwrap();
        let old_avionics = vec![AvionicsComponent {
            name: "Legacy GPS".to_string(),
            introduced_year: 1998,
            valuation_year: 2026,
            value_reference_year: 2026,
            average_inflation_rate: 0.0,
            unit_replacement_cost_usd: 20_000.0,
            quantity: 1,
            profile: default_avionics_profile(),
        }];
        let old_estimate =
            estimate_aircraft_value(300_000.0, 35.0, 3800.0, profile, None, None, &old_avionics)
                .unwrap();

        assert!((old_estimate.breakdown.avionics_value_usd - 20_000.0).abs() < 0.01);
    }

    #[test]
    fn avionics_depreciate_after_value_reference_year() {
        let current = AvionicsComponent {
            name: "Current panel".to_string(),
            introduced_year: 2000,
            valuation_year: 2026,
            value_reference_year: 2026,
            average_inflation_rate: 0.0,
            unit_replacement_cost_usd: 15_000.0,
            quantity: 1,
            profile: default_avionics_profile(),
        };
        let future = AvionicsComponent {
            valuation_year: 2036,
            ..current.clone()
        };

        assert!(
            avionics_component_value(&future).unwrap()
                < avionics_component_value(&current).unwrap()
        );
    }

    #[test]
    fn nominal_dollar_factor_deflates_past_and_inflates_future() {
        let past = nominal_dollar_factor(2026, 2006, 0.025).unwrap();
        let future = nominal_dollar_factor(2026, 2056, 0.025).unwrap();

        assert!((0.60..0.62).contains(&past));
        assert!((2.09..2.11).contains(&future));
    }

    #[test]
    fn valuation_year_uses_nominal_dollars_for_that_year() {
        let current = estimate_aircraft_value_in_year(
            950_000.0,
            0.0,
            0.0,
            DEFAULT_ANNUAL_AIRFRAME_HOURS,
            get_aircraft_profile("complex_piston").unwrap(),
            None,
            None,
            &[],
            None,
            DollarBasis {
                value_reference_year: 2026,
                valuation_year: 2026,
                average_inflation_rate: 0.025,
            },
        )
        .unwrap();
        let historical = estimate_aircraft_value_in_year(
            950_000.0,
            0.0,
            0.0,
            DEFAULT_ANNUAL_AIRFRAME_HOURS,
            get_aircraft_profile("complex_piston").unwrap(),
            None,
            None,
            &[],
            None,
            DollarBasis {
                value_reference_year: 2026,
                valuation_year: 2006,
                average_inflation_rate: 0.025,
            },
        )
        .unwrap();

        assert_eq!(current.breakdown.dollar_basis_factor, 1.0);
        assert!(historical.breakdown.effective_new_price_usd < 590_000.0);
        assert!(historical.estimated_value_usd < current.estimated_value_usd);
    }

    #[test]
    fn replacement_floor_is_not_capped_by_original_new_price_basis() {
        let mut profile = get_aircraft_profile("light_piston").unwrap();
        profile.replacement_floor_fraction = 0.30;

        let estimate = estimate_aircraft_value_in_year(
            35_000.0,
            49.0,
            2400.0,
            DEFAULT_ANNUAL_AIRFRAME_HOURS,
            profile,
            None,
            None,
            &[],
            Some(700_000.0),
            DollarBasis {
                value_reference_year: 1977,
                valuation_year: 2026,
                average_inflation_rate: 0.025,
            },
        )
        .unwrap();

        assert_eq!(estimate.breakdown.replacement_floor_value_usd, 210_000.0);
        assert!(estimate.breakdown.effective_new_price_usd < 120_000.0);
        assert_eq!(
            estimate.estimated_value_usd,
            estimate.breakdown.replacement_floor_value_usd
        );
        assert_eq!(
            estimate.breakdown.valuation_basis_usd,
            estimate.breakdown.replacement_floor_value_usd
        );
    }
}
