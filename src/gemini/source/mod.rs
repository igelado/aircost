//! Bounded publisher-source structures used by deterministic grounding.

pub(crate) mod pdf;

use crate::gemini::interactions::{GeminiInteractionsError, GeminiInteractionsResult};

pub(crate) const MAX_TEXT_ROWS: usize = 16_384;
pub(crate) const MAX_TEXT_ROW_CHARACTERS: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProductIdentityTarget {
    model_key: String,
    manufacturer_identifier_key: String,
}

impl ProductIdentityTarget {
    pub(crate) fn new(
        model: &str,
        manufacturer_identifier: &str,
    ) -> GeminiInteractionsResult<Self> {
        let model_key = compact_identity_key(model);
        let manufacturer_identifier_key = compact_identity_key(manufacturer_identifier);
        if model_key.is_empty() || manufacturer_identifier_key.is_empty() {
            return Err(GeminiInteractionsError::InvalidResponse(
                "source product target requires a concrete model and manufacturer identifier"
                    .to_string(),
            ));
        }
        Ok(Self {
            model_key,
            manufacturer_identifier_key,
        })
    }

    pub(super) fn row_is_relevant(&self, text: &str) -> bool {
        exact_identity_component_is_present(text, &self.model_key)
            || exact_identity_component_is_present(text, &self.manufacturer_identifier_key)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TextRow {
    pub(crate) kind: TextRowKind,
    pub(crate) ordinal: usize,
    pub(crate) text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextRowKind {
    HtmlTableRow,
    PdfPhysicalLine,
    PdfVisualRow,
}

pub(super) fn normalize_text_row(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() && !character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn compact_identity_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn exact_identity_component_is_present(text: &str, identity_key: &str) -> bool {
    if identity_key.is_empty() {
        return false;
    }
    let tokens = text
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect::<Vec<_>>();
    tokens.iter().enumerate().any(|(start, _)| {
        let mut joined = String::new();
        for token in &tokens[start..] {
            joined.push_str(token);
            if joined == identity_key {
                return true;
            }
            if joined.len() >= identity_key.len() {
                return false;
            }
        }
        false
    })
}
