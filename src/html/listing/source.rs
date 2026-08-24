//! Publisher-aware listing text supplied to extraction models.
//!
//! Controller pages expose the authoritative listing facts as one stable
//! title/price/specification structure. Extracting that structure directly
//! avoids flattening unrelated page chrome and, importantly, keeps every
//! specification label and value in a separate source unit.

use std::{collections::BTreeSet, fmt};

use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use url::Url;

use crate::html::clean::{clean_listing_html, ListingBodyEvidenceUnits, PublisherTextExtractor};
use crate::html::listing::media::{
    is_controller_source_host, validate_controller_listing_source_url, MAX_RETAINED_HTML_BYTES,
};

const CONTROLLER_MAIN_CLASS: &str = "detail__main-content";
const CONTROLLER_TITLE_CLASS: &str = "detail__title";
const CONTROLLER_PRICE_CLASS: &str = "listing-prices__retail-price";
const CONTROLLER_SPECS_CLASS: &str = "detail__specs";
const CONTROLLER_HEADING_CLASS: &str = "detail__specs-heading";
const CONTROLLER_WRAPPER_CLASS: &str = "detail__specs-wrapper";
const CONTROLLER_LABEL_CLASS: &str = "detail__specs-label";
const CONTROLLER_VALUE_CLASS: &str = "detail__specs-value";
const CONTROLLER_TRAILING_CLASSES: &[&str] = &["detail__specs-service-logs", "acc-container"];

const MAX_CONTROLLER_SOURCE_BYTES: usize = 24_000;
const MAX_CONTROLLER_TITLE_BYTES: usize = 512;
const MAX_CONTROLLER_PRICE_BYTES: usize = 128;
const MAX_CONTROLLER_SECTION_BYTES: usize = 256;
const MAX_CONTROLLER_SECTIONS: usize = 32;
const MAX_CONTROLLER_FIELDS: usize = 256;
const MAX_CONTROLLER_LABEL_BYTES: usize = 512;
const MAX_CONTROLLER_VALUE_BYTES: usize = 16_000;

const TITLE_OPEN: &str = "[CONTROLLER TITLE]";
const TITLE_CLOSE: &str = "[/CONTROLLER TITLE]";
const PRICE_OPEN: &str = "[CONTROLLER PRIMARY ASKING PRICE]";
const PRICE_CLOSE: &str = "[/CONTROLLER PRIMARY ASKING PRICE]";
const AVAILABILITY_OPEN: &str = "[CONTROLLER LISTING OFFER AVAILABILITY]";
const AVAILABILITY_CLOSE: &str = "[/CONTROLLER LISTING OFFER AVAILABILITY]";
const SECTION_OPEN: &str = "[CONTROLLER SPEC SECTION]";
const SECTION_CLOSE: &str = "[/CONTROLLER SPEC SECTION]";
const FIELD_OPEN: &str = "[CONTROLLER FIELD]";
const FIELD_CLOSE: &str = "[/CONTROLLER FIELD]";
const LABEL_OPEN: &str = "[LABEL]";
const LABEL_CLOSE: &str = "[/LABEL]";
const VALUE_OPEN: &str = "[VALUE]";
const VALUE_CLOSE: &str = "[/VALUE]";
const RESERVED_MARKERS: &[&str] = &[
    TITLE_OPEN,
    TITLE_CLOSE,
    PRICE_OPEN,
    PRICE_CLOSE,
    AVAILABILITY_OPEN,
    AVAILABILITY_CLOSE,
    SECTION_OPEN,
    SECTION_CLOSE,
    FIELD_OPEN,
    FIELD_CLOSE,
    LABEL_OPEN,
    LABEL_CLOSE,
    VALUE_OPEN,
    VALUE_CLOSE,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListingSourceError {
    InvalidControllerSourceUrl(String),
    RetainedHtmlTooLarge {
        actual_bytes: usize,
        maximum_bytes: usize,
    },
    InvalidControllerStructure(String),
    ControllerSourceOverflow {
        actual_bytes: usize,
        maximum_bytes: usize,
    },
}

impl fmt::Display for ListingSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidControllerSourceUrl(error) => {
                write!(formatter, "invalid Controller listing source URL: {error}")
            }
            Self::RetainedHtmlTooLarge {
                actual_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "retained Controller HTML is {actual_bytes} bytes; maximum is {maximum_bytes}"
            ),
            Self::InvalidControllerStructure(error) => {
                write!(formatter, "invalid Controller listing structure: {error}")
            }
            Self::ControllerSourceOverflow {
                actual_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "Controller listing source is {actual_bytes} bytes; maximum is {maximum_bytes}"
            ),
        }
    }
}

impl std::error::Error for ListingSourceError {}

#[derive(Debug)]
struct ControllerField {
    section: String,
    label: String,
    value: String,
}

#[derive(Debug)]
struct ControllerListingSource {
    extraction_text: String,
    evidence_units: Vec<String>,
}

