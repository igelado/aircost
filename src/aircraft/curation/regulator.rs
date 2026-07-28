//! Deterministic, case-bound interpretation of FAA type-certificate evidence.
//!
//! Transport validation belongs to [`crate::aircraft::faa::drs`]. This module
//! consumes one already validated current TCDS and retains only the exact
//! excerpts needed to bind an FAA model, a retained listing model label, and
//! the FAA manufacturer serial to one canonical family.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;
use url::Url;

use crate::aircraft::faa::drs::TcdsDocument;
use crate::aircraft::faa::normalize_serial_key;

/// Regulator-owned proof that one exact FAA designation covers one exact
/// manufacturer serial. A TCDS can prove this without naming a marketing
/// family (for example, the 3A13 heading for Model 182R).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TcdsIdentityBinding {
    pub document_guid: String,
    pub document_url: String,
    pub tcds_number: String,
    pub revision_number: Option<String>,
    pub revision_date: Option<String>,
    pub source_url: String,
    pub pdf_sha256: String,
    pub exact_faa_model: String,
    pub faa_serial_key: String,
    pub faa_model_heading: SelectedTcdsExcerpt,
    pub serial_eligibility: TcdsSerialEligibility,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TcdsFamilyBinding {
    pub document_guid: String,
    pub document_url: String,
    pub tcds_number: String,
    pub revision_number: Option<String>,
    pub revision_date: Option<String>,
    pub source_url: String,
    pub pdf_sha256: String,
    pub exact_faa_model: String,
    pub observed_model: String,
    pub canonical_family_name: String,
    pub faa_serial_key: String,
    pub faa_model_heading: SelectedTcdsExcerpt,
    pub serial_eligibility: TcdsSerialEligibility,
}

