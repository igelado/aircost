use html_escape::decode_html_entities;
use scraper::{Html, Selector};

const DEFAULT_MAX_LISTING_TEXT_CHARACTERS: usize = 24_000;

pub fn clean_listing_html(html: &str) -> String {
    clean_listing_html_with_limit(html, DEFAULT_MAX_LISTING_TEXT_CHARACTERS)
}

pub fn clean_listing_html_with_limit(html: &str, max_characters: usize) -> String {
    let document = Html::parse_document(html);
    let mut candidates = Vec::new();

    candidates.extend(selector_text(&document, "title"));
    candidates.extend(meta_values(&document));
    candidates.extend(json_ld_values(&document));
    candidates.extend(selector_text(
        &document,
        "main, article, h1, h2, h3, p, li, dt, dd, th, td, span, div",
    ));

    let mut lines = Vec::new();
    let mut previous = String::new();
    for candidate in candidates {
        for line in candidate.lines() {
            let cleaned = normalize_page_text(line);
            if !cleaned.is_empty() && cleaned != previous {
                previous = cleaned.clone();
                lines.push(cleaned);
            }
        }
    }

    trim_listing_text(&lines.join("\n"), max_characters)
}

/// Extract publisher-authored page text for source-proof verification.
///
/// Unlike listing cleanup, this preserves the complete document and never
/// moves or truncates the result around an aircraft-listing anchor. Script,
/// style, template, and other non-visible text is excluded so it cannot be
/// mistaken for publisher evidence.
pub fn clean_publisher_source_html(html: &str) -> String {
    let document = Html::parse_document(html);
    let mut text = String::new();
    for node in document.tree.root().descendants() {
        let Some(node_text) = node.value().as_text() else {
            continue;
        };
        if node.ancestors().any(|ancestor| {
            ancestor.value().as_element().is_some_and(|element| {
                matches!(
                    element.name(),
                    "script" | "style" | "noscript" | "template" | "svg" | "canvas" | "iframe"
                ) || element.attr("hidden").is_some()
                    || element
                        .attr("aria-hidden")
                        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
                    || element
                        .attr("style")
                        .is_some_and(inline_style_hides_element)
            })
        }) {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(node_text);
    }
    normalize_page_text(&text)
}

fn inline_style_hides_element(style: &str) -> bool {
    style.split(';').any(|declaration| {
        let Some((property, value)) = declaration.split_once(':') else {
            return false;
        };
        let property = property.trim();
        (property.eq_ignore_ascii_case("display") && css_value_is(value, "none"))
            || (property.eq_ignore_ascii_case("visibility") && css_value_is(value, "hidden"))
    })
}

fn css_value_is(value: &str, expected: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized
        .strip_suffix("!important")
        .unwrap_or(&normalized)
        .trim()
        .eq_ignore_ascii_case(expected)
}

/// Canonicalize evidence for exact, token-bounded source-page comparison.
///
/// Punctuation and markup boundaries are not identity-bearing. Alphanumeric
/// token contents are: in particular, `182` remains different from `182T`.
pub fn normalize_source_evidence_span(value: &str) -> String {
    decode_html_entities(value)
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Return whether `evidence` occurs as one contiguous, token-bounded span in
/// already-extracted publisher text.
pub fn publisher_text_contains_evidence_span(publisher_text: &str, evidence: &str) -> bool {
    let document = normalize_source_evidence_span(publisher_text);
    let evidence = normalize_source_evidence_span(evidence);
    if evidence.is_empty() {
        return false;
    }
    document.match_indices(&evidence).any(|(start, matched)| {
        let end = start + matched.len();
        (start == 0 || document.as_bytes()[start - 1] == b' ')
            && (end == document.len() || document.as_bytes()[end] == b' ')
    })
}

fn selector_text(document: &Html, selector: &str) -> Vec<String> {
    let selector = Selector::parse(selector).unwrap();
    document
        .select(&selector)
        .map(|element| element.text().collect::<Vec<_>>().join(" "))
        .collect()
}

fn meta_values(document: &Html) -> Vec<String> {
    let selector = Selector::parse("meta").unwrap();
    let mut values = Vec::new();
    for element in document.select(&selector) {
        let name = element
            .attr("name")
            .or_else(|| element.attr("property"))
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(
            name.as_str(),
            "description" | "title" | "og:title" | "og:description"
        ) {
            continue;
        }
        if let Some(content) = element.attr("content") {
            values.push(content.to_string());
        }
    }
    values
}

fn json_ld_values(document: &Html) -> Vec<String> {
    let selector = Selector::parse(r#"script[type="application/ld+json"]"#).unwrap();
    document
        .select(&selector)
        .map(|element| element.text().collect::<Vec<_>>().join(" "))
        .collect()
}

fn normalize_page_text(value: &str) -> String {
    decode_html_entities(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn trim_listing_text(text: &str, max_characters: usize) -> String {
    if text.len() <= max_characters {
        return text.to_string();
    }

    let start = listing_anchor(text)
        .map(|anchor| anchor.saturating_sub(1000))
        .unwrap_or(0);
    let start = nearest_char_boundary(text, start);
    let end = nearest_char_boundary(text, (start + max_characters).min(text.len()));
    text[start..end].to_string()
}

fn listing_anchor(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    [
        "cirrus", "cessna", "sling", "sr20", "sr22", "sr22t", "t182t",
    ]
    .iter()
    .filter_map(|keyword| lower.find(keyword))
    .min()
}

fn nearest_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::{
        clean_listing_html, clean_publisher_source_html, normalize_source_evidence_span,
        publisher_text_contains_evidence_span,
    };

    #[test]
    fn cleans_listing_html_to_text() {
        let html = r#"
        <html>
          <head>
            <title>2022 CIRRUS SR22T G6 SN: 8922 for Sale</title>
            <meta name="description" content="2022 Cirrus SR22T G6 aircraft listing.">
            <script>window.analytics = {"engine": 22};</script>
          </head>
          <body>
            <h1>2022 Cirrus SR22T G6 SN: 8922 for Sale</h1>
            <p>Registration No:</p><p>N317JT</p>
            <p>TTSN: 771</p>
            <p>Garmin GFC-700 Digital Autopilot</p>
          </body>
        </html>
        "#;

        let text = clean_listing_html(html);

        assert!(text.contains("2022 CIRRUS SR22T G6 SN: 8922"));
        assert!(text.contains("Registration No:"));
        assert!(text.contains("TTSN: 771"));
        assert!(text.contains("Garmin GFC-700 Digital Autopilot"));
        assert!(!text.contains("window.analytics"));
        assert!(!text.contains("<h1>"));
    }

    #[test]
    fn publisher_source_text_is_complete_and_accepts_conames_across_markup_and_entities() {
        let long_prefix = "unrelated ".repeat(30_000);
        let html = format!(
            r#"<html><body>{long_prefix}<p>Cessna <strong>Skylane</strong>&nbsp;(182)</p>
            <script>Cessna 209740 fabricated evidence</script></body></html>"#
        );

        let text = clean_publisher_source_html(&html);

        assert!(text.starts_with("unrelated unrelated"));
        assert!(text.contains("Cessna Skylane (182)"));
        assert!(!text.contains("209740"));
        assert!(publisher_text_contains_evidence_span(
            &text,
            "Cessna Skylane 182"
        ));
    }

    #[test]
    fn source_span_comparison_is_token_bounded() {
        assert_eq!(
            normalize_source_evidence_span("Cessna\u{a0}Skylane (182)"),
            "cessna skylane 182"
        );
        assert!(publisher_text_contains_evidence_span(
            "The Cessna Skylane 182 is listed.",
            "Skylane 182"
        ));
        assert!(!publisher_text_contains_evidence_span(
            "The FAA designation is 182T.",
            "182"
        ));
        assert!(!publisher_text_contains_evidence_span(
            "A different 209741 designation.",
            "209740"
        ));
    }

    #[test]
    fn publisher_source_text_separates_dom_text_nodes_without_fabricating_tokens() {
        let text = clean_publisher_source_html(
            "<html><body><span>Cessna</span><span>182</span>\
             <p>designation <i>18</i><b>2</b></p></body></html>",
        );

        assert!(publisher_text_contains_evidence_span(&text, "Cessna 182"));
        assert!(!publisher_text_contains_evidence_span(
            &text,
            "designation 182"
        ));
    }

    #[test]
    fn publisher_source_text_excludes_hidden_subtrees_without_style_substring_false_positives() {
        let text = clean_publisher_source_html(
            r#"
            <html><body>
              <p>Visible Cessna Skylane 182 evidence</p>
              <div hidden>Hidden fabricated family identity</div>
              <div aria-hidden=" TRUE ">ARIA-hidden fabricated family identity</div>
              <div style=" DISPLAY : NONE!important ">Display-hidden fabricated family identity</div>
              <div style=" visibility : HIDDEN ">Visibility-hidden fabricated family identity</div>
              <div style="content: 'display:none'; display: block">
                Visible content-property text
              </div>
              <div style="--display:none; display: none-block">
                Visible substring-collision text
              </div>
            </body></html>
            "#,
        );

        assert!(text.contains("Visible Cessna Skylane 182 evidence"));
        assert!(text.contains("Visible content-property text"));
        assert!(text.contains("Visible substring-collision text"));
        assert!(!text.contains("Hidden fabricated"));
        assert!(!text.contains("ARIA-hidden fabricated"));
        assert!(!text.contains("Display-hidden fabricated"));
        assert!(!text.contains("Visibility-hidden fabricated"));
        assert!(publisher_text_contains_evidence_span(
            &text,
            "Cessna Skylane 182"
        ));
        assert!(!publisher_text_contains_evidence_span(
            &text,
            "fabricated family identity"
        ));
    }
}