/// Publisher-aware evidence units admitted by the same source adapter used to
/// construct extraction-model input.
///
/// Controller evidence is restricted to one exact specification value. Labels,
/// adjacent values, price/page chrome, and JSON-LD cannot become avionics
/// evidence. Other publishers retain the generic visible-body unit contract.
#[derive(Debug)]
pub(crate) enum ListingEvidenceUnits {
    Controller(Vec<String>),
    Generic(ListingBodyEvidenceUnits),
}

impl ListingEvidenceUnits {
    pub(crate) fn contains_exact_span(&self, evidence: &str) -> bool {
        match self {
            Self::Controller(units) => units.iter().any(|unit| {
                crate::html::clean::exact_normalized_source_span_occurs(unit, evidence)
            }),
            Self::Generic(units) => units.contains_exact_span(evidence),
        }
    }
}

/// Build the bounded source text used for listing extraction.
///
/// A URL on a Controller host is never sent through the broad scraper. Once
/// the publisher is recognized, an invalid route or changed/ambiguous DOM is
/// an error rather than permission to mix navigation, ads, finance offers, or
/// related listings into the extraction corpus.
pub fn listing_extraction_source(
    source_url: &str,
    retained_html: &str,
) -> Result<String, ListingSourceError> {
    if is_controller_source_host(source_url) {
        controller_listing_source(source_url, retained_html).map(|source| source.extraction_text)
    } else {
        Ok(clean_listing_html(retained_html))
    }
}

/// Build the exact source-unit index used to validate model evidence.
pub(crate) fn listing_evidence_units(
    source_url: &str,
    retained_html: &str,
) -> Result<ListingEvidenceUnits, ListingSourceError> {
    if is_controller_source_host(source_url) {
        controller_listing_source(source_url, retained_html)
            .map(|source| ListingEvidenceUnits::Controller(source.evidence_units))
    } else {
        Ok(ListingEvidenceUnits::Generic(
            ListingBodyEvidenceUnits::from_html(retained_html),
        ))
    }
}

/// Rebind one checkpoint occurrence to the bounded text produced by this
/// source adapter.
///
/// Controller text retains exact field envelopes, so only the unique
/// `Avionics/Radios` value is eligible. Generic adapter text retains one
/// normalized visible unit per line; matching is restricted to a single line
/// so a flattened cross-unit substring cannot become identity evidence.
pub(crate) fn listing_extraction_source_contains_exact_avionics_occurrence(
    source_url: &str,
    extraction_source: &str,
    evidence: &str,
) -> bool {
    let evidence = evidence.trim();
    if evidence.is_empty() {
        return false;
    }
    if is_controller_source_host(source_url) {
        validate_controller_listing_source_url(source_url).is_ok()
            && controller_avionics_value_from_extraction_source(extraction_source).is_some_and(
                |value| crate::html::clean::exact_normalized_source_span_occurs(value, evidence),
            )
    } else {
        extraction_source
            .lines()
            .any(|unit| crate::html::clean::exact_normalized_source_span_occurs(unit, evidence))
    }
}

/// Return whether one occurrence is a complete line in the exact Controller
/// `Avionics/Radios` value emitted by this adapter.
pub(crate) fn controller_extraction_source_has_exact_avionics_line(
    source_url: &str,
    extraction_source: &str,
    evidence: &str,
) -> bool {
    let evidence = evidence.trim();
    !evidence.is_empty()
        && validate_controller_listing_source_url(source_url).is_ok()
        && controller_avionics_value_from_extraction_source(extraction_source)
            .is_some_and(|value| value.lines().any(|line| line.trim() == evidence))
}

fn controller_avionics_value_from_extraction_source(source: &str) -> Option<&str> {
    let mut accepted = None;
    let field_marker = format!("{FIELD_OPEN}\n{LABEL_OPEN}\n");
    let label_close = format!("\n{LABEL_CLOSE}\n{VALUE_OPEN}\n");
    let value_close = format!("\n{VALUE_CLOSE}\n{FIELD_CLOSE}");
    let field_count = source.match_indices(FIELD_OPEN).count();
    let mut parsed_count = 0usize;
    for field in source.split(&field_marker).skip(1) {
        parsed_count = parsed_count.saturating_add(1);
        let (label, after_label) = field.split_once(&label_close)?;
        let (value, after_value) = after_label.split_once(&value_close)?;
        if after_value.starts_with('\n') || after_value.is_empty() {
            if label == "Avionics/Radios" && accepted.replace(value).is_some() {
                return None;
            }
        } else {
            return None;
        }
    }
    (field_count > 0 && parsed_count == field_count)
        .then_some(accepted)
        .flatten()
}

