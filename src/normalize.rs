use html_escape::decode_html_entities;

pub fn normalize_name(value: &str) -> String {
    let decoded = decode_html_entities(value).to_lowercase();
    let mut cleaned = String::with_capacity(decoded.len());
    for character in decoded.chars() {
        if character.is_ascii_alphanumeric() {
            cleaned.push(character);
        } else {
            cleaned.push(' ');
        }
    }

    let parts = cleaned
        .split_whitespace()
        .filter(|part| !is_legal_suffix(part))
        .collect::<Vec<_>>();
    let normalized = parts.join(" ");
    match normalized.as_str() {
        "cessna aircraft" | "cessna aircraft company" | "textron aviation" => "cessna".to_string(),
        "cirrus aircraft" | "cirrus design" => "cirrus".to_string(),
        "the air plane factory" | "sling aircraft" | "sling airplane" => "sling".to_string(),
        _ => normalized,
    }
}

pub fn canonical_manufacturer_name(value: &str) -> String {
    match normalize_name(value).as_str() {
        "cessna" => "Cessna".to_string(),
        "cirrus" => "Cirrus".to_string(),
        "sling" => "Sling".to_string(),
        _ => value.trim().to_string(),
    }
}

pub fn normalize_avionics_model_name(value: &str) -> String {
    let normalized = normalize_name(value);
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let mut normalized_tokens = Vec::with_capacity(tokens.len());
    let mut index = 0;

    while index < tokens.len() {
        let token = tokens[index];
        if let Some(next) = tokens.get(index + 1) {
            if should_merge_avionics_code_tokens(token, next) {
                normalized_tokens.push(format!("{token}{next}"));
                index += 2;
                continue;
            }
        }
        normalized_tokens.push(token.to_string());
        index += 1;
    }

    normalized_tokens.join(" ")
}

pub fn is_generic_avionics_model_name(value: &str) -> bool {
    normalize_avionics_model_name(value).is_empty()
}

pub fn is_generic_avionics_manufacturer_name(value: &str) -> bool {
    matches!(
        normalize_name(value).as_str(),
        "" | "unknown"
            | "generic"
            | "standard"
            | "factory"
            | "oem"
            | "various"
            | "multiple"
            | "manufacturer"
            | "avionics"
            | "nav com"
            | "radios"
            | "radio"
            | "equipment"
    )
}

pub fn is_usable_avionics_label(manufacturer: &str, model: &str) -> bool {
    !is_generic_avionics_manufacturer_name(manufacturer) && !is_generic_avionics_model_name(model)
}

fn should_merge_avionics_code_tokens(left: &str, right: &str) -> bool {
    let left_is_short_alpha_prefix = left.len() <= 4
        && left
            .chars()
            .all(|character| character.is_ascii_alphabetic());
    let right_is_short_alpha_suffix = right.len() <= 4
        && right
            .chars()
            .all(|character| character.is_ascii_alphabetic());
    let left_has_digit = left.chars().any(|character| character.is_ascii_digit());
    let right_has_digit = right.chars().any(|character| character.is_ascii_digit());

    (left_is_short_alpha_prefix && right_has_digit)
        || (left_has_digit && right_is_short_alpha_suffix)
}

fn is_legal_suffix(value: &str) -> bool {
    matches!(
        value,
        "co" | "company"
            | "corp"
            | "corporation"
            | "inc"
            | "incorporated"
            | "llc"
            | "ltd"
            | "limited"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        is_generic_avionics_manufacturer_name, is_generic_avionics_model_name,
        is_usable_avionics_label, normalize_avionics_model_name, normalize_name,
    };

    #[test]
    fn normalizes_known_manufacturer_aliases() {
        assert_eq!(normalize_name("Cessna Aircraft Company"), "cessna");
        assert_eq!(normalize_name("Cirrus Aircraft"), "cirrus");
        assert_eq!(normalize_name("SR22T-G6"), "sr22t g6");
    }

    #[test]
    fn normalizes_common_avionics_code_spacing() {
        assert_eq!(normalize_avionics_model_name("G1000 NXi"), "g1000nxi");
        assert_eq!(normalize_avionics_model_name("G1000NXi"), "g1000nxi");
        assert_eq!(normalize_avionics_model_name("GDL-69A"), "gdl69a");
        assert_eq!(normalize_avionics_model_name("GDL69A"), "gdl69a");
        assert_eq!(normalize_avionics_model_name("GTX 33"), "gtx33");
        assert_eq!(
            normalize_avionics_model_name("Flight Stream 510"),
            "flight stream 510"
        );
    }

    #[test]
    fn keeps_specific_avionics_labels() {
        for label in [
            "ADS-B",
            "GTX 345R (ADS-B)",
            "GNS 430W",
            "GMA 350c (Bluetooth Audio)",
            "G1000 NXi",
            "S-TEC 55X",
            "WX 500",
            "300A Navomatic",
        ] {
            assert!(
                !is_generic_avionics_model_name(label),
                "{label} should be specific"
            );
        }
    }

    #[test]
    fn identifies_generic_avionics_manufacturers() {
        for manufacturer in ["Unknown", "Generic", "Factory", "Standard", "OEM", "Radios"] {
            assert!(
                is_generic_avionics_manufacturer_name(manufacturer),
                "{manufacturer} should be generic"
            );
        }
        assert!(!is_generic_avionics_manufacturer_name("Garmin"));
        assert!(!is_generic_avionics_manufacturer_name("Bendix/King"));
    }

    #[test]
    fn requires_usable_avionics_manufacturer_and_model() {
        assert!(is_usable_avionics_label("Garmin", "GNS 430W"));
        assert!(!is_usable_avionics_label("Unknown", "GNS 430W"));
        assert!(!is_usable_avionics_label("Garmin", ""));
    }
}
