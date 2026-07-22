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
    use super::clean_listing_html;

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
}
