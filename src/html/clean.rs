use ego_tree::{NodeId, NodeRef};
use html_escape::decode_html_entities;
use scraper::{node::Element, ElementRef, Html, Node, Selector};
use std::collections::HashSet;
use std::marker::PhantomData;

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
/// mistaken for publisher evidence. Visible DOM line and block boundaries are
/// retained; source-formatting whitespace is collapsed except inside `pre`.
pub fn clean_publisher_source_html(html: &str) -> String {
    let document = Html::parse_document(html);
    clean_publisher_nodes(
        &document,
        document.tree.root().descendants(),
        |node| {
            node.ancestors().any(|ancestor| {
                ancestor
                    .value()
                    .as_element()
                    .is_some_and(|element| element.name() == "pre")
            })
        },
        publisher_element_is_hidden,
    )
}

/// Extract visible text from one already-validated publisher element while
/// preserving its author-entered line boundaries.
pub(crate) fn clean_publisher_multiline_element_text<'document>(
    document: &'document Html,
    element: ElementRef<'document>,
) -> String {
    PublisherTextExtractor::new(document).multiline_element_text(element)
}

/// Reusable visible-text extractor for several elements in one parsed page.
///
/// Publisher-specific structure readers should create this once so embedded
/// stylesheet visibility rules are indexed once for the complete document.
pub(crate) struct PublisherTextExtractor<'document> {
    stylesheet_hidden_nodes: HashSet<NodeId>,
    document: PhantomData<&'document Html>,
}

impl<'document> PublisherTextExtractor<'document> {
    pub(crate) fn new(document: &'document Html) -> Self {
        Self {
            stylesheet_hidden_nodes: stylesheet_hidden_node_ids(document),
            document: PhantomData,
        }
    }

    pub(crate) fn multiline_element_text(&self, element: ElementRef<'document>) -> String {
        clean_publisher_nodes_with_hidden(
            element.descendants(),
            |_| true,
            structurally_hidden_element,
            &self.stylesheet_hidden_nodes,
        )
    }
}

fn clean_publisher_nodes<'a>(
    document: &'a Html,
    nodes: impl Iterator<Item = NodeRef<'a, Node>>,
    preserves_line_breaks: impl Fn(NodeRef<'a, Node>) -> bool,
    element_is_hidden: fn(&Element) -> bool,
) -> String {
    let stylesheet_hidden_nodes = stylesheet_hidden_node_ids(document);
    clean_publisher_nodes_with_hidden(
        nodes,
        preserves_line_breaks,
        element_is_hidden,
        &stylesheet_hidden_nodes,
    )
}

fn clean_publisher_nodes_with_hidden<'a>(
    nodes: impl Iterator<Item = NodeRef<'a, Node>>,
    preserves_line_breaks: impl Fn(NodeRef<'a, Node>) -> bool,
    element_is_hidden: fn(&Element) -> bool,
    stylesheet_hidden_nodes: &HashSet<NodeId>,
) -> String {
    let mut text = String::new();
    let mut previous_line_container = None;
    let mut pending_line_break = false;
    for node in nodes {
        if publisher_node_is_hidden(node, stylesheet_hidden_nodes, element_is_hidden) {
            continue;
        }
        if node
            .value()
            .as_element()
            .is_some_and(|element| element.name() == "br")
        {
            pending_line_break = true;
            continue;
        }
        let Some(node_text) = node.value().as_text() else {
            continue;
        };
        let preserves_line_breaks = preserves_line_breaks(node);
        let (node_text, starts_with_line_break, ends_with_line_break) =
            normalize_publisher_text_fragment(node_text, preserves_line_breaks);
        if node_text.is_empty() {
            pending_line_break |=
                preserves_line_breaks && (starts_with_line_break || ends_with_line_break);
            continue;
        }
        let line_container = node.ancestors().find_map(|ancestor| {
            ancestor
                .value()
                .as_element()
                .is_some_and(|element| publisher_text_element_starts_line(element.name()))
                .then(|| ancestor.id())
        });
        if !text.is_empty() {
            if pending_line_break
                || starts_with_line_break
                || line_container != previous_line_container
            {
                text.push('\n');
            } else {
                text.push(' ');
            }
        }
        text.push_str(&node_text);
        previous_line_container = line_container;
        pending_line_break = ends_with_line_break;
    }
    text
}