fn controller_listing_source(
    source_url: &str,
    retained_html: &str,
) -> Result<ControllerListingSource, ListingSourceError> {
    let source_url = validate_controller_listing_source_url(source_url)
        .map_err(|error| ListingSourceError::InvalidControllerSourceUrl(error.to_string()))?;
    if retained_html.len() > MAX_RETAINED_HTML_BYTES {
        return Err(ListingSourceError::RetainedHtmlTooLarge {
            actual_bytes: retained_html.len(),
            maximum_bytes: MAX_RETAINED_HTML_BYTES,
        });
    }

    let document = Html::parse_document(retained_html);
    let listing_id = controller_listing_id(&source_url)?;
    let offer_availability = controller_offer_availability(&document, &listing_id)?;
    let text = PublisherTextExtractor::new(&document);
    let main = unique_element(
        document.root_element(),
        "main#main-content.detail__main-content",
        CONTROLLER_MAIN_CLASS,
        "listing main",
    )?;
    let title_element = unique_element(
        main,
        "h1.detail__title",
        CONTROLLER_TITLE_CLASS,
        "listing title",
    )?;
    let price_element = unique_element(
        main,
        ".listing-prices__retail-price",
        CONTROLLER_PRICE_CLASS,
        "primary asking price",
    )?;
    let specs = unique_element(
        main,
        ".detail__specs",
        CONTROLLER_SPECS_CLASS,
        "specifications",
    )?;

    let title = bounded_element_text(
        &text,
        title_element,
        "listing title",
        MAX_CONTROLLER_TITLE_BYTES,
    )?;
    let price = bounded_element_text(
        &text,
        price_element,
        "primary asking price",
        MAX_CONTROLLER_PRICE_BYTES,
    )?;
    let fields = parse_controller_fields(&text, specs)?;

    // Each specification value is one authoritative semantic evidence unit.
    // Preserve this index before consuming fields into the model-source
    // envelope so evidence validation and extraction cannot disagree about
    // publisher boundaries.
    let evidence_units = fields.iter().map(|field| field.value.clone()).collect();

    let mut source = String::new();
    push_envelope(&mut source, TITLE_OPEN, &title, TITLE_CLOSE);
    push_envelope(&mut source, PRICE_OPEN, &price, PRICE_CLOSE);
    if let Some(availability) = offer_availability {
        push_envelope(
            &mut source,
            AVAILABILITY_OPEN,
            &availability,
            AVAILABILITY_CLOSE,
        );
    }
    let mut current_section = None;
    for field in fields {
        if current_section.as_deref() != Some(field.section.as_str()) {
            push_envelope(&mut source, SECTION_OPEN, &field.section, SECTION_CLOSE);
            current_section = Some(field.section.clone());
        }
        source.push_str(FIELD_OPEN);
        source.push('\n');
        push_envelope(&mut source, LABEL_OPEN, &field.label, LABEL_CLOSE);
        push_envelope(&mut source, VALUE_OPEN, &field.value, VALUE_CLOSE);
        source.push_str(FIELD_CLOSE);
        source.push('\n');
    }
    if source.len() > MAX_CONTROLLER_SOURCE_BYTES {
        return Err(ListingSourceError::ControllerSourceOverflow {
            actual_bytes: source.len(),
            maximum_bytes: MAX_CONTROLLER_SOURCE_BYTES,
        });
    }
    Ok(ControllerListingSource {
        extraction_text: source,
        evidence_units,
    })
}

fn parse_controller_fields<'document>(
    text: &PublisherTextExtractor<'document>,
    specs: ElementRef<'document>,
) -> Result<Vec<ControllerField>, ListingSourceError> {
    let children = specs.child_elements().collect::<Vec<_>>();
    let mut fields = Vec::new();
    let mut section_count = 0usize;
    let mut index = 0usize;
    let mut reached_trailing_content = false;
    while index < children.len() {
        let child = children[index];
        if has_any_class_token(child, CONTROLLER_TRAILING_CLASSES) {
            reached_trailing_content = true;
            index += 1;
            continue;
        }
        if reached_trailing_content {
            return Err(invalid_structure(
                "specification section appears after trailing service-log or advertising content",
            ));
        }
        if child.value().name() != "h3" || !has_exact_class_token(child, CONTROLLER_HEADING_CLASS) {
            return Err(invalid_structure(
                "expected a specification heading followed by its field wrapper",
            ));
        }
        let Some(wrapper) = children.get(index + 1).copied() else {
            return Err(invalid_structure(
                "specification heading has no field wrapper",
            ));
        };
        if wrapper.value().name() != "div"
            || !has_exact_class_token(wrapper, CONTROLLER_WRAPPER_CLASS)
        {
            return Err(invalid_structure(
                "specification heading is not followed by its exact field wrapper",
            ));
        }

        section_count = section_count.saturating_add(1);
        if section_count > MAX_CONTROLLER_SECTIONS {
            return Err(invalid_structure(format!(
                "more than {MAX_CONTROLLER_SECTIONS} specification sections"
            )));
        }
        let section = bounded_element_text(
            text,
            child,
            "specification heading",
            MAX_CONTROLLER_SECTION_BYTES,
        )?;
        parse_controller_wrapper(text, wrapper, &section, &mut fields)?;
        index += 2;
    }
    if fields.is_empty() {
        return Err(invalid_structure("specifications contain no fields"));
    }
    Ok(fields)
}