impl TcdsFamilyBinding {
    pub(crate) fn identity_binding(&self) -> TcdsIdentityBinding {
        TcdsIdentityBinding {
            document_guid: self.document_guid.clone(),
            document_url: self.document_url.clone(),
            tcds_number: self.tcds_number.clone(),
            revision_number: self.revision_number.clone(),
            revision_date: self.revision_date.clone(),
            source_url: self.source_url.clone(),
            pdf_sha256: self.pdf_sha256.clone(),
            exact_faa_model: self.exact_faa_model.clone(),
            faa_serial_key: self.faa_serial_key.clone(),
            faa_model_heading: self.faa_model_heading.clone(),
            serial_eligibility: self.serial_eligibility.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct SelectedTcdsExcerpt {
    pub page_number: u32,
    pub excerpt: String,
    pub normalized_excerpt_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TcdsSerialEligibility {
    pub page_number: u32,
    pub excerpt: String,
    pub normalized_excerpt_sha256: String,
    pub model: String,
    pub first_serial_key: String,
    pub last_serial_key: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TcdsMakeLineageEvidence {
    pub document_guid: String,
    pub tcds_number: String,
    pub source_url: String,
    pub pdf_sha256: String,
    pub exact_faa_model: String,
    pub faa_serial_key: String,
    pub manufacturer_serial_eligibility: Option<TcdsManufacturerSerialEligibility>,
    pub holder_transfer: Option<TcdsHolderTransferEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TcdsManufacturerSerialEligibility {
    pub page_number: u32,
    pub excerpt: String,
    pub normalized_excerpt_sha256: String,
    pub manufacturer_name: String,
    pub model: String,
    pub first_serial_key: String,
    pub last_serial_key: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TcdsHolderTransferEvidence {
    pub page_number: u32,
    pub excerpt: String,
    pub normalized_excerpt_sha256: String,
    pub former_holder_name: String,
    pub current_holder_name: String,
    pub effective_date_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TcdsFamilyBindingError {
    InvalidDocument(&'static str),
    InvalidInput(&'static str),
    ModelHeadingMissing,
    ModelHeadingAmbiguous,
    SerialEligibilityMissing,
    SerialEligibilityAmbiguous,
    SerialNotEligible,
    MakeLineageAmbiguous,
    HolderTransferAmbiguous,
}

impl fmt::Display for TcdsFamilyBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDocument(field) => {
                write!(formatter, "FAA TCDS {field} is invalid")
            }
            Self::InvalidInput(field) => write!(formatter, "{field} is invalid"),
            Self::ModelHeadingMissing => {
                formatter.write_str("exact FAA model family heading is missing")
            }
            Self::ModelHeadingAmbiguous => {
                formatter.write_str("exact FAA model has ambiguous family headings")
            }
            Self::SerialEligibilityMissing => {
                formatter.write_str("exact FAA model serial eligibility is missing")
            }
            Self::SerialEligibilityAmbiguous => {
                formatter.write_str("exact FAA model serial eligibility is ambiguous")
            }
            Self::SerialNotEligible => {
                formatter.write_str("FAA manufacturer serial is outside TCDS eligibility")
            }
            Self::MakeLineageAmbiguous => {
                formatter.write_str("FAA TCDS manufacturer-specific serial lineage is ambiguous")
            }
            Self::HolderTransferAmbiguous => {
                formatter.write_str("FAA TCDS holder-transfer record is ambiguous")
            }
        }
    }
}

impl std::error::Error for TcdsFamilyBindingError {}

/// Bind an exact FAA designation and serial without inventing a model family.
///
/// This is the regulator-owned identity layer. Callers that require a named
/// family must additionally use [`bind_tcds_family`], whose stronger contract
/// rejects headings that contain only capacity or certification descriptors.
pub(crate) fn bind_tcds_identity(
    document: &TcdsDocument,
    exact_faa_model: &str,
    faa_serial: &str,
) -> Result<TcdsIdentityBinding, TcdsFamilyBindingError> {
    validate_case_input(exact_faa_model, "exact FAA model")?;
    validate_document(document, exact_faa_model)?;

    let faa_serial_key = normalize_serial_key(faa_serial)
        .filter(|value| value.len() <= 64)
        .ok_or(TcdsFamilyBindingError::InvalidInput("FAA serial"))?;
    let faa_model_heading = find_unique_designation_heading(document, exact_faa_model)?;
    let serial_eligibility =
        find_unique_serial_eligibility(document, exact_faa_model, &faa_serial_key)?;

    Ok(TcdsIdentityBinding {
        document_guid: document.metadata.document_guid.clone(),
        document_url: document.metadata.document_url.clone(),
        tcds_number: document.metadata.tcds_number.clone(),
        revision_number: document.metadata.revision_number.clone(),
        revision_date: document.metadata.revision_date.clone(),
        source_url: document.source_url.clone(),
        pdf_sha256: document.pdf_sha256.clone(),
        exact_faa_model: exact_faa_model.to_string(),
        faa_serial_key,
        faa_model_heading,
        serial_eligibility,
    })
}

/// Build a regulator-owned family binding without deriving labels or dates.
pub(crate) fn bind_tcds_family(
    document: &TcdsDocument,
    exact_faa_model: &str,
    observed_model: &str,
    faa_serial: &str,
) -> Result<TcdsFamilyBinding, TcdsFamilyBindingError> {
    validate_case_input(observed_model, "observed model")?;
    let identity = bind_tcds_identity(document, exact_faa_model, faa_serial)?;
    let faa_model_heading = find_unique_heading(document, exact_faa_model)?;

    Ok(TcdsFamilyBinding {
        document_guid: identity.document_guid,
        document_url: identity.document_url,
        tcds_number: identity.tcds_number,
        revision_number: identity.revision_number,
        revision_date: identity.revision_date,
        source_url: identity.source_url,
        pdf_sha256: identity.pdf_sha256,
        exact_faa_model: identity.exact_faa_model,
        observed_model: observed_model.to_string(),
        canonical_family_name: faa_model_heading.family,
        faa_serial_key: identity.faa_serial_key,
        faa_model_heading: identity.faa_model_heading,
        serial_eligibility: identity.serial_eligibility,
    })
}

/// Select exact TCDS evidence that a particular manufacturer name applies to
/// this exact model and serial. When the same document contains one
/// unambiguous holder-transfer record, retain it as a separate exact excerpt.
///
/// Absence is not an error: many TCDSs prove model eligibility without a
/// manufacturer-specific serial table. Ambiguous matching evidence fails
/// closed, and callers must never derive a make relationship from holder names
/// or listing text when this hook returns `None`.
pub(crate) fn bind_tcds_make_lineage(
    document: &TcdsDocument,
    exact_faa_model: &str,
    faa_serial: &str,
) -> Result<Option<TcdsMakeLineageEvidence>, TcdsFamilyBindingError> {
    let identity = bind_tcds_identity(document, exact_faa_model, faa_serial)?;
    let manufacturer_serial_eligibility = find_unique_manufacturer_serial_eligibility(
        document,
        exact_faa_model,
        &identity.faa_serial_key,
    )?;
    let holder_transfer = find_unique_holder_transfer(document)?;
    if holder_transfer.as_ref().is_some_and(|holder| {
        document
            .metadata
            .tc_holder
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|current| {
                !super::tcds_holder_names_match(current, &holder.current_holder_name)
            })
            || (!document.metadata.former_tc_holders.is_empty()
                && !document.metadata.former_tc_holders.iter().any(|former| {
                    super::tcds_holder_names_match(former, &holder.former_holder_name)
                }))
    }) {
        return Err(TcdsFamilyBindingError::HolderTransferAmbiguous);
    }
    if manufacturer_serial_eligibility.is_none() && holder_transfer.is_none() {
        return Ok(None);
    }
    Ok(Some(TcdsMakeLineageEvidence {
        document_guid: document.metadata.document_guid.clone(),
        tcds_number: document.metadata.tcds_number.clone(),
        source_url: document.source_url.clone(),
        pdf_sha256: document.pdf_sha256.clone(),
        exact_faa_model: identity.exact_faa_model,
        faa_serial_key: identity.faa_serial_key,
        manufacturer_serial_eligibility,
        holder_transfer,
    }))
}

fn normalized_excerpt_sha256(excerpt: &str) -> String {
    let normalized = excerpt.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{:x}", Sha256::digest(normalized.as_bytes()))
}

fn validate_case_input(value: &str, field: &'static str) -> Result<(), TcdsFamilyBindingError> {
    if value.is_empty()
        || value.len() > 128
        || value.trim() != value
        || !value.is_ascii()
        || value.chars().any(char::is_control)
    {
        return Err(TcdsFamilyBindingError::InvalidInput(field));
    }
    Ok(())
}

fn validate_document(
    document: &TcdsDocument,
    exact_faa_model: &str,
) -> Result<(), TcdsFamilyBindingError> {
    let metadata = &document.metadata;
    if !is_uuid(&metadata.document_guid)
        || metadata.tcds_number.trim().is_empty()
        || metadata.exact_model != exact_faa_model
        || metadata
            .models
            .iter()
            .filter(|model| model.as_str() == exact_faa_model)
            .count()
            != 1
        || document.pdf_size_bytes == 0
        || document.page_count == 0
        || document.page_count != document.pages.len()
        || !is_lower_hex_sha256(&document.pdf_sha256)
    {
        return Err(TcdsFamilyBindingError::InvalidDocument(
            "metadata or PDF provenance",
        ));
    }
    if !is_drs_url(&metadata.document_url) || !is_drs_url(&document.source_url) {
        return Err(TcdsFamilyBindingError::InvalidDocument("source URL"));
    }
    let mut page_numbers = document
        .pages
        .iter()
        .map(|page| page.page_number)
        .collect::<Vec<_>>();
    page_numbers.sort_unstable();
    page_numbers.dedup();
    if page_numbers.len() != document.pages.len()
        || document.pages.iter().any(|page| page.page_number == 0)
    {
        return Err(TcdsFamilyBindingError::InvalidDocument(
            "physical page text",
        ));
    }
    Ok(())
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn is_drs_url(value: &str) -> bool {
    Url::parse(value).ok().is_some_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("drs.faa.gov")
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}

#[derive(Clone, Debug)]
struct HeadingBinding {
    family: String,
}

#[derive(Clone, Debug)]
struct HeadingCandidate {
    model: String,
    family: String,
    page_number: u32,
    line_start: usize,
    excerpt: String,
}

#[derive(Clone, Debug)]
struct DesignationHeadingCandidate {
    page_number: u32,
    line_start: usize,
    excerpt: String,
}

fn find_unique_designation_heading(
    document: &TcdsDocument,
    exact_model: &str,
) -> Result<SelectedTcdsExcerpt, TcdsFamilyBindingError> {
    let mut candidates = document
        .pages
        .iter()
        .flat_map(|page| {
            line_spans(&page.text).into_iter().filter_map(|line| {
                parse_model_heading(line.text).and_then(|heading| {
                    (heading.model == exact_model).then(|| DesignationHeadingCandidate {
                        page_number: page.page_number,
                        line_start: line.start,
                        excerpt: heading.excerpt,
                    })
                })
            })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(TcdsFamilyBindingError::ModelHeadingMissing);
    }
    candidates.sort_by(|left, right| {
        left.page_number
            .cmp(&right.page_number)
            .then_with(|| left.line_start.cmp(&right.line_start))
            .then_with(|| left.excerpt.cmp(&right.excerpt))
    });
    candidates.dedup_by(|left, right| {
        left.page_number == right.page_number && left.excerpt == right.excerpt
    });
    if candidates.len() != 1 {
        return Err(TcdsFamilyBindingError::ModelHeadingAmbiguous);
    }
    let selected = candidates.remove(0);
    Ok(selected_excerpt(selected.page_number, &selected.excerpt))
}

fn find_unique_heading(
    document: &TcdsDocument,
    exact_model: &str,
) -> Result<HeadingBinding, TcdsFamilyBindingError> {
    let mut candidates = document
        .pages
        .iter()
        .flat_map(|page| {
            line_spans(&page.text).into_iter().filter_map(|line| {
                parse_model_family_heading(line.text).and_then(|(model, family, excerpt)| {
                    (model == exact_model).then(|| HeadingCandidate {
                        model,
                        family,
                        page_number: page.page_number,
                        line_start: line.start,
                        excerpt,
                    })
                })
            })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(TcdsFamilyBindingError::ModelHeadingMissing);
    }
    candidates.sort_by(|left, right| {
        left.page_number
            .cmp(&right.page_number)
            .then_with(|| left.line_start.cmp(&right.line_start))
            .then_with(|| left.excerpt.cmp(&right.excerpt))
    });
    candidates.dedup_by(|left, right| {
        left.model == right.model && left.family == right.family && left.excerpt == right.excerpt
    });
    let families = candidates
        .iter()
        .map(|candidate| candidate.family.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if families.len() != 1 {
        return Err(TcdsFamilyBindingError::ModelHeadingAmbiguous);
    }
    let selected = &candidates[0];
    Ok(HeadingBinding {
        family: selected.family.clone(),
    })
}

#[derive(Clone, Debug)]
struct ParsedModelHeading {
    model: String,
    named_family: Option<String>,
    excerpt: String,
}

/// Parse one exact, approval-dated TCDS model heading. Family interpretation
/// is deliberately optional: the heading still proves the FAA designation
/// when its second field is merely capacity/configuration text.
fn parse_model_heading(line: &str) -> Option<ParsedModelHeading> {
    for model_offset in ascii_word_positions(line, "Model") {
        if !heading_prefix_is_allowed(&line[..model_offset]) {
            continue;
        }
        let after_model_offset = model_offset + "Model".len();
        let after_model = &line[after_model_offset..];
        if !after_model.chars().next().is_some_and(char::is_whitespace) {
            continue;
        }
        let after_model = after_model.trim_start();
        let Some(first_comma) = after_model.find(',') else {
            continue;
        };
        let (model, after_first_comma) = after_model.split_at(first_comma);
        let model = compact_whitespace(model);
        if !valid_heading_component(&model, false) {
            continue;
        }
        let after_first_comma = &after_first_comma[1..];
        let minimum_end = line.len() - after_first_comma.len();
        let Some(heading_end) = heading_approval_end(line, minimum_end, model_offset) else {
            continue;
        };
        let named_family = after_first_comma
            .find(',')
            .map(|second_comma| compact_whitespace(&after_first_comma[..second_comma]))
            .filter(|family| valid_named_family_component(family));
        return Some(ParsedModelHeading {
            model,
            named_family,
            excerpt: line[model_offset..heading_end].trim().to_string(),
        });
    }
    None
}

fn parse_model_family_heading(line: &str) -> Option<(String, String, String)> {
    let heading = parse_model_heading(line)?;
    Some((heading.model, heading.named_family?, heading.excerpt))
}

fn heading_approval_end(line: &str, minimum_end: usize, heading_start: usize) -> Option<usize> {
    let approved = fixed_ascii_keyword_positions(&line[minimum_end..], "Approved")
        .next()
        .map(|matched| FixedAsciiKeywordMatch {
            start: minimum_end + matched.start,
            end: minimum_end + matched.end,
        })?;
    let approved_offset = approved.start;
    if approved_offset.saturating_sub(heading_start) > 192 {
        return None;
    }
    (1900..=2200)
        .filter_map(|year| {
            let year = year.to_string();
            let offset = exact_ascii_token_positions(&line[approved.end..], &year)
                .next()
                .map(|offset| approved.end + offset + year.len());
            offset
        })
        .filter(|end| end.saturating_sub(heading_start) <= 256)
        .min()
}

fn heading_prefix_is_allowed(prefix: &str) -> bool {
    if prefix.trim().chars().all(|character| {
        character.is_ascii_whitespace()
            || character.is_ascii_digit()
            || "IVXLCDMivxlcdm.:-()".contains(character)
    }) {
        return true;
    }
    let Some(section_marker) = prefix.trim_end().split_whitespace().next_back() else {
        return true;
    };
    section_marker.strip_suffix('.').is_some_and(|roman| {
        !roman.is_empty()
            && roman
                .chars()
                .all(|character| "IVXLCDMivxlcdm".contains(character))
    })
}

fn valid_heading_component(value: &str, require_letter: bool) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.is_ascii()
        && !value.chars().any(char::is_control)
        && (!require_letter || value.bytes().any(|byte| byte.is_ascii_alphabetic()))
}

/// TCDS model headings are not a structured family-name API. Accept the
/// second comma-delimited field only when it is distinctly name-shaped,
/// rather than a capacity, certification category, or other configuration
/// descriptor. Unknown heading forms fail closed instead of creating a global
/// catalog family from positional PDF text.
fn valid_named_family_component(value: &str) -> bool {
    if !valid_heading_component(value, true)
        || !value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
        || value.chars().any(|character| {
            !(character.is_ascii_alphanumeric()
                || character.is_ascii_whitespace()
                || matches!(character, '-' | '\'' | '&'))
        })
    {
        return false;
    }

    !value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .any(|token| {
            matches!(
                token.as_str(),
                "aircraft"
                    | "airplane"
                    | "amphibian"
                    | "approved"
                    | "category"
                    | "commuter"
                    | "helicopter"
                    | "landplane"
                    | "normal"
                    | "occupant"
                    | "occupants"
                    | "passenger"
                    | "passengers"
                    | "pclm"
                    | "restricted"
                    | "rotorcraft"
                    | "seat"
                    | "seats"
                    | "seaplane"
                    | "transport"
                    | "utility"
            )
        })
}

fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Debug)]
struct SerialEligibilityCandidate {
    model: String,
    first_serial_key: String,
    last_serial_key: Option<String>,
    page_number: u32,
    marker_start: usize,
    entry_end: usize,
    excerpt: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelSectionContext {
    model: String,
    start: usize,
}

/// Resolve the active Roman-numeral model section at one byte offset.
///
/// PDF extraction may flatten a whole physical page, so line boundaries are
/// not reliable here. A section candidate must instead have the exact
/// `XIV. Model <token>` grammar and either the primary-heading comma or the
/// fixed continuation suffix. The nearest preceding section owns the marker;
/// no context is borrowed from another physical page.
fn active_model_section_context(text: &str, offset: usize) -> Option<ModelSectionContext> {
    let mut candidates = ascii_word_positions(text.get(..offset)?, "Model")
        .filter_map(|model_offset| parse_model_section_context(text, model_offset))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| candidate.start);
    let selected = candidates.pop()?;
    if candidates
        .last()
        .is_some_and(|candidate| candidate.start == selected.start)
    {
        return None;
    }
    Some(selected)
}

fn parse_model_section_context(text: &str, model_offset: usize) -> Option<ModelSectionContext> {
    let prefix = text.get(..model_offset)?.trim_end();
    let section_token = prefix.split_whitespace().next_back()?;
    let roman = section_token.strip_suffix('.')?;
    if roman.is_empty()
        || roman.len() > 16
        || !roman
            .bytes()
            .all(|byte| matches!(byte, b'I' | b'V' | b'X' | b'L' | b'C' | b'D' | b'M'))
    {
        return None;
    }
    let section_start = prefix.len().checked_sub(section_token.len())?;

    let after_model = text.get(model_offset + "Model".len()..)?;
    if !after_model.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let after_model = after_model.trim_start();
    let model_end = after_model
        .char_indices()
        .take_while(|(_, character)| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '/')
        })
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    let model = &after_model[..model_end];
    if !valid_heading_component(model, false) {
        return None;
    }
    let suffix = after_model[model_end..].trim_start();
    if !suffix.starts_with(',')
        && !["(cont’d)", "(cont'd)", "(contd)"]
            .iter()
            .any(|continuation| suffix.starts_with(continuation))
    {
        return None;
    }
    Some(ModelSectionContext {
        model: model.to_string(),
        start: section_start,
    })
}

fn find_unique_serial_eligibility(
    document: &TcdsDocument,
    exact_model: &str,
    faa_serial_key: &str,
) -> Result<TcdsSerialEligibility, TcdsFamilyBindingError> {
    let mut candidates = Vec::new();
    for page in &document.pages {
        let lines = line_spans(&page.text);
        for (marker_index, marker) in lines.iter().enumerate() {
            for marker_phrase in ["Serial Numbers Eligible", "Serial Nos. Eligible"] {
                for marker_offset in ascii_phrase_positions(marker.text, marker_phrase) {
                    let marker_start = marker.start + marker_offset;
                    let suffix_start = marker.start + marker_offset + marker_phrase.len();
                    let section_bound_to_exact_model =
                        active_model_section_context(&page.text, marker_start)
                            .is_some_and(|context| context.model == exact_model);
                    let mut exact_model_continuation = None;
                    let mut table_open = collect_serial_eligibility_rows(
                        &page.text[suffix_start..marker.start + marker.text.len()],
                        suffix_start,
                        marker_start,
                        page.page_number,
                        &page.text,
                        exact_model,
                        section_bound_to_exact_model,
                        true,
                        &mut candidates,
                        &mut exact_model_continuation,
                    );
                    if !table_open {
                        continue;
                    }

                    for entry in lines
                        .iter()
                        .skip(marker_index + 1)
                        .take(MAX_SERIAL_ELIGIBILITY_WINDOW_LINES.saturating_sub(1))
                    {
                        table_open = collect_serial_eligibility_rows(
                            entry.text,
                            entry.start,
                            marker_start,
                            page.page_number,
                            &page.text,
                            exact_model,
                            section_bound_to_exact_model,
                            false,
                            &mut candidates,
                            &mut exact_model_continuation,
                        );
                        if !table_open {
                            break;
                        }
                    }
                }
            }
        }
    }
    if candidates.is_empty() {
        return Err(TcdsFamilyBindingError::SerialEligibilityMissing);
    }
    candidates.sort_by(|left, right| {
        left.first_serial_key
            .cmp(&right.first_serial_key)
            .then_with(|| left.last_serial_key.cmp(&right.last_serial_key))
            .then_with(|| left.page_number.cmp(&right.page_number))
            .then_with(|| left.marker_start.cmp(&right.marker_start))
            .then_with(|| left.entry_end.cmp(&right.entry_end))
    });
    candidates.dedup_by(|left, right| {
        left.model == right.model
            && left.first_serial_key == right.first_serial_key
            && left.last_serial_key == right.last_serial_key
    });
    let mut eligible = candidates
        .into_iter()
        .filter(|candidate| {
            serial_is_eligible(
                faa_serial_key,
                &candidate.first_serial_key,
                candidate.last_serial_key.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return Err(TcdsFamilyBindingError::SerialNotEligible);
    }
    if eligible.len() != 1 {
        return Err(TcdsFamilyBindingError::SerialEligibilityAmbiguous);
    }
    let selected = eligible.remove(0);
    Ok(TcdsSerialEligibility {
        page_number: selected.page_number,
        normalized_excerpt_sha256: normalized_excerpt_sha256(&selected.excerpt),
        excerpt: selected.excerpt,
        model: selected.model,
        first_serial_key: selected.first_serial_key,
        last_serial_key: selected.last_serial_key,
    })
}

const MAX_SERIAL_ELIGIBILITY_WINDOW_LINES: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedSerialEligibilityRange {
    first_serial_key: String,
    last_serial_key: Option<String>,
}

#[derive(Clone, Debug)]
struct ParsedSerialEligibilityRow {
    models: Vec<String>,
    ranges: Vec<ParsedSerialEligibilityRange>,
    evidence_end: usize,
    row_end: usize,
}

#[derive(Clone, Debug)]
struct PendingExactModelSerialRow {
    ranges: Vec<ParsedSerialEligibilityRange>,
}

/// Consume only a contiguous sequence of table-shaped rows. Once unrelated
/// prose or another section begins, later lines cannot be borrowed as serial
/// evidence merely because they happen to contain `<model>: <serial> and on`.
///
/// The marker and several rows may be flattened onto one extracted PDF line.
/// `after_marker` permits only the marker's optional colon before the first
/// row; it does not weaken row anchoring. An unlabeled bounded row is accepted
/// only when the marker sits inside the exact model's explicit section. The
/// collector then consumes only immediately contiguous table-shaped rows and
/// stops permanently at the first equipment/prose row.
#[allow(clippy::too_many_arguments)]
fn collect_serial_eligibility_rows(
    text: &str,
    absolute_start: usize,
    marker_start: usize,
    page_number: u32,
    page_text: &str,
    exact_model: &str,
    section_bound_to_exact_model: bool,
    after_marker: bool,
    candidates: &mut Vec<SerialEligibilityCandidate>,
    exact_model_continuation: &mut Option<PendingExactModelSerialRow>,
) -> bool {
    let mut cursor = text.len() - text.trim_start().len();
    let mut parsed_row = false;
    if after_marker && text.as_bytes().get(cursor) == Some(&b':') {
        cursor += 1;
    }

    loop {
        cursor += text[cursor..].len() - text[cursor..].trim_start().len();
        if cursor == text.len() {
            if !after_marker && !parsed_row && exact_model_continuation.is_some() {
                *exact_model_continuation = None;
                return false;
            }
            return true;
        }
        let labeled_row = parse_serial_eligibility_row_prefix(&text[cursor..]);
        let continuing_exact_model_row =
            labeled_row.is_none() && exact_model_continuation.is_some();
        let row = labeled_row.clone().or_else(|| {
            if continuing_exact_model_row {
                parse_unlabeled_exact_model_continuation_row_prefix(&text[cursor..], exact_model)
            } else if section_bound_to_exact_model {
                parse_unlabeled_model_eligibility_row_prefix(&text[cursor..], exact_model)
            } else {
                None
            }
        });
        let Some(row) = row else {
            *exact_model_continuation = None;
            return false;
        };
        let matches_exact_model = row
            .models
            .iter()
            .any(|model| extracted_model_token_matches(model, exact_model));
        let comma_continues =
            row.evidence_end > 0 && text[cursor..].as_bytes()[row.evidence_end - 1] == b',';

        if continuing_exact_model_row {
            let Some(mut pending) = exact_model_continuation.take() else {
                return false;
            };
            if !matches_exact_model
                || pending
                    .ranges
                    .len()
                    .checked_add(row.ranges.len())
                    .is_none_or(|count| count > MAX_SERIAL_RANGES_PER_ROW)
            {
                return false;
            }
            pending.ranges.extend(row.ranges.iter().cloned());
            if comma_continues {
                *exact_model_continuation = Some(pending);
            } else {
                if !push_serial_eligibility_candidates(
                    &pending.ranges,
                    absolute_start + cursor + row.row_end,
                    marker_start,
                    page_number,
                    page_text,
                    exact_model,
                    candidates,
                ) {
                    return false;
                }
            }
        } else {
            // A new labeled row closes any unfinished exact-model continuation.
            // Its staged ranges are deliberately discarded: a comma promised
            // another immediately contiguous range, so admitting only the
            // prefix would turn malformed or ambiguous evidence into a scope.
            *exact_model_continuation = None;
            if matches_exact_model && comma_continues {
                *exact_model_continuation = Some(PendingExactModelSerialRow {
                    ranges: row.ranges.clone(),
                });
            } else if matches_exact_model {
                if !push_serial_eligibility_candidates(
                    &row.ranges,
                    absolute_start + cursor + row.evidence_end,
                    marker_start,
                    page_number,
                    page_text,
                    exact_model,
                    candidates,
                ) {
                    return false;
                }
            }
        }
        parsed_row = true;
        cursor += row.row_end;
    }
}

#[allow(clippy::too_many_arguments)]
fn push_serial_eligibility_candidates(
    ranges: &[ParsedSerialEligibilityRange],
    entry_end: usize,
    marker_start: usize,
    page_number: u32,
    page_text: &str,
    exact_model: &str,
    candidates: &mut Vec<SerialEligibilityCandidate>,
) -> bool {
    let Some(excerpt) = page_text.get(marker_start..entry_end) else {
        return false;
    };
    let excerpt = excerpt.trim().to_string();
    for range in ranges {
        candidates.push(SerialEligibilityCandidate {
            model: exact_model.to_string(),
            first_serial_key: range.first_serial_key.clone(),
            last_serial_key: range.last_serial_key.clone(),
            page_number,
            marker_start,
            entry_end,
            excerpt: excerpt.clone(),
        });
    }
    true
}

fn parse_serial_eligibility_row_prefix(value: &str) -> Option<ParsedSerialEligibilityRow> {
    let colon = value.find(':')?;
    let model_field = compact_whitespace(&value[..colon]);
    let model_field = model_field.strip_prefix("Model ").unwrap_or(&model_field);
    if model_field.is_empty() || model_field.len() > 64 {
        return None;
    }
    let models = model_field
        .split('/')
        .map(compact_whitespace)
        .collect::<Vec<_>>();
    if models.is_empty()
        || models.len() > 8
        || models.iter().any(|model| {
            model.is_empty()
                || model.matches(' ').count() > 2
                || !model
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphanumeric())
                || !model
                    .chars()
                    .next_back()
                    .is_some_and(|character| character.is_ascii_alphanumeric())
                || !model.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '-' || character == ' '
                })
        })
    {
        return None;
    }

    let after_colon = &value[colon + 1..];
    let after_serial_start = colon + 1 + (after_colon.len() - after_colon.trim_start().len());
    let (ranges, range_end) = parse_serial_ranges_prefix(&value[after_serial_start..])?;
    let evidence_end =
        consume_serial_row_terminator(value, after_serial_start.saturating_add(range_end));
    let row_end = consume_model_year_annotation(value, evidence_end);
    Some(ParsedSerialEligibilityRow {
        models,
        ranges,
        evidence_end,
        row_end,
    })
}

fn extracted_model_token_matches(extracted: &str, exact_model: &str) -> bool {
    if extracted == exact_model {
        return true;
    }
    let inserted_spaces = extracted.matches(' ').count();
    inserted_spaces > 0
        && inserted_spaces <= 2
        && extracted
            .bytes()
            .filter(|byte| *byte != b' ')
            .eq(exact_model.bytes())
}

fn parse_unlabeled_serial_eligibility_row_prefix(
    value: &str,
    exact_model: &str,
) -> Option<ParsedSerialEligibilityRow> {
    let row = parse_unlabeled_serial_ranges_row_prefix(value, exact_model)?;
    (row.ranges.len() == 1).then_some(row)
}

/// Parse an unlabeled row only after the caller has deterministically bound
/// the explicit serial-eligibility marker to this exact model section.
///
/// Historical FAA tables can place a singleton and a bounded range on the
/// same row, and an `except` clause can split one printed range into multiple
/// persistable contiguous ranges. A bounded row must carry its exact
/// `(YYYY Model)` annotation, except that a comma-terminated row may continue
/// on the next physical line. The pre-existing single open `and On` form also
/// remains valid. None of those broader model-table shapes are accepted by
/// the generic manufacturer-lineage parser, which retains the stricter
/// single-range wrapper above.
fn parse_unlabeled_model_eligibility_row_prefix(
    value: &str,
    exact_model: &str,
) -> Option<ParsedSerialEligibilityRow> {
    let row = parse_unlabeled_serial_ranges_row_prefix(value, exact_model)?;
    let has_model_year_annotation = row.row_end > row.evidence_end;
    let comma_wrapped_continuation =
        row.evidence_end > 0 && value.as_bytes().get(row.evidence_end - 1) == Some(&b',');
    let single_open_range = row.ranges.len() == 1 && row.ranges[0].last_serial_key.is_none();
    (has_model_year_annotation || comma_wrapped_continuation || single_open_range).then_some(row)
}

fn parse_unlabeled_exact_model_continuation_row_prefix(
    value: &str,
    exact_model: &str,
) -> Option<ParsedSerialEligibilityRow> {
    let row = parse_unlabeled_serial_ranges_row_prefix(value, exact_model)?;
    let has_model_year_annotation = row.row_end > row.evidence_end;
    let comma_wrapped_continuation =
        row.evidence_end > 0 && value.as_bytes().get(row.evidence_end - 1) == Some(&b',');
    (has_model_year_annotation || comma_wrapped_continuation).then_some(row)
}

fn parse_unlabeled_serial_ranges_row_prefix(
    value: &str,
    exact_model: &str,
) -> Option<ParsedSerialEligibilityRow> {
    let (ranges, range_end) = parse_serial_ranges_prefix(value)?;
    let evidence_end = consume_serial_row_terminator(value, range_end);
    let row_end = consume_model_year_annotation(value, evidence_end);
    Some(ParsedSerialEligibilityRow {
        models: vec![exact_model.to_string()],
        ranges,
        evidence_end,
        row_end,
    })
}

const MAX_SERIAL_RANGES_PER_ROW: usize = 4;

fn parse_serial_ranges_prefix(value: &str) -> Option<(Vec<ParsedSerialEligibilityRange>, usize)> {
    let mut cursor = value.len() - value.trim_start().len();
    let mut ranges = Vec::new();
    loop {
        if ranges.len() == MAX_SERIAL_RANGES_PER_ROW {
            return None;
        }
        let (first_serial, trailing) = take_serial_token(&value[cursor..])?;
        let first_serial_key = normalize_serial_key(first_serial)?;
        comparable_serial(&first_serial_key)?;
        cursor = value.len().saturating_sub(trailing.len());
        cursor += value[cursor..].len() - value[cursor..].trim_start().len();

        if let Some(consumed) = ascii_phrase_prefix_len(&value[cursor..], "and on") {
            ranges.push(ParsedSerialEligibilityRange {
                first_serial_key,
                last_serial_key: None,
            });
            return Some((ranges, cursor + consumed));
        }

        let mut bounded_range = None;
        for separator in ["thru", "through", "to"] {
            let Some(after_separator) = strip_ascii_word_prefix(&value[cursor..], separator) else {
                continue;
            };
            let after_separator = after_separator.trim_start();
            let (last_serial, trailing) = take_serial_token(after_separator)?;
            let last_serial_key = normalize_serial_key(last_serial)?;
            comparable_serial(&last_serial_key)?;
            bounded_range = Some((last_serial_key, value.len().saturating_sub(trailing.len())));
            break;
        }
        if let Some((last_serial_key, end)) = bounded_range {
            let bounded = ParsedSerialEligibilityRange {
                first_serial_key,
                last_serial_key: Some(last_serial_key),
            };
            let (bounded, end) = split_bounded_range_at_exact_exclusions(value, end, bounded)?;
            ranges.extend(bounded);
            return (!ranges.is_empty()).then_some((ranges, end));
        }

        if value.as_bytes().get(cursor) != Some(&b',') {
            return None;
        }
        ranges.push(ParsedSerialEligibilityRange {
            first_serial_key: first_serial_key.clone(),
            last_serial_key: Some(first_serial_key),
        });
        cursor += 1;
        cursor += value[cursor..].len() - value[cursor..].trim_start().len();
    }
}

/// Parse an optional FAA serial-row `except` clause and return only contiguous
/// eligible ranges.
///
/// A persisted make-lineage scope cannot represent holes. Keeping the original
/// outer bounds would therefore admit an expressly excluded serial. Each
/// exclusion must be one exact serial comparable to the bounded range; prose,
/// ranges, duplicates, foreign prefixes/widths, and out-of-range values fail
/// closed by rejecting the complete row.
fn split_bounded_range_at_exact_exclusions(
    value: &str,
    range_end: usize,
    range: ParsedSerialEligibilityRange,
) -> Option<(Vec<ParsedSerialEligibilityRange>, usize)> {
    let suffix = value.get(range_end..)?;
    let mut clause_offset = suffix.len() - suffix.trim_start().len();
    if suffix.as_bytes().get(clause_offset) == Some(&b',') {
        clause_offset += 1;
        clause_offset += suffix[clause_offset..].len() - suffix[clause_offset..].trim_start().len();
    }
    let Some(after_except) = strip_ascii_word_prefix(&suffix[clause_offset..], "except") else {
        return Some((vec![range], range_end));
    };
    let mut cursor = range_end + clause_offset + suffix[clause_offset..].len() - after_except.len();
    cursor += value[cursor..].len() - value[cursor..].trim_start().len();

    let first = comparable_serial(&range.first_serial_key)?;
    let last_key = range.last_serial_key.as_deref()?;
    let last = comparable_serial(last_key)?;
    if first.prefix != last.prefix
        || first.digit_width != last.digit_width
        || first.number > last.number
    {
        return None;
    }

    let mut excluded_numbers = Vec::new();
    loop {
        let (token, trailing) = take_serial_token(&value[cursor..])?;
        let exclusion_key = normalize_serial_key(token)?;
        let exclusion = comparable_serial(&exclusion_key)?;
        if exclusion.prefix != first.prefix
            || exclusion.digit_width != first.digit_width
            || exclusion.number < first.number
            || exclusion.number > last.number
            || excluded_numbers.contains(&exclusion.number)
        {
            return None;
        }
        excluded_numbers.push(exclusion.number);
        cursor = value.len().saturating_sub(trailing.len());

        let separator_start = cursor + value[cursor..].len() - value[cursor..].trim_start().len();
        if value.as_bytes().get(separator_start) == Some(&b',') {
            cursor = separator_start + 1;
        } else if value.as_bytes().get(separator_start) == Some(&b'&') {
            cursor = separator_start + 1;
        } else if let Some(after_and) = strip_ascii_word_prefix(&value[separator_start..], "and") {
            cursor = value.len().saturating_sub(after_and.len());
        } else {
            break;
        }
        cursor += value[cursor..].len() - value[cursor..].trim_start().len();
        if cursor == value.len() {
            return None;
        }
    }

    // The exclusion grammar must end at the physical row boundary, ordinary
    // punctuation, or a model-year annotation. In particular, text such as
    // `except 123 or 124` must not silently exclude only its first token.
    let tail = &value[cursor..];
    let tail_whitespace = tail.len() - tail.trim_start().len();
    let has_physical_row_boundary = tail[..tail_whitespace]
        .bytes()
        .any(|byte| matches!(byte, b'\r' | b'\n'));
    let tail = &tail[tail_whitespace..];
    if !has_physical_row_boundary
        && !tail.is_empty()
        && !tail.starts_with('.')
        && !tail.starts_with(';')
        && !tail.starts_with('(')
    {
        return None;
    }

    excluded_numbers.sort_unstable();
    let mut narrowed = Vec::with_capacity(excluded_numbers.len() + 1);
    let mut next_number = first.number;
    for excluded in excluded_numbers {
        if next_number < excluded {
            narrowed.push(serial_range_from_numbers(
                &first,
                next_number,
                excluded - 1,
            )?);
        }
        next_number = excluded + 1;
    }
    if next_number <= last.number {
        narrowed.push(serial_range_from_numbers(&first, next_number, last.number)?);
    }
    (!narrowed.is_empty()).then_some((narrowed, cursor))
}

fn serial_range_from_numbers(
    template: &ComparableSerial,
    first: u128,
    last: u128,
) -> Option<ParsedSerialEligibilityRange> {
    Some(ParsedSerialEligibilityRange {
        first_serial_key: serial_key_from_number(template, first)?,
        last_serial_key: Some(serial_key_from_number(template, last)?),
    })
}

fn serial_key_from_number(template: &ComparableSerial, number: u128) -> Option<String> {
    let digits = format!("{number:0width$}", width = template.digit_width);
    (digits.len() == template.digit_width).then(|| format!("{}{}", template.prefix, digits))
}

fn consume_serial_row_terminator(value: &str, mut end: usize) -> usize {
    if value
        .as_bytes()
        .get(end)
        .is_some_and(|byte| matches!(byte, b'.' | b';' | b','))
    {
        end += 1;
    }
    end
}

fn consume_model_year_annotation(value: &str, end: usize) -> usize {
    let whitespace = value[end..].len() - value[end..].trim_start().len();
    let annotation_start = end + whitespace;
    let Some(annotation) = value.get(annotation_start..) else {
        return end;
    };
    let prefix = &annotation[..annotation.len().min(32)];
    let Some(close) = prefix.find(')') else {
        return end;
    };
    let Some(inner) = annotation
        .get(1..close)
        .filter(|_| annotation.starts_with('('))
    else {
        return end;
    };
    let compact = compact_whitespace(inner);
    let Some(year) = compact.strip_suffix(" Model") else {
        return end;
    };
    if year.len() != 4
        || !year.bytes().all(|byte| byte.is_ascii_digit())
        || !matches!(year.parse::<u16>(), Ok(1900..=2200))
    {
        return end;
    }
    annotation_start + close + 1
}

fn ascii_phrase_prefix_len(value: &str, phrase: &str) -> Option<usize> {
    let prefix = value.get(..phrase.len())?;
    if !prefix.eq_ignore_ascii_case(phrase) {
        return None;
    }
    let after = &value[phrase.len()..];
    after
        .chars()
        .next()
        .is_none_or(|character| !character.is_ascii_alphanumeric())
        .then_some(phrase.len())
}

fn take_serial_token(value: &str) -> Option<(&str, &str)> {
    let end = value
        .char_indices()
        .take_while(|(_, character)| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '/')
        })
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    let token = &value[..end];
    (!token.is_empty()).then_some((token, &value[end..]))
}

fn strip_ascii_word_prefix<'a>(value: &'a str, word: &str) -> Option<&'a str> {
    let prefix = value.get(..word.len())?;
    if !prefix.eq_ignore_ascii_case(word) {
        return None;
    }
    let after = &value[word.len()..];
    after
        .chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then_some(after)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManufacturerSerialCandidate {
    manufacturer_name: String,
    model: String,
    first_serial_key: String,
    last_serial_key: Option<String>,
    page_number: u32,
    excerpt: String,
}

fn find_unique_manufacturer_serial_eligibility(
    document: &TcdsDocument,
    exact_model: &str,
    faa_serial_key: &str,
) -> Result<Option<TcdsManufacturerSerialEligibility>, TcdsFamilyBindingError> {
    let mut candidates = Vec::new();
    for page in &document.pages {
        for marker in manufacturer_serial_marker_positions(&page.text) {
            let marker_start = marker.start;
            let after_marker = &page.text[marker.end..];
            let leading = after_marker.len() - after_marker.trim_start().len();
            let after_marker = &after_marker[leading..];
            let Some(colon) = after_marker
                .get(..after_marker.len().min(192))
                .and_then(|text| text.find(':'))
            else {
                continue;
            };
            let manufacturer_name = compact_whitespace(&after_marker[..colon]);
            if !valid_company_name(&manufacturer_name) {
                continue;
            }
            let rows_start = marker.end + leading + colon + 1;
            let rows = &page.text[rows_start..];
            let exact_context = page_model_context_matches(&page.text, marker_start, exact_model);
            let mut cursor = rows.len() - rows.trim_start().len();
            loop {
                cursor += rows[cursor..].len() - rows[cursor..].trim_start().len();
                if cursor == rows.len() {
                    break;
                }
                let Some(row) =
                    parse_serial_eligibility_row_prefix(&rows[cursor..]).or_else(|| {
                        exact_context.then(|| {
                            parse_unlabeled_serial_eligibility_row_prefix(
                                &rows[cursor..],
                                exact_model,
                            )
                        })?
                    })
                else {
                    break;
                };
                if row
                    .models
                    .iter()
                    .any(|model| extracted_model_token_matches(model, exact_model))
                {
                    let evidence_end = rows_start + cursor + row.evidence_end;
                    let Some(excerpt) = page.text.get(marker_start..evidence_end) else {
                        break;
                    };
                    for range in &row.ranges {
                        candidates.push(ManufacturerSerialCandidate {
                            manufacturer_name: manufacturer_name.clone(),
                            model: exact_model.to_string(),
                            first_serial_key: range.first_serial_key.clone(),
                            last_serial_key: range.last_serial_key.clone(),
                            page_number: page.page_number,
                            excerpt: excerpt.trim().to_string(),
                        });
                    }
                }
                cursor += row.row_end;
            }
        }
    }

    candidates.sort_by(|left, right| {
        left.manufacturer_name
            .cmp(&right.manufacturer_name)
            .then_with(|| left.first_serial_key.cmp(&right.first_serial_key))
            .then_with(|| left.last_serial_key.cmp(&right.last_serial_key))
            .then_with(|| left.page_number.cmp(&right.page_number))
            .then_with(|| left.excerpt.cmp(&right.excerpt))
    });
    candidates.dedup();
    let mut matching = candidates
        .into_iter()
        .filter(|candidate| {
            serial_is_eligible(
                faa_serial_key,
                &candidate.first_serial_key,
                candidate.last_serial_key.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Ok(None);
    }
    if matching.len() != 1 {
        return Err(TcdsFamilyBindingError::MakeLineageAmbiguous);
    }
    let selected = matching.remove(0);
    Ok(Some(TcdsManufacturerSerialEligibility {
        page_number: selected.page_number,
        normalized_excerpt_sha256: normalized_excerpt_sha256(&selected.excerpt),
        excerpt: selected.excerpt,
        manufacturer_name: selected.manufacturer_name,
        model: selected.model,
        first_serial_key: selected.first_serial_key,
        last_serial_key: selected.last_serial_key,
    }))
}

fn page_model_context_matches(text: &str, offset: usize, exact_model: &str) -> bool {
    if active_model_section_context(text, offset)
        .is_some_and(|context| context.model == exact_model)
    {
        return true;
    }
    let Some(prefix) = text.get(..offset) else {
        return false;
    };
    let mut contexts = ascii_phrase_positions(prefix, "Data Pertinent to Model")
        .filter_map(|start| {
            let after = prefix[start + "Data Pertinent to Model".len()..].trim_start();
            let end = after
                .char_indices()
                .take_while(|(_, character)| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '/')
                })
                .map(|(index, character)| index + character.len_utf8())
                .last()?;
            let model = &after[..end];
            let suffix = after[end..].trim_start();
            (suffix.starts_with("(cont’d)")
                || suffix.starts_with("(cont'd)")
                || suffix.starts_with("(contd)")
                || suffix.starts_with('\n'))
            .then_some((start, model))
        })
        .collect::<Vec<_>>();
    contexts.sort_by_key(|(start, _)| *start);
    contexts
        .pop()
        .is_some_and(|(_, model)| model == exact_model)
}

fn valid_company_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.is_ascii()
        && value.bytes().any(|byte| byte.is_ascii_alphabetic())
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character.is_ascii_whitespace()
                || matches!(character, '.' | ',' | '-' | '\'' | '&')
        })
}

