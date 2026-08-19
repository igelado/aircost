//! Canonical integrity rules for the set of avionics deltas on one listing.
//!
//! `avionics_model_id` remains the action subject. A removal therefore names
//! the removed product as both its subject and displaced target; it does not
//! install that subject. These rules operate on canonical approved-product
//! keys so two raw catalog IDs cannot evade graph validation.

use std::collections::HashSet;

use crate::normalize::{normalize_avionics_manufacturer_name, normalize_avionics_model_name};

pub(crate) mod disposition;
pub(crate) mod extraction;

#[derive(Clone, Debug)]
pub(crate) struct CanonicalAvionicsAction {
    pub subject_key: String,
    pub configuration_action: String,
    pub displaced_key: Option<String>,
}

impl CanonicalAvionicsAction {
    pub(crate) fn new(
        subject_key: impl Into<String>,
        configuration_action: impl Into<String>,
        displaced_key: Option<String>,
    ) -> Self {
        Self {
            subject_key: subject_key.into(),
            configuration_action: configuration_action.into(),
            displaced_key,
        }
    }
}

/// Construct a graph key from the persisted approved-product identity.
///
/// Persisted paths must use this constructor with values loaded from
/// `avionics_approved_product_graph_identities`; display labels are not stable
/// identities and must never select graph equality.
pub(crate) fn approved_avionics_product_key(
    manufacturer_identity_id: i64,
    canonical_product_key: &str,
) -> Result<String, String> {
    if manufacturer_identity_id <= 0 {
        return Err("approved avionics manufacturer identity id must be positive".to_string());
    }
    if canonical_product_key.is_empty()
        || !canonical_product_key
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
    {
        return Err("approved avionics product key is not canonical".to_string());
    }
    Ok(format!(
        "{manufacturer_identity_id}\u{1f}{canonical_product_key}"
    ))
}

/// Preview-only fallback for a not-yet-persisted Gemini proposal. Once a
/// product has a catalog ID, callers must load its approved identity and use
/// [`approved_avionics_product_key`] instead.
pub(crate) fn preview_avionics_product_key(manufacturer: &str, model: &str) -> String {
    let manufacturer = compact_key(&normalize_avionics_manufacturer_name(manufacturer));
    let model = compact_key(&normalize_avionics_model_name(model));
    format!("preview:{manufacturer}\u{1f}{model}")
}

fn compact_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect()
}

