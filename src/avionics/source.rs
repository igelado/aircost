//! Deterministic product proof from freshly fetched authoritative documents.
//!
//! Product identity may be established only by one bounded visible structural
//! row that contains the complete catalog model and stable identifier. HTML
//! table rows and PDF physical lines remain separate so adjacent products can
//! never be cross-paired.

use crate::gemini::curation::workflow::{
    direct_source_product_identity_signal_is_present, MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS,
};
use crate::gemini::interactions::{SourceTextRow, SourceTextRowKind};

#[derive(Clone, Copy, Debug)]
pub(crate) struct OemProductIdentity<'a> {
    pub(crate) catalog_id: i64,
    pub(crate) model: &'a str,
    pub(crate) manufacturer_identifier: &'a str,
}

/// Select the first source-order row that proves exactly the target product.
///
/// Repeated unambiguous rows are harmless, but a row that also proves a
/// different manufacturer-scoped catalog identity is not evidence for either
/// product. The caller remains responsible for source-origin admission,
/// immutable catalog checks, and duplicate catalog rejection.
pub(crate) fn exact_oem_product_identity_row(
    rows: &[SourceTextRow],
    rows_complete: bool,
    target: OemProductIdentity<'_>,
    manufacturer_catalog: &[OemProductIdentity<'_>],
) -> Result<String, String> {
    if !rows_complete {
        return Err(
            "the fetched document exceeded the bounded structural-row projection".to_string(),
        );
    }
    let mut matched_target = false;
    let mut first_clean_row = None;
    let mut first_ambiguous_ordinal = None;

    for row in rows {
        if !matches!(
            row.kind,
            SourceTextRowKind::HtmlTableRow | SourceTextRowKind::PdfPhysicalLine
        ) || !direct_source_product_identity_signal_is_present(
            &row.text,
            target.model,
            target.manufacturer_identifier,
        ) {
            continue;
        }
        matched_target = true;
        let conflicting_identity = manufacturer_catalog.iter().any(|candidate| {
            candidate.catalog_id != target.catalog_id
                && direct_source_product_identity_signal_is_present(
                    &row.text,
                    candidate.model,
                    candidate.manufacturer_identifier,
                )
        });
        if conflicting_identity {
            first_ambiguous_ordinal.get_or_insert(row.ordinal);
            continue;
        }
        if row.text.chars().count() > MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS {
            continue;
        }
        first_clean_row.get_or_insert_with(|| row.text.clone());
    }

    if let Some(ordinal) = first_ambiguous_ordinal {
        return Err(format!(
            "source row {ordinal} pairs the target with another manufacturer-scoped catalog identity"
        ));
    }
    if let Some(row) = first_clean_row {
        return Ok(row);
    }
    if matched_target {
        return Err(format!(
            "the exact source row exceeds the {MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS}-character evidence bound"
        ));
    }
    Err(
        "the freshly fetched source has no bounded visible HTML table row or PDF physical line containing the complete target model and stable identifier"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::{exact_oem_product_identity_row, OemProductIdentity};
    use crate::gemini::interactions::{SourceTextRow, SourceTextRowKind};

    fn row(kind: SourceTextRowKind, ordinal: usize, text: &str) -> SourceTextRow {
        SourceTextRow {
            kind,
            ordinal,
            text: text.to_string(),
        }
    }

    fn identity<'a>(
        catalog_id: i64,
        model: &'a str,
        manufacturer_identifier: &'a str,
    ) -> OemProductIdentity<'a> {
        OemProductIdentity {
            catalog_id,
            model,
            manufacturer_identifier,
        }
    }

    #[test]
    fn accepts_one_exact_visible_html_or_pdf_row() {
        let target = identity(30, "GEA 71", "011-00831-00");
        let catalog = [target];
        for kind in [
            SourceTextRowKind::HtmlTableRow,
            SourceTextRowKind::PdfPhysicalLine,
        ] {
            let rows = [row(kind, 4, "GEA 71 Unit (011-00831-00) | 010-00283-00")];
            assert_eq!(
                exact_oem_product_identity_row(&rows, true, target, &catalog).unwrap(),
                rows[0].text
            );
        }
    }

    #[test]
    fn never_cross_pairs_adjacent_rows() {
        let target = identity(33, "GTX 33", "011-00455-00");
        let catalog = [target];
        let rows = [
            row(SourceTextRowKind::PdfPhysicalLine, 0, "GTX 33"),
            row(SourceTextRowKind::PdfPhysicalLine, 1, "011-00455-00"),
        ];

        assert!(
            exact_oem_product_identity_row(&rows, true, target, &catalog)
                .unwrap_err()
                .contains("no bounded visible")
        );
    }

    #[test]
    fn rejects_a_row_that_proves_two_scoped_products() {
        let target = identity(125, "ME406", "453-6603");
        let neighbor = identity(126, "ME406HM", "453-6604");
        let catalog = [target, neighbor];
        let rows = [row(
            SourceTextRowKind::PdfPhysicalLine,
            11,
            "ME406 (453-6603), ME406HM (453-6604)",
        )];

        let error = exact_oem_product_identity_row(&rows, true, target, &catalog).unwrap_err();
        assert!(error.contains("row 11"));
        assert!(error.contains("another manufacturer-scoped"));
    }

    #[test]
    fn repeated_clean_target_rows_select_the_first_in_source_order() {
        let target = identity(244, "GEA 71B", "011-03682-00");
        let catalog = [target];
        let rows = [
            row(
                SourceTextRowKind::PdfPhysicalLine,
                8,
                "GEA 71B | 011-03682-00 | 497",
            ),
            row(
                SourceTextRowKind::PdfPhysicalLine,
                90,
                "GEA 71B (011-03682-00)",
            ),
        ];

        assert_eq!(
            exact_oem_product_identity_row(&rows, true, target, &catalog).unwrap(),
            rows[0].text
        );
    }

    #[test]
    fn later_ambiguous_target_row_invalidates_an_earlier_clean_row() {
        let target = identity(125, "ME406", "453-6603");
        let neighbor = identity(126, "ME406HM", "453-6604");
        let catalog = [target, neighbor];
        let rows = [
            row(
                SourceTextRowKind::PdfPhysicalLine,
                4,
                "Emergency Locator Transmitter | 453-6603 | ME406",
            ),
            row(
                SourceTextRowKind::PdfPhysicalLine,
                27,
                "ME406 (453-6603), ME406HM (453-6604)",
            ),
        ];

        let error = exact_oem_product_identity_row(&rows, true, target, &catalog).unwrap_err();
        assert!(error.contains("row 27"));
        assert!(error.contains("another manufacturer-scoped"));
    }

    #[test]
    fn exact_rows_over_the_durable_evidence_bound_fail_closed() {
        let target = identity(734, "GSU 75", "011-03094-00");
        let catalog = [target];
        let rows = [row(
            SourceTextRowKind::PdfPhysicalLine,
            0,
            &format!("GSU 75 (011-03094-00) {}", "bounded filler ".repeat(16)),
        )];

        assert!(
            exact_oem_product_identity_row(&rows, true, target, &catalog)
                .unwrap_err()
                .contains("128-character")
        );
    }

    #[test]
    fn incomplete_structural_projection_fails_before_row_selection() {
        let target = identity(30, "GEA 71", "011-00831-00");
        let catalog = [target];
        let rows = [row(
            SourceTextRowKind::HtmlTableRow,
            0,
            "GEA 71 | 011-00831-00",
        )];

        assert!(
            exact_oem_product_identity_row(&rows, false, target, &catalog)
                .unwrap_err()
                .contains("exceeded the bounded structural-row projection")
        );
    }
}
