//! Shared avionics model-label relationship semantics.
//!
//! These relations are deliberately narrower than fuzzy similarity. They
//! preserve meaningful product suffixes while recognizing harmless typography
//! and a complete, server-owned capability used as a trailing description.

use crate::extract::CURATED_AVIONICS_TYPES;
use crate::normalize::{normalize_avionics_identifier, normalize_name};

const MINIMUM_PREFIX_MODEL_KEY_LENGTH: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AvionicsModelIdentityRelation {
    TypographyExact,
    DescriptiveExpansion,
    MeaningfulVariant,
}

/// Classify the structural relationship between two same-manufacturer model
/// labels without independently authorizing either identity.
///
/// Exact manufacturer prefixes are redundant retrieval syntax. A trailing
/// descriptive expansion is harmless only when it is one complete
/// server-owned capability already attached to the compared products. Every
/// other prefix/suffix delta is a meaningful variant by default; no suffix
/// vocabulary can safely enumerate future hardware revisions.
pub(crate) fn avionics_model_identity_relation(
    left_manufacturer: &str,
    left_model: &str,
    left_types: &[String],
    right_manufacturer: &str,
    right_model: &str,
    right_types: &[String],
) -> Option<AvionicsModelIdentityRelation> {
    let left_label = model_label_without_exact_manufacturer_prefix(left_manufacturer, left_model);
    let right_label =
        model_label_without_exact_manufacturer_prefix(right_manufacturer, right_model);
    let left_key = normalize_avionics_identifier(&left_label);
    let right_key = normalize_avionics_identifier(&right_label);
    if left_key.is_empty() || right_key.is_empty() {
        return None;
    }
    if left_key == right_key {
        return Some(AvionicsModelIdentityRelation::TypographyExact);
    }

    let mut descriptive_phrases = left_types
        .iter()
        .chain(right_types)
        .filter(|capability| CURATED_AVIONICS_TYPES.contains(&capability.as_str()))
        .map(|capability| normalize_name(capability))
        .filter(|capability| !capability.is_empty())
        .collect::<Vec<_>>();
    descriptive_phrases.sort_by(|left, right| {
        right
            .split_whitespace()
            .count()
            .cmp(&left.split_whitespace().count())
            .then_with(|| right.len().cmp(&left.len()))
            .then_with(|| left.cmp(right))
    });
    descriptive_phrases.dedup();

    let (left_core, left_description) =
        strip_exact_capability_description(&left_label, &descriptive_phrases);
    let (right_core, right_description) =
        strip_exact_capability_description(&right_label, &descriptive_phrases);
    let left_core_key = normalize_avionics_identifier(&left_core);
    let right_core_key = normalize_avionics_identifier(&right_core);
    if left_core_key.is_empty() || right_core_key.is_empty() {
        return None;
    }
    if left_core_key == right_core_key {
        let one_side_is_descriptive = left_description.is_some() ^ right_description.is_some();
        let matching_descriptions =
            left_description.is_some() && left_description == right_description;
        return Some(if one_side_is_descriptive || matching_descriptions {
            AvionicsModelIdentityRelation::DescriptiveExpansion
        } else {
            AvionicsModelIdentityRelation::MeaningfulVariant
        });
    }

    let left_numbers = model_numeric_runs(&left_core_key);
    let right_numbers = model_numeric_runs(&right_core_key);
    if left_numbers != right_numbers {
        return None;
    }

    let shorter_key_length = left_core_key.len().min(right_core_key.len());
    (shorter_key_length >= MINIMUM_PREFIX_MODEL_KEY_LENGTH
        && (left_core_key.starts_with(&right_core_key)
            || right_core_key.starts_with(&left_core_key)))
    .then_some(AvionicsModelIdentityRelation::MeaningfulVariant)
}

fn model_label_without_exact_manufacturer_prefix(manufacturer: &str, model: &str) -> String {
    let model = normalize_name(model);
    let manufacturer = normalize_name(manufacturer);
    if manufacturer.is_empty() {
        return model;
    }
    model
        .strip_prefix(&format!("{manufacturer} "))
        .unwrap_or(&model)
        .to_string()
}

fn strip_exact_capability_description(
    model: &str,
    descriptive_phrases: &[String],
) -> (String, Option<String>) {
    for description in descriptive_phrases {
        let Some(core) = model.strip_suffix(&format!(" {description}")) else {
            continue;
        };
        if core.chars().any(|character| character.is_ascii_digit())
            && normalize_avionics_identifier(core).len() >= MINIMUM_PREFIX_MODEL_KEY_LENGTH
        {
            return (core.to_string(), Some(description.clone()));
        }
    }
    (model.to_string(), None)
}

pub(crate) fn model_numeric_runs(model_key: &str) -> Vec<&str> {
    let mut runs = Vec::new();
    let mut start = None;
    for (index, character) in model_key.char_indices() {
        if character.is_ascii_digit() {
            start.get_or_insert(index);
        } else if let Some(start) = start.take() {
            runs.push(&model_key[start..index]);
        }
    }
    if let Some(start) = start {
        runs.push(&model_key[start..]);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::{avionics_model_identity_relation, AvionicsModelIdentityRelation};

    #[test]
    fn descriptions_are_distinct_from_meaningful_product_suffixes() {
        let integrated_flight_deck = vec!["Integrated Flight Deck".to_string()];
        assert_eq!(
            avionics_model_identity_relation(
                "Garmin",
                "Garmin G1000",
                &integrated_flight_deck,
                "Garmin",
                "G1000",
                &integrated_flight_deck,
            ),
            Some(AvionicsModelIdentityRelation::TypographyExact)
        );
        assert_eq!(
            avionics_model_identity_relation(
                "Garmin",
                "G1000 Integrated Flight Deck",
                &integrated_flight_deck,
                "Garmin",
                "G1000",
                &integrated_flight_deck,
            ),
            Some(AvionicsModelIdentityRelation::DescriptiveExpansion)
        );

        for (base, variant) in [
            ("G1000", "G1000 NXi"),
            ("GIA 63", "GIA 63W"),
            ("KX 155", "KX 155A"),
            ("GTX 345", "GTX 345R"),
            ("GTN 650", "GTN 650Xi"),
            ("GTX 33", "GTX 33ES"),
            ("GNS 430", "GNS 430WAAS"),
        ] {
            assert_eq!(
                avionics_model_identity_relation("Garmin", base, &[], "Garmin", variant, &[]),
                Some(AvionicsModelIdentityRelation::MeaningfulVariant),
                "{base:?} and {variant:?} must remain distinct"
            );
        }
    }

    #[test]
    fn different_numeric_models_and_lru_components_are_unrelated() {
        assert_eq!(
            avionics_model_identity_relation(
                "Garmin",
                "G1000",
                &[],
                "Garmin",
                "G1000 (GDU 1040)",
                &[],
            ),
            None
        );
        assert_eq!(
            avionics_model_identity_relation("Garmin", "GIA 63W", &[], "Garmin", "GIA 64W", &[],),
            None
        );
    }
}