fn find_unique_holder_transfer(
    document: &TcdsDocument,
) -> Result<Option<TcdsHolderTransferEvidence>, TcdsFamilyBindingError> {
    const MARKER: &str = "Type Certificate Holder Record";
    const TRANSFER: &str = "transferred to";
    let mut candidates = Vec::new();
    for page in &document.pages {
        for marker_start in ascii_phrase_positions(&page.text, MARKER) {
            let after_marker_start = marker_start + MARKER.len();
            let after_marker = &page.text[after_marker_start..];
            let leading = after_marker.len() - after_marker.trim_start().len();
            let after_marker = &after_marker[leading..];
            let Some(transfer_offset) = ascii_phrase_positions(after_marker, TRANSFER).next()
            else {
                continue;
            };
            let former_holder_name = compact_whitespace(&after_marker[..transfer_offset]);
            if !valid_company_name(&former_holder_name) {
                continue;
            }
            let after_transfer = &after_marker[transfer_offset + TRANSFER.len()..];
            let transfer_leading = after_transfer.len() - after_transfer.trim_start().len();
            let after_transfer = &after_transfer[transfer_leading..];
            for on_offset in ascii_word_positions(after_transfer, "on") {
                let current_holder_name = compact_whitespace(&after_transfer[..on_offset]);
                if !valid_company_name(&current_holder_name) {
                    continue;
                }
                let after_on = &after_transfer[on_offset + "on".len()..];
                let date_leading = after_on.len() - after_on.trim_start().len();
                let Some(date_len) = calendar_date_prefix_len(&after_on[date_leading..]) else {
                    continue;
                };
                let effective_date_text =
                    compact_whitespace(&after_on[date_leading..date_leading + date_len]);
                let excerpt_end = after_marker_start
                    + leading
                    + transfer_offset
                    + TRANSFER.len()
                    + transfer_leading
                    + on_offset
                    + "on".len()
                    + date_leading
                    + date_len;
                let Some(excerpt) = page.text.get(marker_start..excerpt_end) else {
                    continue;
                };
                candidates.push(TcdsHolderTransferEvidence {
                    page_number: page.page_number,
                    normalized_excerpt_sha256: normalized_excerpt_sha256(excerpt),
                    excerpt: excerpt.to_string(),
                    former_holder_name,
                    current_holder_name,
                    effective_date_text,
                });
                break;
            }
        }
    }
    candidates.sort_by(|left, right| {
        left.page_number
            .cmp(&right.page_number)
            .then_with(|| left.excerpt.cmp(&right.excerpt))
    });
    candidates.dedup();
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => Err(TcdsFamilyBindingError::HolderTransferAmbiguous),
    }
}