/// Validate a listing's avionics deltas as one set rather than independent
/// rows. Callers must additionally enforce exact raw-ID equality for stored
/// removals and exact raw-ID inequality for stored replacements.
pub(crate) fn validate_canonical_avionics_actions(
    actions: &[CanonicalAvionicsAction],
) -> Result<(), String> {
    let mut installed_subjects = HashSet::new();
    let mut displacement_targets = HashSet::new();

    for action in actions {
        if action.subject_key.is_empty() || action.subject_key == "\u{1f}" {
            return Err("avionics action subject has no canonical product identity".to_string());
        }
        match action.configuration_action.as_str() {
            "installed" => {
                if action.displaced_key.is_some() {
                    return Err(format!(
                        "installed canonical avionics product {:?} cannot displace another product",
                        action.subject_key
                    ));
                }
                if !installed_subjects.insert(action.subject_key.as_str()) {
                    return Err(format!(
                        "canonical avionics product {:?} is installed by more than one listing action",
                        action.subject_key
                    ));
                }
            }
            "replaces" => {
                let displaced = action.displaced_key.as_deref().ok_or_else(|| {
                    format!(
                        "replacement canonical avionics product {:?} has no displaced target",
                        action.subject_key
                    )
                })?;
                if displaced == action.subject_key {
                    return Err(format!(
                        "canonical avionics product {:?} cannot replace itself",
                        action.subject_key
                    ));
                }
                if !installed_subjects.insert(action.subject_key.as_str()) {
                    return Err(format!(
                        "canonical avionics product {:?} is installed by more than one listing action",
                        action.subject_key
                    ));
                }
                if !displacement_targets.insert(displaced) {
                    return Err(format!(
                        "canonical avionics product {displaced:?} is displaced by more than one listing action"
                    ));
                }
            }
            "removes" => {
                let displaced = action.displaced_key.as_deref().ok_or_else(|| {
                    format!(
                        "removed canonical avionics product {:?} has no displaced target",
                        action.subject_key
                    )
                })?;
                if displaced != action.subject_key {
                    return Err(format!(
                        "removal must identify canonical avionics product {:?} as both subject and displaced target",
                        action.subject_key
                    ));
                }
                if !displacement_targets.insert(displaced) {
                    return Err(format!(
                        "canonical avionics product {displaced:?} is displaced by more than one listing action"
                    ));
                }
            }
            unsupported => {
                return Err(format!(
                    "unsupported avionics configuration action {unsupported:?}"
                ))
            }
        }
    }

    if let Some(identity) = installed_subjects
        .intersection(&displacement_targets)
        .next()
    {
        return Err(format!(
            "canonical avionics product {identity:?} cannot be both installed and displaced by one listing delta"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(subject: &str, kind: &str, displaced: Option<&str>) -> CanonicalAvionicsAction {
        CanonicalAvionicsAction::new(subject, kind, displaced.map(ToString::to_string))
    }

    #[test]
    fn canonical_product_key_ignores_supported_typography_variants() {
        assert_eq!(
            preview_avionics_product_key("Bendix/King", "KX-155"),
            preview_avionics_product_key("Bendix King", "KX 155")
        );
    }

    #[test]
    fn approved_key_requires_persisted_identity_components() {
        assert_eq!(
            approved_avionics_product_key(42, "gns430w").unwrap(),
            "42\u{1f}gns430w"
        );
        assert!(approved_avionics_product_key(0, "gns430w").is_err());
        assert!(approved_avionics_product_key(42, "GNS-430W").is_err());
    }

    #[test]
    fn pure_removal_is_valid_but_self_replacement_is_not() {
        assert!(validate_canonical_avionics_actions(&[action(
            "garmin\u{1f}gns430w",
            "removes",
            Some("garmin\u{1f}gns430w"),
        )])
        .is_ok());
        assert!(validate_canonical_avionics_actions(&[action(
            "garmin\u{1f}gns430w",
            "replaces",
            Some("garmin\u{1f}gns430w"),
        )])
        .is_err());
    }

    #[test]
    fn duplicate_displacement_and_installed_subjects_are_rejected() {
        let duplicate_target = [
            action(
                "garmin\u{1f}gnc355",
                "replaces",
                Some("garmin\u{1f}gns430w"),
            ),
            action(
                "avidyne\u{1f}ifd440",
                "replaces",
                Some("garmin\u{1f}gns430w"),
            ),
        ];
        assert!(validate_canonical_avionics_actions(&duplicate_target).is_err());

        let duplicate_subject = [
            action("garmin\u{1f}gnc355", "installed", None),
            action("garmin\u{1f}gnc355", "installed", None),
        ];
        assert!(validate_canonical_avionics_actions(&duplicate_subject).is_err());
    }

    #[test]
    fn replacement_cycles_and_chains_are_rejected() {
        let cycle = [
            action("maker\u{1f}a", "replaces", Some("maker\u{1f}b")),
            action("maker\u{1f}b", "replaces", Some("maker\u{1f}a")),
        ];
        assert!(validate_canonical_avionics_actions(&cycle).is_err());

        let chain = [
            action("maker\u{1f}c", "replaces", Some("maker\u{1f}b")),
            action("maker\u{1f}b", "replaces", Some("maker\u{1f}a")),
        ];
        assert!(validate_canonical_avionics_actions(&chain).is_err());
    }
}
