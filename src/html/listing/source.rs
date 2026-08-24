//! Publisher-aware listing text supplied to extraction models.
//!
//! Controller pages expose the authoritative listing facts as one stable
//! title/price/specification structure. Extracting that structure directly
//! avoids flattening unrelated page chrome and, importantly, keeps every
//! specification label and value in a separate source unit.

use std::fmt;

use scraper::{ElementRef, Html, Selector};

use crate::html::clean::{clean_listing_html, PublisherTextExtractor};
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
const CONTROLLER_STATUS_LABEL: &str = "Condition";
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
const SECTION_OPEN: &str = "[CONTROLLER SPEC SECTION]";
const SECTION_CLOSE: &str = "[/CONTROLLER SPEC SECTION]";
const FIELD_OPEN: &str = "[CONTROLLER FIELD]";
const STATUS_FIELD_OPEN: &str = "[CONTROLLER PRIMARY STATUS FIELD]";
const FIELD_CLOSE: &str = "[/CONTROLLER FIELD]";
const STATUS_FIELD_CLOSE: &str = "[/CONTROLLER PRIMARY STATUS FIELD]";
const LABEL_OPEN: &str = "[LABEL]";
const LABEL_CLOSE: &str = "[/LABEL]";
const VALUE_OPEN: &str = "[VALUE]";
const VALUE_CLOSE: &str = "[/VALUE]";
const RESERVED_MARKERS: &[&str] = &[
    TITLE_OPEN,
    TITLE_CLOSE,
    PRICE_OPEN,
    PRICE_CLOSE,
    SECTION_OPEN,
    SECTION_CLOSE,
    FIELD_OPEN,
    STATUS_FIELD_OPEN,
    FIELD_CLOSE,
    STATUS_FIELD_CLOSE,
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
    primary_status: bool,
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
        controller_listing_source(source_url, retained_html)
    } else {
        Ok(clean_listing_html(retained_html))
    }
}

fn controller_listing_source(
    source_url: &str,
    retained_html: &str,
) -> Result<String, ListingSourceError> {
    validate_controller_listing_source_url(source_url)
        .map_err(|error| ListingSourceError::InvalidControllerSourceUrl(error.to_string()))?;
    if retained_html.len() > MAX_RETAINED_HTML_BYTES {
        return Err(ListingSourceError::RetainedHtmlTooLarge {
            actual_bytes: retained_html.len(),
            maximum_bytes: MAX_RETAINED_HTML_BYTES,
        });
    }

    let document = Html::parse_document(retained_html);
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
    if fields.iter().filter(|field| field.primary_status).count() != 1 {
        return Err(invalid_structure(
            "expected exactly one visible Condition field for primary status",
        ));
    }

    let mut source = String::new();
    push_envelope(&mut source, TITLE_OPEN, &title, TITLE_CLOSE);
    push_envelope(&mut source, PRICE_OPEN, &price, PRICE_CLOSE);
    let mut current_section = None;
    for field in fields {
        if current_section.as_deref() != Some(field.section.as_str()) {
            push_envelope(&mut source, SECTION_OPEN, &field.section, SECTION_CLOSE);
            current_section = Some(field.section.clone());
        }
        let (field_open, field_close) = if field.primary_status {
            (STATUS_FIELD_OPEN, STATUS_FIELD_CLOSE)
        } else {
            (FIELD_OPEN, FIELD_CLOSE)
        };
        source.push_str(field_open);
        source.push('\n');
        push_envelope(&mut source, LABEL_OPEN, &field.label, LABEL_CLOSE);
        push_envelope(&mut source, VALUE_OPEN, &field.value, VALUE_CLOSE);
        source.push_str(field_close);
        source.push('\n');
    }
    if source.len() > MAX_CONTROLLER_SOURCE_BYTES {
        return Err(ListingSourceError::ControllerSourceOverflow {
            actual_bytes: source.len(),
            maximum_bytes: MAX_CONTROLLER_SOURCE_BYTES,
        });
    }
    Ok(source)
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
            primary_status: normalized_label(&label) == CONTROLLER_STATUS_LABEL,
            label,
            value,
        });
    }
    Ok(())
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
    use super::{listing_extraction_source, MAX_CONTROLLER_VALUE_BYTES};

    const URL: &str = "https://www.controller.com/listing/for-sale/257959105/example";
    const REALISTIC: &str =
        include_str!("../../../tests/fixtures/controller/id25_like_listing.html");

    #[test]
    fn controller_source_preserves_fields_without_cross_field_or_page_noise() {
        let source = listing_extraction_source(URL, REALISTIC).unwrap();

        assert!(source.contains("[CONTROLLER TITLE]\n2022 CESSNA 182T SKYLANE"));
        assert!(source.contains("[CONTROLLER PRIMARY ASKING PRICE]\n$669,500"));
        assert!(source.contains(
            "[CONTROLLER PRIMARY STATUS FIELD]\n[LABEL]\nCondition\n[/LABEL]\n[VALUE]\nUsed\n[/VALUE]"
        ));
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
        let rows = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, source_url, rendered_html FROM plugin_submissions ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .expect("could not read retained plugin captures");
        assert_eq!(rows.len(), 70, "the frozen corpus must contain 70 captures");

        let noise_selector = Selector::parse(
            ".payments-as-low-as-route, .detail__additional-listings, .similar-listings, .recently-viewed",
        )
        .unwrap();
        let mut character_counts = Vec::with_capacity(rows.len());
        let mut finance_or_related_price_tokens = 0usize;
        for (submission_id, source_url, html) in rows {
            let source = listing_extraction_source(&source_url, &html).unwrap_or_else(|error| {
                panic!("submission {submission_id} failed the Controller contract: {error}")
            });
            character_counts.push(source.chars().count());

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
        character_counts.sort_unstable();
        eprintln!(
            "Controller corpus: captures={}, source_chars_min={}, source_chars_median={}, source_chars_max={}, excluded_finance_or_related_prices={}",
            character_counts.len(),
            character_counts[0],
            character_counts[character_counts.len() / 2],
            character_counts[character_counts.len() - 1],
            finance_or_related_price_tokens,
        );
        pool.close().await;
    }
}