fn calendar_date_prefix_len(value: &str) -> Option<usize> {
    let month_end = value.find(char::is_whitespace)?;
    let month = &value[..month_end];
    if !matches!(
        month,
        "January"
            | "February"
            | "March"
            | "April"
            | "May"
            | "June"
            | "July"
            | "August"
            | "September"
            | "October"
            | "November"
            | "December"
    ) {
        return None;
    }
    let after_month = &value[month_end..];
    let whitespace = after_month.len() - after_month.trim_start().len();
    let day_start = month_end + whitespace;
    let comma = value[day_start..]
        .find(',')
        .map(|offset| day_start + offset)?;
    let day = value[day_start..comma].parse::<u8>().ok()?;
    if !(1..=31).contains(&day) {
        return None;
    }
    let after_comma = &value[comma + 1..];
    let whitespace = after_comma.len() - after_comma.trim_start().len();
    let year_start = comma + 1 + whitespace;
    let year = value.get(year_start..year_start + 4)?;
    if !year.bytes().all(|byte| byte.is_ascii_digit())
        || !matches!(year.parse::<u16>(), Ok(1900..=2200))
        || value
            .as_bytes()
            .get(year_start + 4)
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return None;
    }
    Some(year_start + 4)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ComparableSerial {
    prefix: String,
    number: u128,
    digit_width: usize,
}