fn parse_controller_wrapper<'document>(
    text: &PublisherTextExtractor<'document>,
    wrapper: ElementRef<'document>,
    section: &str,
    fields: &mut Vec<ControllerField>,
) -> Result<(), ListingSourceError> {
    let children = wrapper.child_elements().collect::<Vec<_>>();
    if children.is_empty() || children.len() % 2 != 0 {
        return Err(invalid_structure(
            "specification wrapper must contain label/value pairs",
        ));
    }
    for pair in children.chunks_exact(2) {
        let label_element = pair[0];
        let value_element = pair[1];
        if !has_exact_class_token(label_element, CONTROLLER_LABEL_CLASS)
            || !has_exact_class_token(value_element, CONTROLLER_VALUE_CLASS)
        {
            return Err(invalid_structure(
                "specification wrapper contains a non label/value child sequence",
            ));
        }
        if fields.len() >= MAX_CONTROLLER_FIELDS {
            return Err(invalid_structure(format!(
                "more than {MAX_CONTROLLER_FIELDS} specification fields"
            )));
        }
        let label = bounded_element_text(
            text,
            label_element,
            "specification label",
            MAX_CONTROLLER_LABEL_BYTES,
        )?;
        let raw_label = normalized_label(label_element.text().collect::<String>());
        if raw_label != normalized_label(&label) {
            return Err(invalid_structure(
                "hidden or structural label text changes a specification field identity",
            ));
        }
        let value = bounded_element_text(
            text,
            value_element,
            "specification value",
            MAX_CONTROLLER_VALUE_BYTES,
        )?;
        fields.push(ControllerField {
            section: section.to_string(),
            label,
            value,
        });
    }
    Ok(())
}

fn controller_listing_id(source_url: &str) -> Result<String, ListingSourceError> {
    let url = Url::parse(source_url)
        .map_err(|_| invalid_structure("validated Controller source URL could not be parsed"))?;
    url.path_segments()
        .and_then(|mut segments| segments.nth(2))
        .filter(|listing_id| {
            !listing_id.is_empty() && listing_id.bytes().all(|byte| byte.is_ascii_digit())
        })
        .map(ToString::to_string)
        .ok_or_else(|| invalid_structure("validated Controller source URL lost its listing ID"))
}