fn stylesheet_hidden_node_ids(document: &Html) -> HashSet<NodeId> {
    let mut hidden_nodes = HashSet::new();
    let style_selector = Selector::parse("style").expect("static style selector is valid");
    for stylesheet in document.select(&style_selector) {
        let stylesheet_text = stylesheet.text().collect::<String>();
        for rule in stylesheet_text.split('}') {
            let Some((selectors, declarations)) = rule.rsplit_once('{') else {
                continue;
            };
            if !inline_style_hides_element(declarations) {
                continue;
            }
            for selector_text in selectors.split(',') {
                let Ok(selector) = Selector::parse(selector_text.trim()) else {
                    continue;
                };
                hidden_nodes.extend(document.select(&selector).map(|element| element.id()));
            }
        }
    }
    hidden_nodes
}

fn publisher_node_is_hidden(
    node: NodeRef<'_, Node>,
    stylesheet_hidden: &HashSet<NodeId>,
    element_is_hidden: fn(&Element) -> bool,
) -> bool {
    std::iter::once(node)
        .chain(node.ancestors())
        .any(|candidate| {
            stylesheet_hidden.contains(&candidate.id())
                || candidate
                    .value()
                    .as_element()
                    .is_some_and(element_is_hidden)
        })
}

fn publisher_element_is_hidden(element: &Element) -> bool {
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
}

fn structurally_hidden_element(element: &Element) -> bool {
    publisher_element_is_hidden(element)
        || element.name() == "head"
        || (element.name() == "dialog" && element.attr("open").is_none())
        || (element.name() == "details" && element.attr("open").is_none())
}