fn comparable_serial(value: &str) -> Option<ComparableSerial> {
    let first_digit = value.bytes().position(|byte| byte.is_ascii_digit())?;
    let (prefix, digits) = value.split_at(first_digit);
    if !prefix.bytes().all(|byte| byte.is_ascii_uppercase())
        || digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || digits.len() > 38
    {
        return None;
    }
    Some(ComparableSerial {
        prefix: prefix.to_string(),
        number: digits.parse().ok()?,
        digit_width: digits.len(),
    })
}

fn serial_is_eligible(serial: &str, first: &str, last: Option<&str>) -> bool {
    let (Some(serial), Some(first)) = (comparable_serial(serial), comparable_serial(first)) else {
        return false;
    };
    if serial.prefix != first.prefix
        || serial.digit_width != first.digit_width
        || serial.number < first.number
    {
        return false;
    }
    match last {
        None => true,
        Some(last) => comparable_serial(last).is_some_and(|last| {
            last.prefix == first.prefix
                && last.digit_width == first.digit_width
                && last.number >= first.number
                && serial.number <= last.number
        }),
    }
}

fn selected_excerpt(page_number: u32, excerpt: &str) -> SelectedTcdsExcerpt {
    SelectedTcdsExcerpt {
        page_number,
        excerpt: excerpt.to_string(),
        normalized_excerpt_sha256: normalized_excerpt_sha256(excerpt),
    }
}

struct LineSpan<'a> {
    start: usize,
    text: &'a str,
}

fn line_spans(text: &str) -> Vec<LineSpan<'_>> {
    let mut result = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        result.push(LineSpan { start, text: line });
        start = end;
    }
    if start < text.len() {
        result.push(LineSpan {
            start,
            text: &text[start..],
        });
    }
    result
}

fn ascii_word_positions<'a>(haystack: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    haystack.match_indices(word).filter_map(move |(index, _)| {
        ascii_token_boundaries(haystack, index, word.len()).then_some(index)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FixedAsciiKeywordMatch {
    start: usize,
    end: usize,
}

const MAX_FIXED_KEYWORD_INSERTED_SPACES: usize = 4;
const MAX_FIXED_PHRASE_EXTRA_SPACES: usize = 16;
const MAX_MANUFACTURER_MARKER_BOUNDARY_SPACES: usize = 4;

/// Match a fixed ASCII keyword while tolerating a small number of literal
/// spaces inserted between its letters by PDF text extraction.
///
/// Matching remains case-sensitive, token-bounded, line-local, and bounded in
/// both inserted bytes and the caller's heading window. The returned span is
/// always an offset into the original text; no repaired text is used as
/// evidence or hashed.
fn fixed_ascii_keyword_positions<'a>(
    haystack: &'a str,
    keyword: &'a str,
) -> impl Iterator<Item = FixedAsciiKeywordMatch> + 'a {
    let bytes = haystack.as_bytes();
    let keyword = keyword.as_bytes();
    (0..bytes.len()).filter_map(move |start| {
        if keyword.is_empty()
            || !keyword.iter().all(u8::is_ascii_alphabetic)
            || bytes.get(start) != keyword.first()
        {
            return None;
        }

        let mut cursor = start;
        let mut inserted_spaces = 0;
        for (keyword_index, expected) in keyword.iter().enumerate() {
            if keyword_index > 0 {
                while bytes.get(cursor) == Some(&b' ')
                    && inserted_spaces < MAX_FIXED_KEYWORD_INSERTED_SPACES
                {
                    cursor += 1;
                    inserted_spaces += 1;
                }
            }
            if bytes.get(cursor) != Some(expected) {
                return None;
            }
            cursor += 1;
        }
        ascii_span_token_boundaries(haystack, start, cursor)
            .then_some(FixedAsciiKeywordMatch { start, end: cursor })
    })
}

/// Match an exact ASCII phrase while tolerating bounded duplicate literal
/// spaces at the phrase's existing word boundaries. Letters and punctuation
/// remain exact; this never deletes or inserts a boundary inside a token.
fn fixed_ascii_phrase_positions<'a>(
    haystack: &'a str,
    phrase: &'a str,
) -> impl Iterator<Item = FixedAsciiKeywordMatch> + 'a {
    let bytes = haystack.as_bytes();
    let phrase = phrase.as_bytes();
    (0..bytes.len()).filter_map(move |start| {
        if phrase.is_empty() || bytes.get(start) != phrase.first() {
            return None;
        }
        let mut cursor = start;
        let mut extra_spaces = 0;
        for expected in phrase {
            if *expected == b' ' {
                if bytes.get(cursor) != Some(&b' ') {
                    return None;
                }
                cursor += 1;
                while bytes.get(cursor) == Some(&b' ')
                    && extra_spaces < MAX_FIXED_PHRASE_EXTRA_SPACES
                {
                    cursor += 1;
                    extra_spaces += 1;
                }
            } else {
                if bytes.get(cursor) != Some(expected) {
                    return None;
                }
                cursor += 1;
            }
        }
        ascii_span_token_boundaries(haystack, start, cursor)
            .then_some(FixedAsciiKeywordMatch { start, end: cursor })
    })
}

/// Match the two literal FAA manufacturer-range marker spellings.
///
/// Revision 75 of TCDS 3A13 extracts the NOTE 6 word `manufactured` as
/// `manufac tured`. Only that fixed word may contain one inserted literal
/// space. The surrounding fixed phrases remain exact and every returned byte
/// offset addresses the original page text, so excerpts and hashes retain the
/// regulator-published extraction rather than a repaired copy.
fn manufacturer_serial_marker_positions(haystack: &str) -> Vec<FixedAsciiKeywordMatch> {
    let mut matches = Vec::new();
    for prefix_phrase in [
        "The following serials numbers are",
        "The following serial numbers are",
    ] {
        for prefix in fixed_ascii_phrase_positions(haystack, prefix_phrase) {
            let Some(before_keyword) = bounded_literal_space_separator_len(&haystack[prefix.end..])
            else {
                continue;
            };
            let keyword_start = prefix.end + before_keyword;
            let Some(keyword_len) =
                tcds_manufactured_keyword_prefix_len(&haystack[keyword_start..])
            else {
                continue;
            };
            let keyword_end = keyword_start + keyword_len;
            let Some(after_keyword) = bounded_literal_space_separator_len(&haystack[keyword_end..])
            else {
                continue;
            };
            let suffix_start = keyword_end + after_keyword;
            let Some(suffix) =
                fixed_ascii_phrase_positions(&haystack[suffix_start..], "under the name")
                    .find(|candidate| candidate.start == 0)
            else {
                continue;
            };
            matches.push(FixedAsciiKeywordMatch {
                start: prefix.start,
                end: suffix_start + suffix.end,
            });
        }
    }
    matches.sort_by_key(|candidate| (candidate.start, candidate.end));
    matches.dedup();
    matches
}

fn tcds_manufactured_keyword_prefix_len(value: &str) -> Option<usize> {
    ["manufactured", "manufac tured"]
        .into_iter()
        .find_map(|candidate| {
            value
                .get(..candidate.len())
                .filter(|prefix| *prefix == candidate)
                .filter(|_| ascii_span_token_boundaries(value, 0, candidate.len()))
                .map(str::len)
        })
}

fn bounded_literal_space_separator_len(value: &str) -> Option<usize> {
    let spaces = value.bytes().take_while(|byte| *byte == b' ').count();
    (1..=MAX_MANUFACTURER_MARKER_BOUNDARY_SPACES)
        .contains(&spaces)
        .then_some(spaces)
}

fn exact_ascii_token_positions<'a>(
    haystack: &'a str,
    token: &'a str,
) -> impl Iterator<Item = usize> + 'a {
    haystack.match_indices(token).filter_map(move |(index, _)| {
        ascii_token_boundaries(haystack, index, token.len()).then_some(index)
    })
}

fn ascii_token_boundaries(haystack: &str, start: usize, len: usize) -> bool {
    ascii_span_token_boundaries(haystack, start, start + len)
}

fn ascii_span_token_boundaries(haystack: &str, start: usize, end: usize) -> bool {
    let bytes = haystack.as_bytes();
    let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
    let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
    before_ok && after_ok
}