/// Extract only the listing-bound Schema.org offer availability value.
///
/// Raw JSON-LD is intentionally excluded from model input. Controller binds
/// the listing Product to the numeric route ID; that exact object may supply
/// one machine-readable Offer availability value without admitting unrelated
/// page, recommendation, or seller objects.
fn controller_offer_availability(
    document: &Html,
    listing_id: &str,
) -> Result<Option<String>, ListingSourceError> {
    let selector =
        Selector::parse(r#"script[type="application/ld+json"]"#).expect("static selector is valid");
    let mut matches = BTreeSet::new();
    for script in document.select(&selector) {
        let json = script.text().collect::<String>();
        let Ok(value) = serde_json::from_str::<Value>(&json) else {
            continue;
        };
        collect_listing_offer_availability(&value, listing_id, false, &mut matches)?;
    }
    if matches.len() > 1 {
        return Err(invalid_structure(
            "listing-bound offer has conflicting availability values",
        ));
    }
    Ok(matches.into_iter().next())
}

fn collect_listing_offer_availability(
    value: &Value,
    listing_id: &str,
    inherited_schema_context: bool,
    matches: &mut BTreeSet<String>,
) -> Result<(), ListingSourceError> {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_listing_offer_availability(
                    value,
                    listing_id,
                    inherited_schema_context,
                    matches,
                )?;
            }
        }
        Value::Object(object) => {
            let schema_context = object_schema_context(object, inherited_schema_context);
            if schema_context
                && object.get("@id").and_then(Value::as_str) == Some(listing_id)
                && object_has_schema_type(object, "Product")
            {
                if let Some(offers) = object.get("offers") {
                    collect_offer_availability_values(offers, schema_context, matches)?;
                }
            }
            for child in object.values() {
                collect_listing_offer_availability(child, listing_id, schema_context, matches)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_offer_availability_values(
    offers: &Value,
    schema_context: bool,
    matches: &mut BTreeSet<String>,
) -> Result<(), ListingSourceError> {
    match offers {
        Value::Array(offers) => {
            for offer in offers {
                collect_offer_availability_values(offer, schema_context, matches)?;
            }
        }
        Value::Object(offer) => {
            let schema_context = object_schema_context(offer, schema_context);
            if !schema_context || !object_has_schema_type(offer, "Offer") {
                return Err(invalid_structure(
                    "listing-bound Product offers must have Schema.org Offer type",
                ));
            }
            let Some(value) = offer.get("availability") else {
                return Ok(());
            };
            let availability = value
                .as_str()
                .and_then(schema_availability_name)
                .ok_or_else(|| invalid_structure("listing-bound offer availability is invalid"))?;
            matches.insert(availability.to_string());
        }
        _ => {
            return Err(invalid_structure(
                "listing-bound offers must be an object or array",
            ));
        }
    }
    Ok(())
}

fn object_schema_context(
    object: &serde_json::Map<String, Value>,
    inherited_schema_context: bool,
) -> bool {
    object
        .get("@context")
        .map_or(inherited_schema_context, |context| {
            json_ld_context_is_unambiguous_schema_org(context)
        })
}

fn json_ld_context_is_unambiguous_schema_org(value: &Value) -> bool {
    match value {
        Value::String(value) => schema_org_name(value).is_some_and(|name| name.is_empty()),
        Value::Array(values) => {
            values.len() == 1
                && values
                    .first()
                    .is_some_and(json_ld_context_is_unambiguous_schema_org)
        }
        Value::Object(context) => {
            context.len() == 1
                && context
                    .get("@vocab")
                    .and_then(Value::as_str)
                    .and_then(schema_org_name)
                    .is_some_and(|name| name.is_empty())
        }
        _ => false,
    }
}

fn object_has_schema_type(object: &serde_json::Map<String, Value>, expected: &str) -> bool {
    object
        .get("@type")
        .is_some_and(|value| json_ld_type_includes(value, expected))
}

fn json_ld_type_includes(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(value) => schema_org_name(value).unwrap_or(value) == expected,
        Value::Array(values) => values
            .iter()
            .any(|value| json_ld_type_includes(value, expected)),
        _ => false,
    }
}

fn schema_org_name(value: &str) -> Option<&str> {
    value
        .strip_prefix("https://schema.org/")
        .or_else(|| value.strip_prefix("http://schema.org/"))
        .or_else(|| (value == "https://schema.org" || value == "http://schema.org").then_some(""))
}

fn schema_availability_name(value: &str) -> Option<&str> {
    let value = schema_org_name(value).unwrap_or(value);
    matches!(
        value,
        "InStock"
            | "OutOfStock"
            | "SoldOut"
            | "PreOrder"
            | "PreSale"
            | "LimitedAvailability"
            | "OnlineOnly"
            | "InStoreOnly"
            | "BackOrder"
            | "Discontinued"
    )
    .then_some(value)
}

fn unique_element<'document>(
    scope: ElementRef<'document>,
    selector: &str,
    exact_class: &str,
    role: &str,
) -> Result<ElementRef<'document>, ListingSourceError> {
    let selector = Selector::parse(selector).expect("static Controller selector is valid");
    let mut matches = scope
        .select(&selector)
        .filter(|element| has_exact_class_token(*element, exact_class));
    let element = matches
        .next()
        .ok_or_else(|| invalid_structure(format!("missing {role}")))?;
    if matches.next().is_some() {
        return Err(invalid_structure(format!("ambiguous {role}")));
    }
    Ok(element)
}

fn bounded_element_text<'document>(
    text: &PublisherTextExtractor<'document>,
    element: ElementRef<'document>,
    role: &str,
    maximum_bytes: usize,
) -> Result<String, ListingSourceError> {
    let value = text.multiline_element_text(element);
    if value.is_empty() {
        return Err(invalid_structure(format!("empty {role}")));
    }
    if value.len() > maximum_bytes {
        return Err(invalid_structure(format!(
            "{role} is {} bytes; maximum is {maximum_bytes}",
            value.len()
        )));
    }
    if RESERVED_MARKERS.iter().any(|marker| value.contains(marker)) {
        return Err(invalid_structure(format!(
            "{role} contains a reserved source boundary marker"
        )));
    }
    Ok(value)
}

fn push_envelope(output: &mut String, open: &str, value: &str, close: &str) {
    output.push_str(open);
    output.push('\n');
    output.push_str(value);
    output.push('\n');
    output.push_str(close);
    output.push('\n');
}