fn publisher_text_element_starts_line(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "tbody"
            | "tfoot"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn normalize_publisher_text_fragment(
    value: &str,
    preserves_line_breaks: bool,
) -> (String, bool, bool) {
    let decoded = decode_html_entities(value);
    if !preserves_line_breaks {
        return (
            decoded.split_whitespace().collect::<Vec<_>>().join(" "),
            false,
            false,
        );
    }
    let starts_with_line_break = decoded
        .chars()
        .take_while(|character| character.is_whitespace())
        .any(|character| matches!(character, '\r' | '\n'));
    let ends_with_line_break = decoded
        .chars()
        .rev()
        .take_while(|character| character.is_whitespace())
        .any(|character| matches!(character, '\r' | '\n'));
    let text = decoded
        .split(['\r', '\n'])
        .filter_map(|line| {
            let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
            (!line.is_empty()).then_some(line)
        })
        .collect::<Vec<_>>()
        .join("\n");
    (text, starts_with_line_break, ends_with_line_break)
}

/// Return whether one evidence value is an exact structurally visible-body
/// text span in retained HTML.
///
/// Only HTML whitespace and entities are normalized. Case, punctuation, and
/// every alphanumeric character remain exact: this gate must never turn a
/// model-produced correction into listing evidence. Head metadata and hidden
/// or executable body content are excluded. Embedded stylesheet selectors are
/// applied, but external browser-computed CSS is not present in retained HTML.
pub fn listing_body_contains_exact_structurally_visible_text_span(
    html: &str,
    evidence: &str,
) -> bool {
    let evidence = normalize_page_text(evidence);
    if evidence.is_empty() {
        return false;
    }
    let document = Html::parse_document(html);
    let stylesheet_hidden_nodes = stylesheet_hidden_node_ids(&document);
    let mut text = String::new();
    for node in document.tree.root().descendants() {
        let Some(node_text) = node.value().as_text() else {
            continue;
        };
        let inside_body = node.ancestors().any(|ancestor| {
            ancestor
                .value()
                .as_element()
                .is_some_and(|element| element.name() == "body")
        });
        if !inside_body
            || publisher_node_is_hidden(node, &stylesheet_hidden_nodes, structurally_hidden_element)
        {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(node_text);
    }
    let visible = normalize_page_text(&text);
    visible.match_indices(&evidence).any(|(start, matched)| {
        let end = start + matched.len();
        visible[..start]
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_alphanumeric())
            && visible[end..]
                .chars()
                .next()
                .is_none_or(|character| !character.is_alphanumeric())
    })
}

fn inline_style_hides_element(style: &str) -> bool {
    style.split(';').any(|declaration| {
        let Some((property, value)) = declaration.split_once(':') else {
            return false;
        };
        let property = property.trim();
        (property.eq_ignore_ascii_case("display") && css_value_is(value, "none"))
            || (property.eq_ignore_ascii_case("visibility") && css_value_is(value, "hidden"))
            || (property.eq_ignore_ascii_case("visibility") && css_value_is(value, "collapse"))
            || (property.eq_ignore_ascii_case("content-visibility")
                && css_value_is(value, "hidden"))
            || (property.eq_ignore_ascii_case("opacity") && css_value_is(value, "0"))
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
        clean_listing_html, clean_publisher_source_html,
        listing_body_contains_exact_structurally_visible_text_span, normalize_source_evidence_span,
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

        assert_eq!(text, "Cessna 182\ndesignation 18 2");
        assert!(publisher_text_contains_evidence_span(&text, "Cessna 182"));
        assert!(!publisher_text_contains_evidence_span(
            &text,
            "designation 182"
        ));
    }

    #[test]
    fn publisher_source_text_preserves_semantic_lines_without_breaking_inline_text() {
        let text = clean_publisher_source_html(
            r#"<html><body>
              <div>Dual <strong>GDU-1044B</strong> PFD/MFD</div>
              <div>Dual GIA-63W NAV/COM/GPS/WAAS<br>Garmin GTX 33 Transponder ADS-B Compliant</div>
              <p>Literal first line
                 Literal second line</p>
            </body></html>"#,
        );

        assert_eq!(
            text,
            "Dual GDU-1044B PFD/MFD\nDual GIA-63W NAV/COM/GPS/WAAS\nGarmin GTX 33 Transponder ADS-B Compliant\nLiteral first line Literal second line"
        );
    }

    #[test]
    fn publisher_source_text_collapses_source_formatting_across_inline_markup() {
        let text = clean_publisher_source_html(
            "<div>GIA-63W NAV/COM/GPS/WAAS\n<strong>Garmin GTX 33 Transponder ADS-B Compliant</strong></div>",
        );

        assert_eq!(
            text,
            "GIA-63W NAV/COM/GPS/WAAS Garmin GTX 33 Transponder ADS-B Compliant"
        );
    }

    #[test]
    fn publisher_source_text_ignores_hidden_breaks_but_preserves_preformatted_lines() {
        assert_eq!(
            clean_publisher_source_html(
                "<div>Garmin GTX 33<br hidden>ADS-B compliant<br style='display:none'>still qualified</div>"
            ),
            "Garmin GTX 33 ADS-B compliant still qualified"
        );
        assert_eq!(
            clean_publisher_source_html(
                "<style>.gone { display: none }</style><div>Garmin GTX 33<br class='gone'>ADS-B compliant</div>"
            ),
            "Garmin GTX 33 ADS-B compliant",
            "an embedded-stylesheet-hidden break must not create a clause boundary"
        );
        assert_eq!(
            clean_publisher_source_html(
                "<pre><span>GIA-63W NAV/COM/GPS/WAAS</span>\n<strong>Garmin GTX 33 ADS-B compliant</strong></pre>"
            ),
            "GIA-63W NAV/COM/GPS/WAAS\nGarmin GTX 33 ADS-B compliant"
        );
    }

    #[test]
    fn publisher_source_text_does_not_privilege_publisher_specific_classes() {
        assert_eq!(
            clean_publisher_source_html(
                "<div class='other detail__specs-value'>Dual GDU-1044B PFD/MFD\nDual GIA-63W NAV/COM/GPS/WAAS\nGarmin GTX 33 ADS-B compliant</div>"
            ),
            "Dual GDU-1044B PFD/MFD Dual GIA-63W NAV/COM/GPS/WAAS Garmin GTX 33 ADS-B compliant"
        );
        assert_eq!(
            clean_publisher_source_html(
                "<div class='detail__specs-value-other'>GIA-63W\nGarmin GTX 33 ADS-B compliant</div>"
            ),
            "GIA-63W Garmin GTX 33 ADS-B compliant",
            "generic publisher cleaning never grants source-specific class privileges"
        );
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

    #[test]
    fn listing_evidence_requires_one_exact_visible_body_span() {
        let html = r#"
        <html>
          <head>
            <title>Title-only Garmin GDL 69A</title>
            <meta name="description" content="Meta-only Garmin GDL 69A">
            <script type="application/ld+json">{"name":"JSON-LD Garmin GDL 69A"}</script>
          </head>
          <body>
            <p>Installed Garmin GDL <strong>69A</strong>&nbsp;receiver</p>
            <script>Script-only Garmin GTX 345</script>
            <template>Template-only Garmin GTX 345</template>
            <div hidden>Hidden Garmin GTX 345</div>
            <div aria-hidden="true">ARIA-hidden Garmin GTX 345</div>
            <div style="display:none">CSS-hidden Garmin GTX 345</div>
          </body>
        </html>
        "#;

        assert!(listing_body_contains_exact_structurally_visible_text_span(
            html,
            "Installed Garmin GDL 69A receiver"
        ));
        assert!(!listing_body_contains_exact_structurally_visible_text_span(
            html,
            "installed Garmin GDL 69A receiver"
        ));
        assert!(!listing_body_contains_exact_structurally_visible_text_span(
            "<html><body>Garmin GNS 430W navigator</body></html>",
            "Garmin GNS 430"
        ));
        for rejected in [
            "Title-only Garmin GDL 69A",
            "Meta-only Garmin GDL 69A",
            "JSON-LD Garmin GDL 69A",
            "Script-only Garmin GTX 345",
            "Template-only Garmin GTX 345",
            "Hidden Garmin GTX 345",
            "ARIA-hidden Garmin GTX 345",
            "CSS-hidden Garmin GTX 345",
        ] {
            assert!(
                !listing_body_contains_exact_structurally_visible_text_span(html, rejected),
                "{rejected:?} must not become listing evidence"
            );
        }
    }

    #[test]
    fn listing_evidence_excludes_embedded_stylesheet_and_closed_container_text() {
        let html = r#"
        <html>
          <head>
            <style>
              .gone { display: none !important; }
              #collapsed { visibility: collapse; }
            </style>
          </head>
          <body>
            <div class="gone">Stylesheet-hidden Garmin GNS 430W</div>
            <div id="collapsed">Collapsed Garmin GTX 345</div>
            <details>Closed details Garmin GDL 69A</details>
            <dialog>Closed dialog Garmin GMA 1347</dialog>
            <details open>Open details Garmin GTN 750Xi</details>
            <dialog open>Open dialog Garmin GI 275</dialog>
          </body>
        </html>
        "#;

        for rejected in [
            "Stylesheet-hidden Garmin GNS 430W",
            "Collapsed Garmin GTX 345",
            "Closed details Garmin GDL 69A",
            "Closed dialog Garmin GMA 1347",
        ] {
            assert!(
                !listing_body_contains_exact_structurally_visible_text_span(html, rejected),
                "{rejected:?} must not become listing evidence"
            );
        }
        assert!(listing_body_contains_exact_structurally_visible_text_span(
            html,
            "Open details Garmin GTN 750Xi"
        ));
        assert!(listing_body_contains_exact_structurally_visible_text_span(
            html,
            "Open dialog Garmin GI 275"
        ));
    }
}