fn ascii_phrase_positions<'a>(
    haystack: &'a str,
    phrase: &'a str,
) -> impl Iterator<Item = usize> + 'a {
    haystack.match_indices(phrase).filter_map(|(index, _)| {
        ascii_token_boundaries(haystack, index, phrase.len()).then_some(index)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::faa::drs::{CurrentTcdsMetadata, TcdsPageText};

    const PDF_SHA256: &str = "666d85552cdcd0972de21bf092c904f5ee5a45e8549cafddb84ed3017f4c454a";

    fn page(page_number: u32, text: impl Into<String>) -> TcdsPageText {
        TcdsPageText {
            page_number,
            text: text.into(),
        }
    }

    fn document(pages: Vec<TcdsPageText>) -> TcdsDocument {
        TcdsDocument {
            metadata: CurrentTcdsMetadata {
                document_guid: "cbe9c99d-492f-4d25-9d37-925d57816f27".to_string(),
                document_url: "https://drs.faa.gov/browse/excelExternalWindow/cbe9c99d-492f-4d25-9d37-925d57816f27".to_string(),
                tcds_number: "3A13".to_string(),
                revision_number: Some("75".to_string()),
                revision_date: Some("08/07/2024".to_string()),
                tc_holder: Some("Textron Aviation Inc.".to_string()),
                former_tc_holders: vec!["Cessna Aircraft Company".to_string()],
                models: vec![
                    "182".to_string(),
                    "182S".to_string(),
                    "182T".to_string(),
                    "T182T".to_string(),
                ],
                exact_model: "182T".to_string(),
            },
            source_url: "https://drs.faa.gov/api/drs/data-pull/download/cbe9c99d-492f-4d25-9d37-925d57816f27"
                .to_string(),
            pdf_sha256: PDF_SHA256.to_string(),
            pdf_size_bytes: 243_543,
            page_count: pages.len(),
            pages,
            exact_model_blocks: Vec::new(),
        }
    }

    fn valid_document() -> TcdsDocument {
        document(vec![
            page(
                1,
                "I.   Model 182, Skylane, 4 PCLM (Normal Category), Approved March 2, 1956\n",
            ),
            page(
                34,
                "XIII. Model 182S, Skylane, 4 PCLM (Normal Category), Approved 03 October 1996.\n      Model 182T, Skylane, 4 PCLM (Normal Category), Approved 23 February 2001.\n",
            ),
            page(
                35,
                "Serial Numbers Eligible\n182S: 18280001 thru 18280944\n182T: 18280945 and On\n",
            ),
            page(
                38,
                "NOTE 7. Company name change effective 7/29/15.\nTextron Aviation Inc.: Model 182T: 18282369 and On.\n",
            ),
        ])
    }

    fn real_3a13_page_38_make_lineage_document(exact_model: &str) -> TcdsDocument {
        let mut document = valid_document();
        document.metadata.exact_model = exact_model.to_string();
        document.pages[0].text = concat!(
            "Type Certificate Holder Record Cessna Aircraft Company transferred to    ",
            "Textron Aviation Inc. on July 29, 2015  ",
            "I.   Model 182, Skylane, 4 PCLM (Normal Category), Approved March 2, 1956",
        )
        .to_string();
        document.pages[3].text = concat!(
            "3A13 Page 38 of 42 Rev. 75   ",
            "Data Pertinent to Model 182S and 182T  (cont’d)  ",
            "NOTE 5.  Model 182S S/N 18280617 thru 18280670 may differ structur ally.   ",
            "NOTE 6.  The following serials numbers are manufac tured under the name ",
            "Cessna Aircraft Company:    ",
            "Model 182S: 18280001 thru 18280944; Model 182T: 18280945 thru 18282368.   ",
            "NOTE 7.  Company name change effectiv e 7/29/15. ",
            "The following serials numbers are manufactured under the name  ",
            "Textron Aviation Inc.: Model 182T: 18282369 and On.   ",
            "NOTE 8.   The Means of Compliance for 23.2510 [23-64] is draft AC 20-184A.",
        )
        .to_string();
        document
    }

    fn t182t_document(serial_page: impl Into<String>) -> TcdsDocument {
        let mut document = document(vec![
            page(
                1,
                "I. Model 182, Skylane, 4 PCLM (Normal Category), Approved March 2, 1956",
            ),
            page(
                38,
                "XIV. Model T182T, Skylane, 4 PCLM (Normal Category), Appr oved 23 February 2001",
            ),
            page(39, serial_page),
        ]);
        document.metadata.exact_model = "T182T".to_string();
        document
    }

    fn model_182k_continuation_document(
        exact_row: &str,
        immediately_following_row: &str,
    ) -> TcdsDocument {
        let mut document = document(vec![
            page(
                9,
                "VI. Model 182K, Skylane, 4 PCLM (Normal Category), Approved August 3, 1966",
            ),
            page(
                10,
                format!(
                    concat!(
                        "VI. Models 182H, 182J, 182K, 182L (cont’d)\n",
                        "Serial Nos. Eligible\n",
                        "Model 182H: 634, 18255846 thru 18256684 (1965 Model)\n",
                        "Model 182J: 18256685 thru 18257625 (1966 Model)\n",
                        "{exact_row}\n",
                        "{immediately_following_row}\n",
                        "Model 182L: 18258506 thru 18259305 (1968 Model)\n",
                    ),
                    exact_row = exact_row,
                    immediately_following_row = immediately_following_row,
                ),
            ),
        ]);
        document.metadata.exact_model = "182K".to_string();
        document.metadata.models.push("182K".to_string());
        document
    }

    fn model_182r_document() -> TcdsDocument {
        let mut document = document(vec![
            page(
                19,
                "XII. Model 182R, 4 PCLM (Normal Category), Approved August 29, 1980",
            ),
            page(
                20,
                concat!(
                    "Serial Nos. Eligible  Model 182R/T182: 18267302, ",
                    "18267716 through 18268055 (1981 Model) ",
                    "Model 182R/T182: 18268056 through 18268293 (1982 Model) ",
                    "Model 182R: 18268542 through 18268586 (1986 Model)",
                ),
            ),
        ]);
        document.metadata.exact_model = "182R".to_string();
        document.metadata.models.push("182R".to_string());
        document
    }

    fn model_182p_document(serial_row: &str) -> TcdsDocument {
        let mut document = document(vec![
            page(
                13,
                "IX. Model 182P, Skylane, 4 PCLM (Normal Category), Approved October 8, 1971",
            ),
            page(
                14,
                format!(
                    "IX. Model 182P (cont’d)\nSerial Nos. Eligible\nModel 182P: {serial_row}\n"
                ),
            ),
        ]);
        document.metadata.exact_model = "182P".to_string();
        document.metadata.models.push("182P".to_string());
        document
    }

    fn model_182q_document() -> TcdsDocument {
        let mut document = document(vec![
            page(
                14,
                "X. Model 182Q, Skylane, 4 PCLM (Normal Category), Approved July 28, 1976",
            ),
            page(
                15,
                concat!(
                    "X. Model 182Q (cont’d)\n",
                    "Fuel Capacity Standard Range Tanks:\n",
                    "61 gal. (S/N 18263479, 18265176 thru 18266590)\n",
                    "Long Range Tanks:\n",
                    "92 gal. (S/N 18266591 thru 18267715)\n",
                    "Control Surface Movements\n",
                    "Serial Nos. Eligible        18265176 thru 18265965 (1977 Model)\n",
                    "                            18263479, 18265966 thru 18266590 (1978 Model)\n",
                    "                            18266591 thru 18267300 (1979 Model)\n",
                    "                            18267301 thru 18267715, except 18267302 (1980 Model)\n",
                    "XI. Model R182, Skylane RG, 4 PCLM (Normal Category), Approved July 7, 1977\n",
                ),
            ),
        ]);
        document.metadata.exact_model = "182Q".to_string();
        document.metadata.models.push("182Q".to_string());
        document
    }

    #[test]
    fn exact_same_document_headings_and_serial_range_bind_family() {
        let binding = bind_tcds_family(&valid_document(), "182T", "182", "18283169").unwrap();

        assert_eq!(binding.tcds_number, "3A13");
        assert_eq!(binding.canonical_family_name, "Skylane");
        assert_eq!(binding.exact_faa_model, "182T");
        assert_eq!(binding.observed_model, "182");
        assert_eq!(binding.faa_serial_key, "18283169");
        assert_eq!(binding.faa_model_heading.page_number, 34);
        assert!(binding
            .faa_model_heading
            .excerpt
            .starts_with("Model 182T, Skylane"));
        assert_eq!(binding.serial_eligibility.page_number, 35);
        assert_eq!(binding.serial_eligibility.first_serial_key, "18280945");
        assert_eq!(binding.serial_eligibility.last_serial_key, None);
        assert!(binding
            .serial_eligibility
            .excerpt
            .contains("Serial Numbers Eligible"));
        assert!(!binding.serial_eligibility.excerpt.contains("NOTE 7"));
        assert_eq!(
            binding.faa_model_heading.normalized_excerpt_sha256.len(),
            64
        );
    }

    #[test]
    fn flattened_real_world_heading_and_serial_rows_remain_supported() {
        let document = document(vec![
            page(
                1,
                "FAA publisher text from a preceding extraction span I. Model 182, Skylane, 4 PCLM (Normal Category), Approved March 2, 1956 trailing text",
            ),
            page(
                34,
                "FAA publisher text from a preceding extraction span XIII. Model 182T, Skylane, 4 PCLM (Normal Category), Approved 23 February 2001. trailing text",
            ),
            page(
                35,
                "page header Serial Numbers Eligible 182S: 18280001 thru 18280944 182T: 18280945 and On Data Pertinent to Models 182S and 182T",
            ),
        ]);

        let binding = bind_tcds_family(&document, "182T", "182", "18283169").unwrap();

        assert_eq!(binding.canonical_family_name, "Skylane");
        assert_eq!(
            binding.serial_eligibility.excerpt,
            "Serial Numbers Eligible 182S: 18280001 thru 18280944 182T: 18280945 and On"
        );
    }

    #[test]
    fn configuration_field_or_note_is_never_promoted_to_family() {
        assert_eq!(
            parse_model_family_heading(
                "I. Model 182T, 4 PCLM (Normal Category), Approved 23 February 2001."
            ),
            None
        );
        assert_eq!(
            parse_model_family_heading(
                "I. Model 182T, converted aircraft, see NOTE 7 for limitations."
            ),
            None
        );
        assert_eq!(
            parse_model_family_heading(
                "I. Model 182T, Skylane, 4 PCLM (Normal Category), Approved 23 February 2001."
            )
            .map(|(_, family, _)| family),
            Some("Skylane".to_string())
        );
    }

    #[test]
    fn bounded_intra_word_spaces_in_approved_preserve_original_evidence() {
        let document = t182t_document(
            "3A13 Page 39 XIV. Model T182T (cont’d) Propeller Limits \
             Serial Numbers Eligible  T18208001 and On",
        );

        let binding = bind_tcds_family(&document, "T182T", "182", "T18208316").unwrap();

        assert_eq!(
            binding.faa_model_heading.excerpt,
            "Model T182T, Skylane, 4 PCLM (Normal Category), Appr oved 23 February 2001"
        );
        assert_eq!(
            binding.faa_model_heading.normalized_excerpt_sha256,
            normalized_excerpt_sha256(&binding.faa_model_heading.excerpt)
        );
        assert_eq!(
            binding.serial_eligibility.excerpt,
            "Serial Numbers Eligible  T18208001 and On"
        );
    }

    #[test]
    fn repaired_approved_keyword_remains_token_year_and_window_bounded() {
        assert_eq!(
            fixed_ascii_keyword_positions("XAppr oved", "Approved").count(),
            0
        );
        assert_eq!(
            fixed_ascii_keyword_positions("Appr ovedX", "Approved").count(),
            0
        );
        assert_eq!(
            fixed_ascii_keyword_positions("Appr     oved", "Approved").count(),
            0
        );
        assert_eq!(
            fixed_ascii_keyword_positions("Appr\toved", "Approved").count(),
            0
        );
        assert!(
            parse_model_heading("XIV. Model T182T, Skylane, 4 PCLM, Appr oved 20010").is_none()
        );
        assert!(parse_model_heading(&format!(
            "XIV. Model T182T, Skylane, {}Appr oved 2001",
            "x".repeat(193)
        ))
        .is_none());
    }

    #[test]
    fn unlabeled_t182t_serial_is_bound_to_exact_same_page_section() {
        let document = t182t_document(
            "3A13 Page 39 XIV. Model T182T (cont’d) Propeller Limits \
             Serial Numbers Eligible  T18208001 and On",
        );

        let binding = bind_tcds_identity(&document, "T182T", "T18208316").unwrap();

        assert_eq!(binding.exact_faa_model, "T182T");
        assert_eq!(binding.faa_serial_key, "T18208316");
        assert_eq!(binding.serial_eligibility.model, "T182T");
        assert_eq!(binding.serial_eligibility.first_serial_key, "T18208001");
    }

    #[test]
    fn unlabeled_serial_never_borrows_missing_wrong_or_other_page_context() {
        for serial_page in [
            "Serial Numbers Eligible T18208001 and On",
            "XIV. Model 182T (cont’d) Serial Numbers Eligible T18208001 and On",
            "XIV. Model XT182T (cont’d) Serial Numbers Eligible T18208001 and On",
            "Data Pertinent to Model T182T Serial Numbers Eligible T18208001 and On",
            concat!(
                "XIV. Model T182T (cont’d) data ",
                "XV. Model 182T (cont’d) Serial Numbers Eligible T18208001 and On",
            ),
        ] {
            let document = t182t_document(serial_page);
            assert_eq!(
                bind_tcds_identity(&document, "T182T", "T18208316"),
                Err(TcdsFamilyBindingError::SerialEligibilityMissing),
                "{serial_page:?}"
            );
        }

        let mut other_page = t182t_document("Serial Numbers Eligible T18208001 and On");
        other_page
            .pages
            .insert(2, page(38, "XIV. Model T182T (cont’d) Propeller Limits"));
        other_page.page_count = other_page.pages.len();
        assert_eq!(
            bind_tcds_identity(&other_page, "T182T", "T18208316"),
            Err(TcdsFamilyBindingError::InvalidDocument(
                "physical page text"
            ))
        );
        other_page.pages[2].page_number = 37;
        assert_eq!(
            bind_tcds_identity(&other_page, "T182T", "T18208316"),
            Err(TcdsFamilyBindingError::SerialEligibilityMissing)
        );
    }

    #[test]
    fn familyless_182r_heading_can_prove_identity_but_not_invent_a_family() {
        let document = model_182r_document();

        let singleton = bind_tcds_identity(&document, "182R", "18267302").unwrap();
        assert_eq!(singleton.exact_faa_model, "182R");
        assert_eq!(singleton.serial_eligibility.first_serial_key, "18267302");
        assert_eq!(
            singleton.serial_eligibility.last_serial_key.as_deref(),
            Some("18267302")
        );

        let bounded = bind_tcds_identity(&document, "182R", "18268550").unwrap();
        assert_eq!(bounded.serial_eligibility.first_serial_key, "18268542");
        assert_eq!(
            bounded.serial_eligibility.last_serial_key.as_deref(),
            Some("18268586")
        );
        assert_eq!(
            bind_tcds_family(&document, "182R", "182R", "18268550"),
            Err(TcdsFamilyBindingError::ModelHeadingMissing)
        );
        assert_eq!(
            parse_model_heading(
                "XII. Model 182R, 4 PCLM (Normal Category), Approved August 29, 1980"
            )
            .and_then(|heading| heading.named_family),
            None
        );
    }

    #[test]
    fn composite_serial_table_model_labels_are_exact_tokens() {
        let row = parse_serial_eligibility_row_prefix(
            "Model 182R/T182: 18267716 through 18268055 (1981 Model)",
        )
        .unwrap();
        assert_eq!(row.models, vec!["182R", "T182"]);
        assert!(!row.models.iter().any(|model| model == "182"));
    }

    #[test]
    fn exact_exclusion_splits_persistable_serial_scope_around_the_hole() {
        let document = model_182p_document("18263476 thru 18264295 except 18263479 (1975 Model)");

        let below = bind_tcds_identity(&document, "182P", "18263478").unwrap();
        assert_eq!(below.serial_eligibility.first_serial_key, "18263476");
        assert_eq!(
            below.serial_eligibility.last_serial_key.as_deref(),
            Some("18263478")
        );
        assert!(below.serial_eligibility.excerpt.contains("except 18263479"));

        let above = bind_tcds_identity(&document, "182P", "18263616").unwrap();
        assert_eq!(above.serial_eligibility.first_serial_key, "18263480");
        assert_eq!(
            above.serial_eligibility.last_serial_key.as_deref(),
            Some("18264295")
        );
        assert_eq!(
            bind_tcds_identity(&document, "182P", "18263479"),
            Err(TcdsFamilyBindingError::SerialNotEligible)
        );
    }

    #[test]
    fn multiple_exact_exclusions_produce_only_contiguous_ranges() {
        let row = parse_serial_eligibility_row_prefix(
            "Model 182P: 18263476 thru 18264295 except 18263479, 18264000 and 18264001 (1975 Model)",
        )
        .unwrap();
        assert_eq!(
            row.ranges,
            vec![
                ParsedSerialEligibilityRange {
                    first_serial_key: "18263476".to_string(),
                    last_serial_key: Some("18263478".to_string()),
                },
                ParsedSerialEligibilityRange {
                    first_serial_key: "18263480".to_string(),
                    last_serial_key: Some("18263999".to_string()),
                },
                ParsedSerialEligibilityRange {
                    first_serial_key: "18264002".to_string(),
                    last_serial_key: Some("18264295".to_string()),
                },
            ]
        );
    }

    #[test]
    fn malformed_or_ambiguous_serial_exclusions_fail_closed() {
        for row in [
            "18263476 thru 18264295 except UNKNOWN (1975 Model)",
            "18263476 thru 18264295 except 18265000 (1975 Model)",
            "18263476 thru 18264295 except 63479 (1975 Model)",
            "18263476 thru 18264295 except 18263479 or 18263480 (1975 Model)",
            "18263476 thru 18264295 except 18263479, 18263479 (1975 Model)",
        ] {
            assert_eq!(
                bind_tcds_identity(&model_182p_document(row), "182P", "18263616"),
                Err(TcdsFamilyBindingError::SerialEligibilityMissing),
                "{row:?}"
            );
        }
    }

    #[test]
    fn exact_labeled_comma_row_owns_only_its_immediate_unlabeled_continuation() {
        let document = model_182k_continuation_document(
            "Model 182K: 18255845, 18257626 thru 18257698,",
            "18257700 thru 18258505 (1967 Model)",
        );

        for (serial, expected_first, expected_last) in [
            ("18255845", "18255845", "18255845"),
            ("18257650", "18257626", "18257698"),
            ("18258157", "18257700", "18258505"),
        ] {
            let binding = bind_tcds_identity(&document, "182K", serial).unwrap_or_else(|error| {
                panic!("exact 182K serial {serial} should bind: {error:?}")
            });
            assert_eq!(binding.serial_eligibility.first_serial_key, expected_first);
            assert_eq!(
                binding.serial_eligibility.last_serial_key.as_deref(),
                Some(expected_last)
            );
            assert!(
                binding
                    .serial_eligibility
                    .excerpt
                    .contains("18257700 thru 18258505 (1967 Model)"),
                "{serial}: {:?}",
                binding.serial_eligibility.excerpt
            );
        }
        assert_eq!(
            bind_tcds_identity(&document, "182K", "18257699"),
            Err(TcdsFamilyBindingError::SerialNotEligible),
            "the gap between disjoint FAA intervals must remain ineligible"
        );
    }

    #[test]
    fn malformed_or_nonowned_comma_continuations_fail_closed() {
        for following_row in [
            "Propeller S/N 18257700 thru 18258505 (1967 Model)",
            "Model 182J: 18257700 thru 18258505 (1967 Model)",
            "\n18257700 thru 18258505 (1967 Model)",
            "18257700 and On",
        ] {
            let document = model_182k_continuation_document(
                "Model 182K: 18255845, 18257626 thru 18257698,",
                following_row,
            );
            assert_eq!(
                bind_tcds_identity(&document, "182K", "18257650"),
                Err(TcdsFamilyBindingError::SerialEligibilityMissing),
                "an unfinished exact-model row must not admit its valid-looking prefix: {following_row:?}"
            );
        }

        let no_comma = model_182k_continuation_document(
            "Model 182K: 18255845, 18257626 thru 18257698",
            "18257700 thru 18258505 (1967 Model)",
        );
        assert_eq!(
            bind_tcds_identity(&no_comma, "182K", "18258157"),
            Err(TcdsFamilyBindingError::SerialNotEligible),
            "an unlabeled row cannot borrow ownership without the preceding comma"
        );
    }

    #[test]
    fn actual_multiline_182q_abbreviated_serial_table_binds_every_exact_range() {
        let document = model_182q_document();
        let first_row_text = "18265176 thru 18265965 (1977 Model)\n";
        let first_row = parse_unlabeled_model_eligibility_row_prefix(first_row_text, "182Q")
            .expect("the actual first bounded row is table-shaped");
        assert_eq!(&first_row_text[first_row.row_end..], "\n");
        let comma_row = parse_unlabeled_model_eligibility_row_prefix(
            "18263479, 18265966 thru 18266590 (1978 Model)",
            "182Q",
        )
        .expect("the actual comma row is table-shaped");
        assert_eq!(comma_row.ranges.len(), 2);
        assert_eq!(comma_row.ranges[0].first_serial_key, "18263479");
        assert_eq!(
            comma_row.ranges[0].last_serial_key.as_deref(),
            Some("18263479")
        );
        for (serial, expected_first, expected_last) in [
            ("18265500", "18265176", "18265965"),
            ("18263479", "18263479", "18263479"),
            ("18266000", "18265966", "18266590"),
            ("18267000", "18266591", "18267300"),
            ("18267301", "18267301", "18267301"),
            ("18267400", "18267303", "18267715"),
        ] {
            let binding =
                bind_tcds_family(&document, "182Q", "182Q", serial).unwrap_or_else(|error| {
                    panic!("exact 182Q serial {serial} should bind: {error:?}")
                });
            assert_eq!(binding.canonical_family_name, "Skylane");
            assert_eq!(binding.serial_eligibility.first_serial_key, expected_first);
            assert_eq!(
                binding.serial_eligibility.last_serial_key.as_deref(),
                Some(expected_last)
            );
            assert!(
                binding
                    .serial_eligibility
                    .excerpt
                    .starts_with("Serial Nos. Eligible"),
                "evidence must retain the exact abbreviated FAA marker"
            );
        }

        assert_eq!(
            bind_tcds_identity(&document, "182Q", "18267302"),
            Err(TcdsFamilyBindingError::SerialNotEligible),
            "the explicit FAA exclusion must remain a hole"
        );
        assert_eq!(
            bind_tcds_identity(&document, "182Q", "18264000"),
            Err(TcdsFamilyBindingError::SerialNotEligible),
            "equipment S/N ranges before the marker are not model eligibility"
        );
    }

    #[test]
    fn abbreviated_unlabeled_table_never_borrows_equipment_or_wrong_model_ranges() {
        let mut equipment = model_182q_document();
        equipment.pages[1].text = concat!(
            "X. Model 182Q (cont’d)\n",
            "Serial Nos. Eligible\n",
            "Propeller S/N 18265176 thru 18265965\n",
            "18265176 thru 18265965 (1977 Model)\n",
        )
        .to_string();
        assert_eq!(
            bind_tcds_identity(&equipment, "182Q", "18265500"),
            Err(TcdsFamilyBindingError::SerialEligibilityMissing),
            "the first non-table equipment row must close the eligibility table"
        );

        let mut wrong_model = model_182q_document();
        wrong_model.pages[1].text = concat!(
            "XI. Model R182 (cont’d)\n",
            "Serial Nos. Eligible\n",
            "18265176 thru 18265965 (1977 Model)\n",
        )
        .to_string();
        assert_eq!(
            bind_tcds_identity(&wrong_model, "182Q", "18265500"),
            Err(TcdsFamilyBindingError::SerialEligibilityMissing),
            "unlabeled bounded rows require the exact model section"
        );
    }

    #[test]
    fn bounded_unlabeled_rows_require_a_short_line_model_year_annotation() {
        let annotated = "18265176 thru 18265965 (1977 Model)\n";
        let row = parse_unlabeled_model_eligibility_row_prefix(annotated, "182Q")
            .expect("a short physical row with an exact model-year annotation is valid");
        assert_eq!(&annotated[row.row_end..], "\n");

        assert!(
            parse_unlabeled_model_eligibility_row_prefix("18265176 thru 18265965\n", "182Q")
                .is_none(),
            "a bare bounded equipment-like range must fail closed"
        );
        assert!(
            parse_unlabeled_model_eligibility_row_prefix(
                "18255845, 18257626 thru 18257698,\n",
                "182K"
            )
            .is_some(),
            "a comma-terminated FAA table row may continue immediately"
        );
        assert!(
            parse_unlabeled_model_eligibility_row_prefix("T18208001 and On\n", "T182T").is_some(),
            "the pre-existing exact-section open range remains valid"
        );
    }

    #[test]
    fn manufacturer_serial_lineage_and_holder_transfer_are_exact_and_case_bound() {
        let mut document =
            t182t_document("XIV. Model T182T (cont’d) Serial Numbers Eligible T18208001 and On");
        document.pages[0].text = concat!(
            "Type Certificate Holder Record Cessna Aircraft Company transferred to ",
            "Textron Aviation Inc. on July 29, 2015 ",
            "I. Model 182, Skylane, Approved March 2, 1956",
        )
        .to_string();
        document.pages.push(page(
            42,
            concat!(
                "Data Pertinent to Model T182T (cont’d)\n",
                "NOTE 5. The following serials numbers are manufactured under the name ",
                "Cessna Aircraft Company: T18208001 thru T18209100. ",
                "NOTE 6. The following serials numbers are manufactured under the name ",
                "Textron Aviation Inc.: T18209101 and On.",
            ),
        ));
        document.page_count = document.pages.len();

        let cessna = bind_tcds_make_lineage(&document, "T182T", "T18208545")
            .unwrap()
            .unwrap();
        assert_eq!(
            cessna
                .manufacturer_serial_eligibility
                .as_ref()
                .unwrap()
                .manufacturer_name,
            "Cessna Aircraft Company"
        );
        assert!(cessna
            .manufacturer_serial_eligibility
            .as_ref()
            .unwrap()
            .excerpt
            .contains("T18208001 thru T18209100"));
        let transfer = cessna.holder_transfer.unwrap();
        assert_eq!(transfer.former_holder_name, "Cessna Aircraft Company");
        assert_eq!(transfer.current_holder_name, "Textron Aviation Inc.");
        assert_eq!(transfer.effective_date_text, "July 29, 2015");
        assert!(transfer
            .excerpt
            .starts_with("Type Certificate Holder Record"));

        let textron = bind_tcds_make_lineage(&document, "T182T", "T18209101")
            .unwrap()
            .unwrap();
        assert_eq!(
            textron
                .manufacturer_serial_eligibility
                .as_ref()
                .unwrap()
                .manufacturer_name,
            "Textron Aviation Inc."
        );
        assert_eq!(
            textron
                .manufacturer_serial_eligibility
                .as_ref()
                .unwrap()
                .last_serial_key,
            None
        );
    }

    #[test]
    fn real_3a13_page_38_split_word_binds_exact_182t_and_182s_manufacturer_scopes() {
        let document = real_3a13_page_38_make_lineage_document("182T");
        let lineage = bind_tcds_make_lineage(&document, "182T", "18282153")
            .unwrap()
            .unwrap();
        let manufacturer = lineage.manufacturer_serial_eligibility.unwrap();
        assert_eq!(manufacturer.page_number, 38);
        assert_eq!(manufacturer.manufacturer_name, "Cessna Aircraft Company");
        assert_eq!(manufacturer.model, "182T");
        assert_eq!(manufacturer.first_serial_key, "18280945");
        assert_eq!(manufacturer.last_serial_key.as_deref(), Some("18282368"));
        assert!(manufacturer.excerpt.contains("manufac tured"));
        assert!(!manufacturer.excerpt.contains("manufactured"));
        assert_eq!(
            manufacturer.normalized_excerpt_sha256,
            normalized_excerpt_sha256(&manufacturer.excerpt)
        );
        let transfer = lineage.holder_transfer.unwrap();
        assert_eq!(transfer.former_holder_name, "Cessna Aircraft Company");
        assert_eq!(transfer.current_holder_name, "Textron Aviation Inc.");
        assert_eq!(transfer.effective_date_text, "July 29, 2015");

        let document = real_3a13_page_38_make_lineage_document("182S");
        let lineage = bind_tcds_make_lineage(&document, "182S", "18280620")
            .unwrap()
            .unwrap();
        let manufacturer = lineage.manufacturer_serial_eligibility.unwrap();
        assert_eq!(manufacturer.manufacturer_name, "Cessna Aircraft Company");
        assert_eq!(manufacturer.model, "182S");
        assert_eq!(manufacturer.first_serial_key, "18280001");
        assert_eq!(manufacturer.last_serial_key.as_deref(), Some("18280944"));
    }

    #[test]
    fn manufacturer_marker_repair_is_exact_and_fails_closed_on_ambiguity() {
        let exact = "The following serials numbers are manufactured under the name Cessna Aircraft";
        let extracted =
            "The following serials numbers are manufac tured under the name Cessna Aircraft";
        assert_eq!(manufacturer_serial_marker_positions(exact).len(), 1);
        let marker = manufacturer_serial_marker_positions(extracted)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            &extracted[marker.start..marker.end],
            "The following serials numbers are manufac tured under the name"
        );
        for invalid in [
            "The following serials numbers are manu fac tured under the name Cessna",
            "The following serials numbers are manufac  tured under the name Cessna",
            "The following serials numbers are manufac\ttured under the name Cessna",
            "The following serials numbers are manufac\ntured under the name Cessna",
            "XThe following serials numbers are manufac tured under the name Cessna",
            "The following serials numbers are manufac turedX under the name Cessna",
        ] {
            assert!(
                manufacturer_serial_marker_positions(invalid).is_empty(),
                "{invalid:?}"
            );
        }

        let mut document = real_3a13_page_38_make_lineage_document("182T");
        document.pages[3].text.push_str(concat!(
            " The following serials numbers are manufac tured under the name ",
            "Impostor Aircraft Company: Model 182T: 18280945 thru 18282368.",
        ));
        assert_eq!(
            bind_tcds_make_lineage(&document, "182T", "18282153"),
            Err(TcdsFamilyBindingError::MakeLineageAmbiguous)
        );
    }

    #[test]
    fn holder_metadata_allows_the_faa_terminal_period_difference_only() {
        let mut document =
            t182t_document("XIV. Model T182T (cont’d) Serial Numbers Eligible T18208001 and On");
        document.pages[0].text = concat!(
            "Type Certificate Holder Record Cessna Aircraft Company transferred to ",
            "Textron Aviation Inc. on July 29, 2015 ",
            "I. Model 182, Skylane, Approved March 2, 1956",
        )
        .to_string();
        document.metadata.tc_holder = Some("TEXTRON AVIATION INC".to_string());
        let lineage = bind_tcds_make_lineage(&document, "T182T", "T18208545")
            .expect("terminal-period differences are equivalent FAA holder labels")
            .expect("the exact TCDS holder transfer should remain available");
        assert_eq!(
            lineage
                .holder_transfer
                .expect("holder transfer")
                .current_holder_name,
            "Textron Aviation Inc."
        );

        document.metadata.tc_holder = Some("TEXTRON AVIATION".to_string());
        assert_eq!(
            bind_tcds_make_lineage(&document, "T182T", "T18208545"),
            Err(TcdsFamilyBindingError::HolderTransferAmbiguous)
        );
    }

    #[test]
    fn manufacturer_lineage_never_borrows_a_different_model_or_ambiguous_range() {
        let mut document =
            t182t_document("XIV. Model T182T (cont’d) Serial Numbers Eligible T18208001 and On");
        document.pages.push(page(
            42,
            concat!(
                "Data Pertinent to Model T182T (cont’d)\n",
                "The following serials numbers are manufactured under the name ",
                "Cessna Aircraft Company: Model 182T: T18208001 and On.",
            ),
        ));
        document.page_count = document.pages.len();
        assert_eq!(
            bind_tcds_make_lineage(&document, "T182T", "T18208545").unwrap(),
            None
        );

        document.pages[3].text = concat!(
            "Data Pertinent to Model T182T (cont’d)\n",
            "The following serials numbers are manufactured under the name ",
            "Cessna Aircraft Company: T18208001 and On. ",
            "The following serials numbers are manufactured under the name ",
            "Textron Aviation Inc.: T18208001 and On.",
        )
        .to_string();
        assert_eq!(
            bind_tcds_make_lineage(&document, "T182T", "T18208545"),
            Err(TcdsFamilyBindingError::MakeLineageAmbiguous)
        );
    }

    #[test]
    fn holder_transfer_remains_available_without_a_manufacturer_serial_note() {
        let mut document = model_182r_document();
        document.pages.push(page(
            1,
            concat!(
                "Type Certificate Holder Record Cessna Aircraft Company transferred to ",
                "Textron Aviation Inc. on July 29, 2015",
            ),
        ));
        document.page_count = document.pages.len();

        let lineage = bind_tcds_make_lineage(&document, "182R", "18268550")
            .unwrap()
            .unwrap();

        assert!(lineage.manufacturer_serial_eligibility.is_none());
        assert_eq!(
            lineage.holder_transfer.unwrap().former_holder_name,
            "Cessna Aircraft Company"
        );
    }

    #[test]
    fn retained_marketing_label_is_audited_without_becoming_a_tcds_heading_claim() {
        let mut document = valid_document();
        document.pages.retain(|page| page.page_number != 1);
        document.page_count = document.pages.len();

        let binding = bind_tcds_family(&document, "182T", "182 SKYLANE", "18283169").unwrap();
        assert_eq!(binding.observed_model, "182 SKYLANE");
        assert_eq!(binding.canonical_family_name, "Skylane");
    }

    #[test]
    fn exact_model_does_not_match_t182t_token() {
        let mut document = valid_document();
        document.pages[1].text =
            "XIII. Model T182T, Turbo Skylane, Approved 23 February 2001.\n".to_string();

        assert_eq!(
            bind_tcds_family(&document, "182T", "182", "18283169"),
            Err(TcdsFamilyBindingError::ModelHeadingMissing)
        );
    }

    #[test]
    fn two_distinct_exact_designation_headings_are_ambiguous() {
        let mut document = valid_document();
        document.pages.push(page(
            36,
            "XIV. Model 182T, Different Family, 4 PCLM, Approved 23 February 2001",
        ));
        document.page_count = document.pages.len();

        assert_eq!(
            bind_tcds_family(&document, "182T", "182", "18283169"),
            Err(TcdsFamilyBindingError::ModelHeadingAmbiguous)
        );
    }

    #[test]
    fn certification_approval_and_revision_dates_never_supply_scope() {
        let binding = bind_tcds_family(&valid_document(), "182T", "182", "18283169").unwrap();

        assert_eq!(binding.revision_date.as_deref(), Some("08/07/2024"));
        assert!(binding.faa_model_heading.excerpt.contains("2001"));
        assert_eq!(binding.serial_eligibility.first_serial_key, "18280945");
        assert_eq!(binding.serial_eligibility.last_serial_key, None);
    }

    #[test]
    fn serial_below_open_eligibility_range_is_rejected() {
        assert_eq!(
            bind_tcds_family(&valid_document(), "182T", "182", "18280000"),
            Err(TcdsFamilyBindingError::SerialNotEligible)
        );
    }

    #[test]
    fn bounded_exact_serial_range_is_supported() {
        let mut document = valid_document();
        document.pages[2].text =
            "Serial Numbers Eligible\n182T: 18280945 thru 18289999\n".to_string();
        let binding = bind_tcds_family(&document, "182T", "182", "18283169").unwrap();
        assert_eq!(
            binding.serial_eligibility.last_serial_key.as_deref(),
            Some("18289999")
        );
        assert_eq!(
            bind_tcds_family(&document, "182T", "182", "18290000"),
            Err(TcdsFamilyBindingError::SerialNotEligible)
        );
    }

    #[test]
    fn note_seven_manufacturer_attribution_is_not_model_eligibility() {
        let mut document = valid_document();
        document.pages.retain(|page| page.page_number != 35);
        document.page_count = document.pages.len();

        assert_eq!(
            bind_tcds_family(&document, "182T", "182", "18283169"),
            Err(TcdsFamilyBindingError::SerialEligibilityMissing)
        );
    }

    #[test]
    fn serial_table_does_not_borrow_a_later_note_or_section_row() {
        let mut note = valid_document();
        note.pages[2].text = concat!(
            "Serial Numbers Eligible\n",
            "182S: 18280001 thru 18280944\n",
            "NOTE 7. Textron Aviation Inc.: Model 182T: 18282369 and On.\n",
        )
        .to_string();
        assert_eq!(
            bind_tcds_family(&note, "182T", "182", "18283169"),
            Err(TcdsFamilyBindingError::SerialEligibilityMissing)
        );

        let mut later_section = valid_document();
        later_section.pages[2].text = concat!(
            "Serial Numbers Eligible\n",
            "182S: 18280001 thru 18280944\n",
            "Data Pertinent to Models 182S and 182T\n",
            "182T: 18280945 and On\n",
        )
        .to_string();
        assert_eq!(
            bind_tcds_family(&later_section, "182T", "182", "18283169"),
            Err(TcdsFamilyBindingError::SerialEligibilityMissing)
        );
    }

    #[test]
    fn conflicting_serial_eligibility_ranges_fail_closed() {
        let mut document = valid_document();
        document
            .pages
            .push(page(36, "Serial Numbers Eligible\n182T: 18282000 and On\n"));
        document.page_count = document.pages.len();

        assert_eq!(
            bind_tcds_family(&document, "182T", "182", "18283169"),
            Err(TcdsFamilyBindingError::SerialEligibilityAmbiguous)
        );
    }

    #[test]
    fn alphanumeric_serials_compare_only_with_the_same_prefix_and_width() {
        let mut document = valid_document();
        document.pages[2].text = "Serial Numbers Eligible\n182T: T18208001 and On\n".to_string();

        assert!(bind_tcds_family(&document, "182T", "182", "T18208316").is_ok());
        assert_eq!(
            bind_tcds_family(&document, "182T", "182", "18208316"),
            Err(TcdsFamilyBindingError::SerialNotEligible)
        );
        assert_eq!(
            bind_tcds_family(&document, "182T", "182", "T1828316"),
            Err(TcdsFamilyBindingError::SerialNotEligible)
        );
    }

    #[test]
    fn document_model_and_source_provenance_are_rechecked() {
        let mut wrong_model = valid_document();
        wrong_model.metadata.exact_model = "T182T".to_string();
        assert_eq!(
            bind_tcds_family(&wrong_model, "182T", "182", "18283169"),
            Err(TcdsFamilyBindingError::InvalidDocument(
                "metadata or PDF provenance"
            ))
        );

        let mut wrong_source = valid_document();
        wrong_source.source_url =
            "https://example.test/api/drs/data-pull/download/document".to_string();
        assert_eq!(
            bind_tcds_family(&wrong_source, "182T", "182", "18283169"),
            Err(TcdsFamilyBindingError::InvalidDocument("source URL"))
        );
    }
}