fn normalized_label(value: impl AsRef<str>) -> String {
    value
        .as_ref()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_exact_class_token(element: ElementRef<'_>, expected: &str) -> bool {
    element.attr("class").is_some_and(|classes| {
        classes
            .split_ascii_whitespace()
            .any(|class| class == expected)
    })
}

fn has_any_class_token(element: ElementRef<'_>, expected: &[&str]) -> bool {
    expected
        .iter()
        .any(|class| has_exact_class_token(element, class))
}

fn invalid_structure(error: impl Into<String>) -> ListingSourceError {
    ListingSourceError::InvalidControllerStructure(error.into())
}

#[cfg(test)]
mod tests {
    use super::{
        controller_extraction_source_has_exact_avionics_line, listing_evidence_units,
        listing_extraction_source, listing_extraction_source_contains_exact_avionics_occurrence,
        MAX_CONTROLLER_VALUE_BYTES,
    };

    const URL: &str = "https://www.controller.com/listing/for-sale/257959105/example";
    const FROZEN_CORPUS_ID_HTML_FINGERPRINT: &str =
        "f97628f16b57844ad3ac6858fe9d799ed4208de6594024079b86e73dc3c98a7f";
    const REALISTIC: &str =
        include_str!("../../../tests/fixtures/controller/id25_like_listing.html");

    #[test]
    fn controller_source_preserves_fields_without_cross_field_or_page_noise() {
        let source = listing_extraction_source(URL, REALISTIC).unwrap();

        assert!(source.contains("[CONTROLLER TITLE]\n2022 CESSNA 182T SKYLANE"));
        assert!(source.contains("[CONTROLLER PRIMARY ASKING PRICE]\n$669,500"));
        assert!(source.contains(
            "[CONTROLLER LISTING OFFER AVAILABILITY]\nInStock\n[/CONTROLLER LISTING OFFER AVAILABILITY]"
        ));
        assert!(source.contains(
            "[CONTROLLER FIELD]\n[LABEL]\nCondition\n[/LABEL]\n[VALUE]\nUsed\n[/VALUE]\n[/CONTROLLER FIELD]"
        ));
        assert!(!source.contains("PRIMARY STATUS FIELD"));
        assert!(source.contains(
            "[LABEL]\nFlight Deck Manufacturer/Model\n[/LABEL]\n[VALUE]\nGARMIN G1000 NXI\n[/VALUE]"
        ));
        assert!(source.contains("[LABEL]\nSVT\n[/LABEL]\n[VALUE]\nYes\n[/VALUE]"));
        assert!(!source.contains("GARMIN G1000 NXI\nSVT\nYes"));
        assert!(!source.contains("GARMIN G1000 NXI SVT"));

        assert!(source.contains(
            "Includes all the standard equipment which included the NXi G1000 with dual coms and navs and GFC 700 autopilot plus synthetic vision.\nExcellent condition!"
        ));
        assert!(source.contains("GARMIN GIA-63W #1\nGARMIN GIA-63W #2\nBENDIX/KING KAP 140"));
        assert!(!source.contains("JSON-LD DECOY"));
        assert!(!source.contains("$123,456"));
        assert!(!source.contains("$6,206.35"));
        assert!(!source.contains("RELATED AIRCRAFT $99,999"));
        assert!(!source.contains("Browse Aircraft"));
    }

    #[test]
    fn controller_evidence_uses_exact_values_from_the_structured_source_adapter() {
        let units = listing_evidence_units(URL, REALISTIC).unwrap();

        assert!(units.contains_exact_span("GARMIN G1000 NXI"));
        assert!(units.contains_exact_span("GFC 700 autopilot"));
        assert!(!units.contains_exact_span("Flight Deck Manufacturer/Model GARMIN G1000 NXI"));
        assert!(!units.contains_exact_span("GARMIN G1000 NXI SVT"));
        assert!(!units.contains_exact_span("SVT Yes"));
    }

    #[test]
    fn checkpoint_occurrence_rebinding_stays_inside_the_controller_avionics_field() {
        let source = listing_extraction_source(URL, REALISTIC).unwrap();

        assert!(
            listing_extraction_source_contains_exact_avionics_occurrence(
                URL,
                &source,
                "GARMIN GIA-63W #1"
            )
        );
        assert!(controller_extraction_source_has_exact_avionics_line(
            URL,
            &source,
            "GARMIN GIA-63W #1"
        ));
        assert!(
            !listing_extraction_source_contains_exact_avionics_occurrence(URL, &source, "GIA-63")
        );
        assert!(
            !listing_extraction_source_contains_exact_avionics_occurrence(
                URL,
                &source,
                "GARMIN G1000 NXI"
            )
        );
        assert!(!controller_extraction_source_has_exact_avionics_line(
            URL,
            &source,
            "GARMIN GIA-63W"
        ));
    }

    #[test]
    fn controller_occurrence_rebinding_rejects_ambiguous_avionics_envelopes() {
        let source = listing_extraction_source(URL, REALISTIC).unwrap();
        let avionics_field = source
            .split("[CONTROLLER FIELD]\n[LABEL]\nAvionics/Radios")
            .nth(1)
            .and_then(|tail| tail.split_once("[/CONTROLLER FIELD]\n"))
            .map(|(field, _)| {
                format!("[CONTROLLER FIELD]\n[LABEL]\nAvionics/Radios{field}[/CONTROLLER FIELD]\n")
            })
            .expect("fixture should contain the Controller avionics field");
        let ambiguous = format!("{source}{avionics_field}");

        assert!(
            !listing_extraction_source_contains_exact_avionics_occurrence(
                URL,
                &ambiguous,
                "GARMIN GIA-63W #1"
            )
        );
        assert!(!controller_extraction_source_has_exact_avionics_line(
            URL,
            &ambiguous,
            "GARMIN GIA-63W #1"
        ));
    }

    #[test]
    fn generic_checkpoint_occurrence_cannot_cross_adapter_units() {
        let source = "Garmin GIA 63W\nNAV/COM/GPS\nUnrelated aircraft field";

        assert!(
            listing_extraction_source_contains_exact_avionics_occurrence(
                "https://example.com/listing",
                source,
                "Garmin GIA 63W"
            )
        );
        assert!(
            !listing_extraction_source_contains_exact_avionics_occurrence(
                "https://example.com/listing",
                source,
                "GIA 63"
            )
        );
        assert!(
            !listing_extraction_source_contains_exact_avionics_occurrence(
                "https://example.com/listing",
                source,
                "GIA 63W NAV/COM/GPS"
            )
        );
    }

    #[test]
    fn controller_aircraft_condition_never_becomes_sale_lifecycle() {
        let without_availability = REALISTIC.replace(
            "\"offers\":{\"@type\":\"Offer\",\"availability\":\"InStock\"}",
            "\"offers\":{\"@type\":\"Offer\"}",
        );
        let source = listing_extraction_source(URL, &without_availability).unwrap();
        assert!(!source.contains("LISTING OFFER AVAILABILITY"));
        assert!(source.contains("[LABEL]\nCondition\n[/LABEL]\n[VALUE]\nUsed\n[/VALUE]"));
        assert!(!source.contains("PRIMARY STATUS FIELD"));

        let conflicting = REALISTIC.replace(
            "\"offers\":{\"@type\":\"Offer\",\"availability\":\"InStock\"}",
            "\"offers\":[{\"@type\":\"Offer\",\"availability\":\"InStock\"},{\"@type\":\"Offer\",\"availability\":\"OutOfStock\"}]",
        );
        let error = listing_extraction_source(URL, &conflicting).unwrap_err();
        assert!(error.to_string().contains("conflicting availability"));

        let same_id_non_product = REALISTIC.replace(
            "\"@type\":\"Product\",\"@id\":\"257959105\"",
            "\"@type\":\"WebPage\",\"@id\":\"257959105\"",
        );
        let source = listing_extraction_source(URL, &same_id_non_product).unwrap();
        assert!(!source.contains("LISTING OFFER AVAILABILITY"));

        let reset_product_context = REALISTIC.replace(
            "\"mainEntity\":{\"@type\":\"Product\"",
            "\"mainEntity\":{\"@context\":null,\"@type\":\"Product\"",
        );
        let source = listing_extraction_source(URL, &reset_product_context).unwrap();
        assert!(!source.contains("LISTING OFFER AVAILABILITY"));

        let ambiguous_root_context = REALISTIC.replace(
            "\"@context\":\"https://schema.org\"",
            "\"@context\":[\"https://schema.org\",null]",
        );
        let source = listing_extraction_source(URL, &ambiguous_root_context).unwrap();
        assert!(!source.contains("LISTING OFFER AVAILABILITY"));

        let non_offer = REALISTIC.replace(
            "\"@type\":\"Offer\",\"availability\":\"InStock\"",
            "\"@type\":\"AggregateOffer\",\"availability\":\"InStock\"",
        );
        let error = listing_extraction_source(URL, &non_offer).unwrap_err();
        assert!(error
            .to_string()
            .contains("must have Schema.org Offer type"));

        let reset_offer_context = REALISTIC.replace(
            "\"offers\":{\"@type\":\"Offer\"",
            "\"offers\":{\"@context\":null,\"@type\":\"Offer\"",
        );
        let error = listing_extraction_source(URL, &reset_offer_context).unwrap_err();
        assert!(error
            .to_string()
            .contains("must have Schema.org Offer type"));
    }

    #[test]
    fn recognized_controller_source_never_uses_generic_fallback() {
        let error = listing_extraction_source(
            "https://www.controller.com/not-a-listing",
            "<main><p>generic text that the broad scraper could retain</p></main>",
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("invalid Controller listing source URL"));

        let error =
            listing_extraction_source(URL, "<main><p>incomplete capture</p></main>").unwrap_err();
        assert!(error.to_string().contains("missing listing main"));
    }

    #[test]
    fn controller_source_rejects_ambiguous_structure_and_critical_overflow() {
        let duplicated = REALISTIC.replace("</body>", &format!("{REALISTIC}</body>"));
        let error = listing_extraction_source(URL, &duplicated).unwrap_err();
        assert!(error.to_string().contains("ambiguous listing main"));

        let oversized = "X".repeat(MAX_CONTROLLER_VALUE_BYTES + 1);
        let html = REALISTIC.replace("Fully equipped 2022 Cessna 182T", &oversized);
        let error = listing_extraction_source(URL, &html).unwrap_err();
        assert!(error.to_string().contains("specification value is"));
        assert!(error.to_string().contains("maximum is 16000"));

        let fields = (0..180)
            .map(|index| {
                format!(
                    "<div class=\"detail__specs-label\">Extra {index}</div><div class=\"detail__specs-value\">{}</div>",
                    "Y".repeat(100)
                )
            })
            .collect::<String>();
        let html = REALISTIC.replace(
            "<div class=\"detail__specs-service-logs\">",
            &format!(
                "<h3 class=\"detail__specs-heading\">Overflow</h3><div class=\"detail__specs-wrapper\">{fields}</div><div class=\"detail__specs-service-logs\">"
            ),
        );
        let error = listing_extraction_source(URL, &html).unwrap_err();
        assert!(error.to_string().contains("Controller listing source is"));
        assert!(error.to_string().contains("maximum is 24000"));
    }

    #[test]
    fn non_controller_source_keeps_generic_cleaning() {
        let source = listing_extraction_source(
            "https://example.test/listing/one",
            "<html><body><main><h1>1999 Example 100</h1></main></body></html>",
        )
        .unwrap();
        assert!(source.contains("1999 Example 100"));
    }

    /// Read-only contract audit for an explicitly supplied retained-capture DB.
    ///
    /// Run with:
    /// `AIRCOST_CONTROLLER_CORPUS_DATABASE=/path/to/prepared.sqlite3 cargo test controller_real_corpus_contract_audit -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "requires an explicitly supplied retained Controller capture database"]
    async fn controller_real_corpus_contract_audit() {
        use scraper::{Html, Selector};
        use sha2::{Digest, Sha256};
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        let path = std::env::var("AIRCOST_CONTROLLER_CORPUS_DATABASE")
            .expect("AIRCOST_CONTROLLER_CORPUS_DATABASE must name the read-only audit database");
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))
            .expect("invalid SQLite corpus path")
            .create_if_missing(false)
            .read_only(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("could not open the corpus database read-only");
        let rows = sqlx::query_as::<_, (i64, String, String, String)>(
            "SELECT id, source_url, rendered_html, rendered_html_sha256 FROM plugin_submissions ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .expect("could not read retained plugin captures");
        assert_eq!(rows.len(), 70, "the frozen corpus must contain 70 captures");
        let mut fingerprint_input = String::new();
        for (submission_id, _, rendered_html, rendered_html_sha256) in &rows {
            assert_eq!(
                format!("{:x}", Sha256::digest(rendered_html.as_bytes())),
                *rendered_html_sha256,
                "submission {submission_id} rendered HTML digest is stale"
            );
            fingerprint_input.push_str(&format!("{submission_id}:{rendered_html_sha256}\n"));
        }
        assert_eq!(
            format!("{:x}", Sha256::digest(fingerprint_input.as_bytes())),
            FROZEN_CORPUS_ID_HTML_FINGERPRINT,
            "the audit database is not the exact frozen capture-ID/HTML corpus"
        );

        let noise_selector = Selector::parse(
            ".payments-as-low-as-route, .detail__additional-listings, .similar-listings, .recently-viewed",
        )
        .unwrap();
        let mut character_counts = Vec::with_capacity(rows.len());
        let mut finance_or_related_price_tokens = 0usize;
        let mut active_offer_availabilities = 0usize;
        for (submission_id, source_url, html, _) in rows {
            let source = listing_extraction_source(&source_url, &html).unwrap_or_else(|error| {
                panic!("submission {submission_id} failed the Controller contract: {error}")
            });
            character_counts.push(source.chars().count());
            if source.contains(
                "[CONTROLLER LISTING OFFER AVAILABILITY]\nInStock\n[/CONTROLLER LISTING OFFER AVAILABILITY]",
            ) {
                active_offer_availabilities += 1;
            }

            let document = Html::parse_document(&html);
            for noise in document.select(&noise_selector) {
                let text = noise.text().collect::<Vec<_>>().join(" ");
                for token in text.split_whitespace().filter(|token| token.contains('$')) {
                    let price = token.trim_matches(|character: char| {
                        !character.is_ascii_digit() && !matches!(character, '$' | ',' | '.')
                    });
                    if price.starts_with('$')
                        && price.chars().any(|character| character.is_ascii_digit())
                    {
                        finance_or_related_price_tokens += 1;
                        assert!(
                            !source.contains(price),
                            "submission {submission_id} retained unrelated price {price}"
                        );
                    }
                }
            }
        }
        assert_eq!(
            active_offer_availabilities, 70,
            "every frozen retained listing must expose its exact listing-bound InStock offer"
        );
        character_counts.sort_unstable();
        eprintln!(
            "Controller corpus: captures={}, source_chars_min={}, source_chars_median={}, source_chars_max={}, active_offer_availabilities={}, excluded_finance_or_related_prices={}",
            character_counts.len(),
            character_counts[0],
            character_counts[character_counts.len() / 2],
            character_counts[character_counts.len() - 1],
            active_offer_availabilities,
            finance_or_related_price_tokens,
        );
        pool.close().await;
    }
}
