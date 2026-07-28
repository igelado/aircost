//! Bounded, target-aware structural projection of publisher PDF documents.
//!
//! Generic extraction preserves `lopdf`'s physical fragments for grounded
//! research. Deterministic OEM proof instead reconstructs only visual rows
//! that contain a server-owned target component. Reconstruction groups text
//! exclusively by page and baseline; source-order adjacency is never enough.

use std::collections::{BTreeMap, HashMap, HashSet};

use lopdf::content::Content;
use lopdf::{Document, Encoding, LoadOptions, Object, ObjectId};

use super::{
    normalize_text_row, ProductIdentityTarget, TextRow, TextRowKind, MAX_TEXT_ROWS,
    MAX_TEXT_ROW_CHARACTERS,
};
use crate::gemini::interactions::{GeminiInteractionsError, GeminiInteractionsResult};

pub(crate) const MAX_PAGES: usize = 256;
pub(crate) const MAX_PAGE_DECOMPRESSED_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_TOTAL_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_RECORD_BASELINE_DELTA: f64 = 0.5;
const MAX_GRAPHICS_STATE_DEPTH: usize = 256;
const MAX_PAGE_TREE_DEPTH: usize = 256;
const MAX_INVOKED_XOBJECT_NAMES_PER_PAGE: usize = 4_096;
const MAX_INVOKED_FORM_XOBJECTS_PER_PAGE: usize = 64;
const MAX_INVOKED_FONTS_PER_PAGE: usize = 64;

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) max_pages: usize,
    pub(crate) max_page_decompressed_bytes: usize,
    pub(crate) max_total_text_bytes: usize,
}

pub(crate) const LIMITS: Limits = Limits {
    max_pages: MAX_PAGES,
    max_page_decompressed_bytes: MAX_PAGE_DECOMPRESSED_BYTES,
    max_total_text_bytes: MAX_TOTAL_TEXT_BYTES,
};

pub(crate) struct Extracted {
    pub(crate) publisher_text: String,
    pub(crate) source_text_rows: Vec<TextRow>,
    pub(crate) source_text_rows_complete: bool,
}

pub(crate) fn extract(
    pdf: &[u8],
    target: Option<&ProductIdentityTarget>,
) -> GeminiInteractionsResult<Extracted> {
    extract_with_limits(pdf, LIMITS, target)
}

pub(crate) fn validate_page_count(
    page_count: usize,
    max_pages: usize,
) -> GeminiInteractionsResult<()> {
    if page_count == 0 || page_count > max_pages {
        return Err(GeminiInteractionsError::InvalidResponse(format!(
            "public source PDF has {page_count} pages; expected 1..={max_pages}"
        )));
    }
    Ok(())
}

fn extract_with_limits(
    pdf: &[u8],
    limits: Limits,
    target: Option<&ProductIdentityTarget>,
) -> GeminiInteractionsResult<Extracted> {
    if limits.max_pages == 0
        || limits.max_page_decompressed_bytes == 0
        || limits.max_total_text_bytes == 0
    {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source PDF extraction limits must be positive".to_string(),
        ));
    }
    if !pdf.starts_with(b"%PDF-") {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source PDF is missing the PDF signature".to_string(),
        ));
    }
    let document = Document::load_mem_with_options(
        pdf,
        LoadOptions {
            strict: true,
            max_decompressed_size: Some(limits.max_page_decompressed_bytes),
            ..LoadOptions::default()
        },
    )
    .map_err(|_| {
        GeminiInteractionsError::InvalidResponse(
            "public source PDF failed strict parsing".to_string(),
        )
    })?;
    if document.is_encrypted() || document.was_encrypted() {
        return Err(GeminiInteractionsError::InvalidResponse(
            "encrypted public source PDFs are not allowed".to_string(),
        ));
    }
    let pages = document.get_pages();
    validate_page_count(pages.len(), limits.max_pages)?;

    let mut extracted = String::new();
    let mut source_text_rows = Vec::new();
    let mut source_text_rows_complete = true;
    let mut next_row_ordinal = 0usize;
    let mut total_text_bytes = 0usize;
    for (page_number, page_id) in pages.iter().map(|(number, id)| (*number, *id)) {
        match target {
            Some(target) => {
                let (visual_rows, page_complete) = target_visual_rows(
                    &document,
                    page_id,
                    target,
                    limits.max_page_decompressed_bytes,
                )?;
                source_text_rows_complete &= page_complete;
                for text in visual_rows {
                    let ordinal = next_row_ordinal;
                    next_row_ordinal = next_row_ordinal.saturating_add(1);
                    let Some(next_total_text_bytes) = total_text_bytes.checked_add(text.len())
                    else {
                        source_text_rows_complete = false;
                        continue;
                    };
                    if source_text_rows.len() >= MAX_TEXT_ROWS {
                        source_text_rows_complete = false;
                        continue;
                    }
                    if next_total_text_bytes > limits.max_total_text_bytes {
                        source_text_rows_complete = false;
                        continue;
                    }
                    total_text_bytes = next_total_text_bytes;
                    if !extracted.is_empty() {
                        extracted.push('\n');
                    }
                    extracted.push_str(&text);
                    source_text_rows.push(TextRow {
                        kind: TextRowKind::PdfVisualRow,
                        ordinal,
                        text,
                    });
                }
            }
            None => {
                let text = document
                    .extract_text_with_limit(&[page_number], limits.max_page_decompressed_bytes)
                    .map_err(|_| {
                        GeminiInteractionsError::InvalidResponse(format!(
                            "public source PDF page {page_number} text extraction failed"
                        ))
                    })?;
                total_text_bytes = total_text_bytes.checked_add(text.len()).ok_or_else(|| {
                    GeminiInteractionsError::InvalidResponse(
                        "public source PDF extracted text exceeded its byte cap".to_string(),
                    )
                })?;
                if total_text_bytes > limits.max_total_text_bytes {
                    return Err(GeminiInteractionsError::InvalidResponse(format!(
                        "public source PDF extracted text exceeds {} bytes",
                        limits.max_total_text_bytes
                    )));
                }
                if !extracted.is_empty() {
                    extracted.push('\n');
                }
                extracted.push_str(&text);
                for line in text.lines() {
                    let ordinal = next_row_ordinal;
                    next_row_ordinal = next_row_ordinal.saturating_add(1);
                    let text = normalize_text_row(line);
                    if text.is_empty() {
                        continue;
                    }
                    if source_text_rows.len() >= MAX_TEXT_ROWS
                        || text.chars().count() > MAX_TEXT_ROW_CHARACTERS
                    {
                        source_text_rows_complete = false;
                        continue;
                    }
                    source_text_rows.push(TextRow {
                        kind: TextRowKind::PdfPhysicalLine,
                        ordinal,
                        text,
                    });
                }
            }
        }
    }

    let publisher_text = normalize_text_row(&extracted);
    if publisher_text.is_empty() {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source PDF has no extractable publisher text".to_string(),
        ));
    }
    Ok(Extracted {
        publisher_text,
        source_text_rows,
        source_text_rows_complete,
    })
}

#[derive(Clone, Copy, Debug)]
struct Transform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Transform {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn concatenate(self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    fn point(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    fn has_horizontal_baseline(self) -> bool {
        const EPSILON: f64 = 0.000_001;
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .into_iter()
            .all(f64::is_finite)
            && self.b.abs() <= EPSILON
            && self.a.abs() > EPSILON
            && self.d.abs() > EPSILON
    }
}

fn page_display_transform(
    document: &Document,
    page_id: ObjectId,
) -> GeminiInteractionsResult<Transform> {
    let mut current_id = page_id;
    let mut visited = HashSet::new();
    for _ in 0..MAX_PAGE_TREE_DEPTH {
        if !visited.insert(current_id) {
            return Err(GeminiInteractionsError::InvalidResponse(
                "public source PDF page tree contains a cycle".to_string(),
            ));
        }
        let dictionary = document.get_dictionary(current_id).map_err(|_| {
            GeminiInteractionsError::InvalidResponse(
                "public source PDF page tree could not be read".to_string(),
            )
        })?;
        if let Ok(rotation) = dictionary.get(b"Rotate") {
            let degrees = rotation.as_i64().map_err(|_| {
                GeminiInteractionsError::InvalidResponse(
                    "public source PDF page rotation is invalid".to_string(),
                )
            })?;
            let degrees = degrees.rem_euclid(360);
            return match degrees {
                0 => Ok(Transform::IDENTITY),
                90 => Ok(Transform {
                    a: 0.0,
                    b: 1.0,
                    c: -1.0,
                    d: 0.0,
                    e: 0.0,
                    f: 0.0,
                }),
                180 => Ok(Transform {
                    a: -1.0,
                    b: 0.0,
                    c: 0.0,
                    d: -1.0,
                    e: 0.0,
                    f: 0.0,
                }),
                270 => Ok(Transform {
                    a: 0.0,
                    b: -1.0,
                    c: 1.0,
                    d: 0.0,
                    e: 0.0,
                    f: 0.0,
                }),
                _ => Err(GeminiInteractionsError::InvalidResponse(
                    "public source PDF page rotation is not a right angle".to_string(),
                )),
            };
        }
        let Ok(parent_id) = dictionary.get(b"Parent").and_then(Object::as_reference) else {
            return Ok(Transform::IDENTITY);
        };
        current_id = parent_id;
    }
    Err(GeminiInteractionsError::InvalidResponse(
        "public source PDF page tree exceeded its depth bound".to_string(),
    ))
}

#[derive(Clone, Debug)]
struct TextFragment {
    x: f64,
    y: f64,
    sequence: usize,
    text: String,
}

#[derive(Clone, Copy, Debug)]
struct TextPosition {
    current: Transform,
    line: Transform,
    leading: f64,
    known: bool,
}

impl Default for TextPosition {
    fn default() -> Self {
        Self {
            current: Transform::IDENTITY,
            line: Transform::IDENTITY,
            leading: 0.0,
            known: false,
        }
    }
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(*value as f64),
        _ => None,
    }
}

fn transform(operands: &[Object]) -> Option<Transform> {
    if operands.len() != 6 {
        return None;
    }
    let values = operands
        .iter()
        .take(6)
        .map(number)
        .collect::<Option<Vec<_>>>()?;
    let transform = Transform {
        a: values[0],
        b: values[1],
        c: values[2],
        d: values[3],
        e: values[4],
        f: values[5],
    };
    [
        transform.a,
        transform.b,
        transform.c,
        transform.d,
        transform.e,
        transform.f,
    ]
    .into_iter()
    .all(f64::is_finite)
    .then_some(transform)
}

fn decode_text_operands(
    encoding: &Encoding<'_>,
    operands: &[Object],
    output: &mut String,
) -> lopdf::Result<()> {
    for operand in operands {
        match operand {
            Object::String(bytes, _) => encoding.write_to_string(bytes, output)?,
            Object::Array(values) => {
                for value in values {
                    match value {
                        Object::String(bytes, _) => {
                            encoding.write_to_string(bytes, output)?;
                        }
                        Object::Integer(spacing) if *spacing < -100 => output.push(' '),
                        Object::Real(spacing) if *spacing < -100.0 => output.push(' '),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn collect_page_xobject_safety(
    document: &Document,
    page_id: ObjectId,
    content: &Content<Vec<lopdf::content::Operation>>,
    remaining_decompressed_bytes: &mut usize,
) -> GeminiInteractionsResult<HashMap<Vec<u8>, bool>> {
    let invoked = content
        .operations
        .iter()
        .filter(|operation| operation.operator == "Do")
        .filter_map(|operation| {
            operation
                .operands
                .first()
                .and_then(|operand| operand.as_name().ok())
                .map(ToOwned::to_owned)
        })
        .collect::<HashSet<_>>();
    if invoked.len() > MAX_INVOKED_XOBJECT_NAMES_PER_PAGE {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source PDF invoked too many XObjects on one page".to_string(),
        ));
    }

    fn object_is_safe(
        document: &Document,
        value: &Object,
        remaining_decompressed_bytes: &mut usize,
        inspected_forms: &mut usize,
    ) -> bool {
        let stream = match value {
            Object::Reference(id) => document.get_object(*id).and_then(Object::as_stream).ok(),
            Object::Stream(stream) => Some(stream),
            _ => None,
        };
        let Some(stream) = stream else {
            return false;
        };
        let subtype = stream.dict.get(b"Subtype").and_then(Object::as_name).ok();
        if subtype == Some(b"Image") {
            return true;
        }
        if subtype != Some(b"Form") {
            return false;
        }
        if *inspected_forms >= MAX_INVOKED_FORM_XOBJECTS_PER_PAGE {
            return false;
        }
        *inspected_forms = inspected_forms.saturating_add(1);
        if *remaining_decompressed_bytes == 0 {
            return false;
        }
        let Some(content) = stream
            .decompressed_content_with_limit(*remaining_decompressed_bytes)
            .ok()
        else {
            return false;
        };
        *remaining_decompressed_bytes = remaining_decompressed_bytes.saturating_sub(content.len());
        Content::decode(&content).ok().is_some_and(|content| {
            !content.operations.iter().any(|operation| {
                matches!(operation.operator.as_str(), "Tj" | "TJ" | "'" | "\"" | "Do")
            })
        })
    }

    fn collect(
        document: &Document,
        resources: &lopdf::Dictionary,
        invoked: &HashSet<Vec<u8>>,
        remaining_decompressed_bytes: &mut usize,
        inspected_forms: &mut usize,
        result: &mut HashMap<Vec<u8>, bool>,
    ) {
        let Some(xobjects) = resources
            .get(b"XObject")
            .ok()
            .and_then(|object| match object {
                Object::Reference(id) => document.get_dictionary(*id).ok(),
                Object::Dictionary(dictionary) => Some(dictionary),
                _ => None,
            })
        else {
            return;
        };
        for (name, value) in xobjects.iter().filter(|(name, _)| invoked.contains(*name)) {
            if result.contains_key(name) {
                continue;
            }
            let safe = object_is_safe(
                document,
                value,
                remaining_decompressed_bytes,
                inspected_forms,
            );
            result.insert(name.clone(), safe);
        }
    }

    let (direct_resources, inherited_resource_ids) =
        document.get_page_resources(page_id).map_err(|_| {
            GeminiInteractionsError::InvalidResponse(
                "public source PDF page resources could not be read".to_string(),
            )
        })?;
    let mut result = HashMap::new();
    let mut inspected_forms = 0usize;
    if let Some(resources) = direct_resources {
        collect(
            document,
            resources,
            &invoked,
            remaining_decompressed_bytes,
            &mut inspected_forms,
            &mut result,
        );
    }
    for resource_id in inherited_resource_ids {
        if let Ok(resources) = document.get_dictionary(resource_id) {
            collect(
                document,
                resources,
                &invoked,
                remaining_decompressed_bytes,
                &mut inspected_forms,
                &mut result,
            );
        }
    }
    for name in invoked {
        result.entry(name).or_insert(false);
    }
    Ok(result)
}

fn collect_page_font_encodings<'a>(
    document: &'a Document,
    page_id: ObjectId,
    content: &Content<Vec<lopdf::content::Operation>>,
    remaining_decompressed_bytes: usize,
) -> GeminiInteractionsResult<BTreeMap<Vec<u8>, Result<Encoding<'a>, ()>>> {
    let invoked = content
        .operations
        .iter()
        .filter(|operation| operation.operator == "Tf")
        .filter_map(|operation| {
            operation
                .operands
                .first()
                .and_then(|operand| operand.as_name().ok())
                .map(ToOwned::to_owned)
        })
        .collect::<HashSet<_>>();
    if invoked.len() > MAX_INVOKED_FONTS_PER_PAGE {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source PDF invoked too many fonts on one page".to_string(),
        ));
    }

    fn collect<'a>(
        document: &'a Document,
        resources: &'a lopdf::Dictionary,
        invoked: &HashSet<Vec<u8>>,
        result: &mut HashMap<Vec<u8>, &'a lopdf::Dictionary>,
    ) {
        let Some(fonts) = resources.get(b"Font").ok().and_then(|object| match object {
            Object::Reference(id) => document.get_dictionary(*id).ok(),
            Object::Dictionary(dictionary) => Some(dictionary),
            _ => None,
        }) else {
            return;
        };
        for (name, value) in fonts.iter().filter(|(name, _)| invoked.contains(*name)) {
            if result.contains_key(name) {
                continue;
            }
            let font = match value {
                Object::Reference(id) => document.get_dictionary(*id).ok(),
                Object::Dictionary(dictionary) => Some(dictionary),
                _ => None,
            };
            if let Some(font) = font {
                result.insert(name.clone(), font);
            }
        }
    }

    let (direct_resources, inherited_resource_ids) =
        document.get_page_resources(page_id).map_err(|_| {
            GeminiInteractionsError::InvalidResponse(
                "public source PDF page resources could not be read".to_string(),
            )
        })?;
    let mut fonts = HashMap::new();
    if let Some(resources) = direct_resources {
        collect(document, resources, &invoked, &mut fonts);
    }
    for resource_id in inherited_resource_ids {
        if let Ok(resources) = document.get_dictionary(resource_id) {
            collect(document, resources, &invoked, &mut fonts);
        }
    }

    let per_font_budget = if invoked.is_empty() {
        remaining_decompressed_bytes
    } else {
        remaining_decompressed_bytes / invoked.len()
    };
    let mut encodings = BTreeMap::new();
    for name in invoked {
        let encoding = fonts.get(&name).ok_or(()).and_then(|font| {
            if per_font_budget == 0 {
                return Err(());
            }
            font.get_font_encoding_with_limit(document, per_font_budget)
                .map_err(|_| ())
        });
        encodings.insert(name, encoding);
    }
    Ok(encodings)
}

struct PageText<'a> {
    encodings: BTreeMap<Vec<u8>, Result<Encoding<'a>, ()>>,
    xobject_safety: HashMap<Vec<u8>, bool>,
    content: Content<Vec<lopdf::content::Operation>>,
    display_transform: Transform,
}

fn load_page_text<'a>(
    document: &'a Document,
    page_id: ObjectId,
    max_decompressed_bytes: usize,
) -> GeminiInteractionsResult<PageText<'a>> {
    let content_data = document
        .get_page_content_with_limit(page_id, max_decompressed_bytes)
        .map_err(|_| {
            GeminiInteractionsError::InvalidResponse(
                "public source PDF page content exceeded its decompressed byte cap".to_string(),
            )
        })?;
    let content = Content::decode(&content_data).map_err(|_| {
        GeminiInteractionsError::InvalidResponse(
            "public source PDF page content could not be decoded".to_string(),
        )
    })?;
    let mut remaining_decompressed_bytes =
        max_decompressed_bytes.saturating_sub(content_data.len());
    let xobject_safety = collect_page_xobject_safety(
        document,
        page_id,
        &content,
        &mut remaining_decompressed_bytes,
    )?;
    let encodings =
        collect_page_font_encodings(document, page_id, &content, remaining_decompressed_bytes)?;
    let display_transform = page_display_transform(document, page_id)?;
    Ok(PageText {
        encodings,
        xobject_safety,
        content,
        display_transform,
    })
}

fn visit_text_fragments<F>(
    page: &PageText<'_>,
    max_decompressed_bytes: usize,
    mut visitor: F,
) -> GeminiInteractionsResult<()>
where
    F: FnMut(TextFragment),
{
    let mut current_font = None::<Vec<u8>>;
    let mut position = TextPosition::default();
    let mut graphics_transform = Transform::IDENTITY;
    let mut graphics_stack = Vec::<Transform>::new();
    let mut sequence = 0usize;
    let mut decoded_text_bytes = 0usize;
    let mut in_text_object = false;

    for operation in &page.content.operations {
        match operation.operator.as_str() {
            "q" => {
                if in_text_object {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF changes graphics scope inside a text object".to_string(),
                    ));
                }
                if graphics_stack.len() >= MAX_GRAPHICS_STATE_DEPTH {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF graphics-state depth exceeded its bound".to_string(),
                    ));
                }
                graphics_stack.push(graphics_transform);
            }
            "Q" => {
                if in_text_object {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF changes graphics scope inside a text object".to_string(),
                    ));
                }
                let Some(saved_transform) = graphics_stack.pop() else {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF graphics-state stack underflowed".to_string(),
                    ));
                };
                graphics_transform = saved_transform;
            }
            "cm" => {
                let transform = transform(&operation.operands).ok_or_else(|| {
                    GeminiInteractionsError::InvalidResponse(
                        "public source PDF contains an invalid coordinate transform".to_string(),
                    )
                })?;
                graphics_transform = graphics_transform.concatenate(transform);
            }
            "BT" => {
                if in_text_object {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF contains nested text objects".to_string(),
                    ));
                }
                in_text_object = true;
                position = TextPosition::default();
            }
            "ET" => {
                if !in_text_object {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF closes a missing text object".to_string(),
                    ));
                }
                in_text_object = false;
            }
            "Tf" => {
                current_font = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    .map(ToOwned::to_owned);
            }
            "Tm" => {
                let transform = transform(&operation.operands).ok_or_else(|| {
                    GeminiInteractionsError::InvalidResponse(
                        "public source PDF contains an invalid text matrix".to_string(),
                    )
                })?;
                position.current = transform;
                position.line = transform;
                position.known = true;
            }
            "Td" | "TD" => {
                let (Some(x), Some(y)) = (
                    operation.operands.first().and_then(number),
                    operation.operands.get(1).and_then(number),
                ) else {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF contains an invalid text-line displacement".to_string(),
                    ));
                };
                if operation.operands.len() != 2 || !x.is_finite() || !y.is_finite() {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF contains an invalid text-line displacement".to_string(),
                    ));
                }
                if operation.operator == "TD" {
                    position.leading = -y;
                }
                let translation = Transform {
                    e: x,
                    f: y,
                    ..Transform::IDENTITY
                };
                position.line = position.line.concatenate(translation);
                position.current = position.line;
                position.known = true;
            }
            "TL" => {
                let Some(leading) = operation.operands.first().and_then(number) else {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF contains invalid text leading".to_string(),
                    ));
                };
                if operation.operands.len() != 1 || !leading.is_finite() {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF contains invalid text leading".to_string(),
                    ));
                }
                position.leading = leading;
            }
            "T*" => {
                let translation = Transform {
                    e: 0.0,
                    f: -position.leading,
                    ..Transform::IDENTITY
                };
                position.line = position.line.concatenate(translation);
                position.current = position.line;
            }
            "Do" => {
                let name = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok());
                if !name.is_some_and(|name| page.xobject_safety.get(name).copied() == Some(true)) {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF invokes an unhandled text-bearing or invalid Form XObject"
                            .to_string(),
                    ));
                }
            }
            "Tj" | "TJ" | "'" | "\"" => {
                if matches!(operation.operator.as_str(), "'" | "\"") {
                    let translation = Transform {
                        e: 0.0,
                        f: -position.leading,
                        ..Transform::IDENTITY
                    };
                    position.line = position.line.concatenate(translation);
                    position.current = position.line;
                }
                let encoding = current_font
                    .as_ref()
                    .and_then(|font| page.encodings.get(font))
                    .and_then(|encoding| encoding.as_ref().ok())
                    .ok_or_else(|| {
                        GeminiInteractionsError::InvalidResponse(
                            "public source PDF text uses a missing or undecodable font".to_string(),
                        )
                    })?;
                let operands = if operation.operator == "\"" {
                    operation.operands.get(2..).unwrap_or_default()
                } else {
                    operation.operands.as_slice()
                };
                let mut text = String::new();
                decode_text_operands(encoding, operands, &mut text).map_err(|_| {
                    GeminiInteractionsError::InvalidResponse(
                        "public source PDF text could not be decoded".to_string(),
                    )
                })?;
                let text = normalize_text_row(&text);
                if text.is_empty() {
                    continue;
                }
                let visual_transform = page
                    .display_transform
                    .concatenate(graphics_transform)
                    .concatenate(position.current);
                if !in_text_object || !position.known || !visual_transform.has_horizontal_baseline()
                {
                    // This fragment cannot establish a visual row. It is
                    // excluded rather than flattened into a horizontal record;
                    // a document containing only such target text remains
                    // unresolved because it produces no eligible proof row.
                    continue;
                }
                decoded_text_bytes =
                    decoded_text_bytes.checked_add(text.len()).ok_or_else(|| {
                        GeminiInteractionsError::InvalidResponse(
                            "public source PDF decoded layout text exceeded its byte cap"
                                .to_string(),
                        )
                    })?;
                if decoded_text_bytes > max_decompressed_bytes {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF decoded layout text exceeded its byte cap".to_string(),
                    ));
                }
                let (x, y) = visual_transform.point(0.0, 0.0);
                visitor(TextFragment {
                    x,
                    y,
                    sequence,
                    text,
                });
                sequence = sequence.saturating_add(1);
            }
            _ => {}
        }
    }
    if in_text_object || !graphics_stack.is_empty() {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source PDF has an unbalanced text or graphics-state scope".to_string(),
        ));
    }
    Ok(())
}

fn target_visual_rows(
    document: &Document,
    page_id: ObjectId,
    target: &ProductIdentityTarget,
    max_decompressed_bytes: usize,
) -> GeminiInteractionsResult<(Vec<String>, bool)> {
    let page = load_page_text(document, page_id, max_decompressed_bytes)?;
    let mut target_baselines = Vec::<f64>::new();
    let mut complete = true;
    visit_text_fragments(&page, max_decompressed_bytes, |fragment| {
        if !target.row_is_relevant(&fragment.text)
            || target_baselines
                .iter()
                .any(|baseline| (fragment.y - baseline).abs() <= MAX_RECORD_BASELINE_DELTA)
        {
            return;
        }
        if target_baselines.len() >= MAX_TEXT_ROWS {
            complete = false;
            return;
        }
        target_baselines.push(fragment.y);
    })?;
    if target_baselines.is_empty() {
        return Ok((Vec::new(), complete));
    }

    target_baselines.sort_by(|left, right| right.total_cmp(left));
    let mut rows = target_baselines
        .iter()
        .copied()
        .map(|baseline| (baseline, Vec::<TextFragment>::new()))
        .collect::<Vec<_>>();
    let mut retained_fragments = 0usize;
    visit_text_fragments(&page, max_decompressed_bytes, |fragment| {
        let Some((_, row)) = rows
            .iter_mut()
            .find(|(baseline, _)| (fragment.y - *baseline).abs() <= MAX_RECORD_BASELINE_DELTA)
        else {
            return;
        };
        if retained_fragments >= MAX_TEXT_ROWS {
            complete = false;
            return;
        }
        retained_fragments = retained_fragments.saturating_add(1);
        row.push(fragment);
    })?;

    let mut result = Vec::new();
    for (_, mut fragments) in rows {
        fragments.sort_by(|left, right| {
            left.x
                .total_cmp(&right.x)
                .then_with(|| left.sequence.cmp(&right.sequence))
        });
        let text = normalize_text_row(
            &fragments
                .into_iter()
                .map(|fragment| fragment.text)
                .collect::<Vec<_>>()
                .join(" "),
        );
        if !target.row_is_relevant(&text) {
            continue;
        }
        if text.chars().count() > MAX_TEXT_ROW_CHARACTERS {
            complete = false;
            continue;
        }
        result.push(text);
    }
    Ok((result, complete))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avionics::source::{exact_oem_product_identity_row, OemProductIdentity};
    use lopdf::content::Operation;
    use lopdf::{dictionary, Stream};

    fn source_pdf(pages: &[&[&str]]) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let mut page_ids = Vec::new();
        for lines in pages {
            let mut operations = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 10.into()]),
                Operation::new("TL", vec![12.into()]),
                Operation::new("Td", vec![50.into(), 740.into()]),
            ];
            for line in *lines {
                operations.push(Operation::new("Tj", vec![Object::string_literal(*line)]));
                operations.push(Operation::new("T*", vec![]));
            }
            operations.push(Operation::new("ET", vec![]));
            let content_id = document.add_object(Stream::new(
                dictionary! {},
                Content { operations }.encode().unwrap(),
            ));
            page_ids.push(document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
            }));
        }
        finish_pdf(document, pages_id, resources_id, page_ids)
    }

    fn source_visual_row_pdf(rows: &[&[(i64, &str)]]) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let mut operations = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 10.into()]),
        ];
        for (row_index, row) in rows.iter().enumerate() {
            let y = 740_i64 - (row_index as i64 * 14);
            for (x, text) in *row {
                operations.push(Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        (*x).into(),
                        y.into(),
                    ],
                ));
                operations.push(Operation::new("Tj", vec![Object::string_literal(*text)]));
            }
        }
        operations.push(Operation::new("ET", vec![]));
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            Content { operations }.encode().unwrap(),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        finish_pdf(document, pages_id, resources_id, vec![page_id])
    }

    fn source_text_operations_pdf(
        mut operations: Vec<Operation>,
        rotation: Option<i64>,
    ) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        operations.insert(0, Operation::new("BT", vec![]));
        operations.insert(1, Operation::new("Tf", vec!["F1".into(), 10.into()]));
        operations.push(Operation::new("ET", vec![]));
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            Content { operations }.encode().unwrap(),
        ));
        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        };
        if let Some(rotation) = rotation {
            page.set("Rotate", rotation);
        }
        let page_id = document.add_object(page);
        finish_pdf(document, pages_id, resources_id, vec![page_id])
    }

    fn finish_pdf(
        mut document: Document,
        pages_id: ObjectId,
        resources_id: ObjectId,
        page_ids: Vec<ObjectId>,
    ) -> Vec<u8> {
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
                "Count" => page_ids.len() as i64,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn extracts_bounded_generic_text_and_physical_fragments() {
        let pdf = source_pdf(&[
            &[
                "GEA 71 Unit (011-00831-00) 010-00283-00",
                "GEA 71 Unit Rack 115-00411-00",
            ],
            &["GEA 71 Installation Manual"],
        ]);
        let extracted = extract(&pdf, None).unwrap();

        assert!(extracted
            .publisher_text
            .contains("GEA 71 Unit (011-00831-00) 010-00283-00"));
        assert_eq!(extracted.source_text_rows.len(), 3);
        assert!(extracted.source_text_rows_complete);
        assert!(extracted
            .source_text_rows
            .iter()
            .all(|row| row.kind == TextRowKind::PdfPhysicalLine));
    }

    #[test]
    fn resource_limits_and_encryption_fail_closed() {
        assert!(extract(b"%PDF-not-a-document", None).is_err());
        assert!(extract(&source_pdf(&[&[]]), None).is_err());

        let two_pages = source_pdf(&[&["Garmin GIA 63"], &["Garmin GIA 63W"]]);
        assert!(extract_with_limits(
            &two_pages,
            Limits {
                max_pages: 1,
                ..LIMITS
            },
            None,
        )
        .is_err());
        assert!(extract_with_limits(
            &two_pages,
            Limits {
                max_total_text_bytes: 4,
                ..LIMITS
            },
            None,
        )
        .is_err());

        let pdf = source_pdf(&[&["Garmin GIA 63W"]]);
        let mut document = Document::load_mem(&pdf).unwrap();
        let encrypt_id = document.add_object(dictionary! {
            "Filter" => "Standard",
            "V" => 1,
            "R" => 2,
            "Length" => 40,
            "O" => Object::string_literal("owner"),
            "U" => Object::string_literal("user"),
            "P" => -4,
        });
        document.trailer.set("Encrypt", encrypt_id);
        let mut encrypted = Vec::new();
        document.save_to(&mut encrypted).unwrap();
        assert!(extract(&encrypted, None).is_err());
    }

    #[test]
    fn targeted_projection_ignores_unrelated_noise_but_not_target_overflow() {
        let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
        let oversized = "unrelated ".repeat(MAX_TEXT_ROW_CHARACTERS);
        let mut crowded = vec!["unrelated"; MAX_TEXT_ROWS + 1];
        crowded.push("GEA 71 Unit (011-00831-00) 010-00283-00");
        let pdf = source_pdf(&[&[oversized.as_str()], crowded.as_slice()]);

        assert!(!extract(&pdf, None).unwrap().source_text_rows_complete);
        let targeted = extract(&pdf, Some(&target)).unwrap();
        assert!(targeted.source_text_rows_complete);
        assert_eq!(targeted.source_text_rows.len(), 1);
        assert_eq!(
            targeted.source_text_rows[0].text,
            "GEA 71 Unit (011-00831-00) 010-00283-00"
        );

        let oversized_target = format!(
            "GEA 71 011-00831-00 {}",
            "target filler ".repeat(MAX_TEXT_ROW_CHARACTERS)
        );
        assert!(extract(&source_pdf(&[&[&oversized_target]]), Some(&target)).is_err());
    }

    #[test]
    fn visual_rows_reconstruct_only_one_page_and_baseline() {
        let target = ProductIdentityTarget::new("ME406", "453-6603").unwrap();
        let pdf = source_visual_row_pdf(&[
            &[(40, "ME406 (453-6603),"), (220, "ME406HM (453-6604)")],
            &[
                (40, "Emergency Locator Transmitter"),
                (260, "453-6603"),
                (380, "ME406"),
            ],
            &[
                (40, "Emergency Locator Transmitter"),
                (260, "453-6604"),
                (380, "ME406HM"),
            ],
        ]);
        let extracted = extract(&pdf, Some(&target)).unwrap();

        assert!(extracted.source_text_rows_complete);
        assert_eq!(extracted.source_text_rows.len(), 2);
        let target_identity = OemProductIdentity {
            catalog_id: 125,
            model: "ME406",
            manufacturer_identifier: "453-6603",
        };
        let neighbor_identity = OemProductIdentity {
            catalog_id: 126,
            model: "ME406HM",
            manufacturer_identifier: "453-6604",
        };
        assert_eq!(
            exact_oem_product_identity_row(
                &extracted.source_text_rows,
                extracted.source_text_rows_complete,
                target_identity,
                &[target_identity, neighbor_identity],
            )
            .unwrap(),
            "Emergency Locator Transmitter 453-6603 ME406"
        );

        let split_pdf = source_visual_row_pdf(&[&[(40, "ME406")], &[(260, "453-6603")]]);
        let split = extract(&split_pdf, Some(&target)).unwrap();
        assert!(exact_oem_product_identity_row(
            &split.source_text_rows,
            split.source_text_rows_complete,
            target_identity,
            &[target_identity, neighbor_identity],
        )
        .is_err());
    }

    #[test]
    fn scaled_rotated_text_displacements_use_the_displayed_page_baseline() {
        let operations = vec![
            Operation::new(
                "Tm",
                vec![
                    0.into(),
                    (-2).into(),
                    2.into(),
                    0.into(),
                    100.into(),
                    700.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("GSU 75")]),
            Operation::new("Td", vec![100.into(), 0.into()]),
            Operation::new("Tj", vec![Object::string_literal("010-01127-00")]),
            Operation::new("Td", vec![(-100).into(), (-7).into()]),
            Operation::new("Tj", vec![Object::string_literal("GSU 75H")]),
            Operation::new("Td", vec![100.into(), 0.into()]),
            Operation::new("Tj", vec![Object::string_literal("010-01127-20")]),
        ];
        let target = ProductIdentityTarget::new("GSU 75", "010-01127-00").unwrap();
        let target_identity = OemProductIdentity {
            catalog_id: 734,
            model: "GSU 75",
            manufacturer_identifier: "010-01127-00",
        };
        let neighbor_identity = OemProductIdentity {
            catalog_id: 735,
            model: "GSU 75H",
            manufacturer_identifier: "010-01127-20",
        };

        let rotated = source_text_operations_pdf(operations.clone(), Some(90));
        let extracted = extract(&rotated, Some(&target)).unwrap();
        assert_eq!(
            exact_oem_product_identity_row(
                &extracted.source_text_rows,
                extracted.source_text_rows_complete,
                target_identity,
                &[target_identity, neighbor_identity],
            )
            .unwrap(),
            "GSU 75 010-01127-00"
        );
        assert!(!extracted
            .source_text_rows
            .iter()
            .any(|row| { row.text.contains("010-01127-00") && row.text.contains("010-01127-20") }));

        let not_display_horizontal = source_text_operations_pdf(operations, None);
        assert!(extract(&not_display_horizontal, Some(&target)).is_err());
    }

    #[test]
    fn missing_fonts_and_text_form_xobjects_fail_closed() {
        let mut missing_font =
            Document::load_mem(&source_visual_row_pdf(&[&[(40, "GEA 71 011-00831-00")]])).unwrap();
        let page_id = *missing_font.get_pages().values().next().unwrap();
        let pages_id = missing_font
            .get_dictionary(page_id)
            .unwrap()
            .get(b"Parent")
            .unwrap()
            .as_reference()
            .unwrap();
        missing_font
            .get_dictionary_mut(pages_id)
            .unwrap()
            .set("Resources", Object::Dictionary(dictionary! {}));
        let mut bytes = Vec::new();
        missing_font.save_to(&mut bytes).unwrap();
        let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
        assert!(extract(&bytes, Some(&target)).is_err());

        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let form = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            },
            Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tj", vec![Object::string_literal("ME406")]),
                    Operation::new("ET", vec![]),
                ],
            }
            .encode()
            .unwrap(),
        );
        let form_id = document.add_object(form);
        let resources_id = document.add_object(dictionary! {
            "XObject" => dictionary! { "TargetForm" => form_id },
        });
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            Content {
                operations: vec![Operation::new(
                    "Do",
                    vec![Object::Name(b"TargetForm".to_vec())],
                )],
            }
            .encode()
            .unwrap(),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        let bytes = finish_pdf(document, pages_id, resources_id, vec![page_id]);
        let target = ProductIdentityTarget::new("ME406", "453-6603").unwrap();
        assert!(extract(&bytes, Some(&target)).is_err());
    }

    #[test]
    fn nearest_resource_names_shadow_ancestors_and_unused_entries_are_ignored() {
        let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();

        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let inherited_image = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 1,
                "Height" => 1,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![0],
        ));
        let direct_text_form = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            },
            Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tj", vec![Object::string_literal("untracked text")]),
                    Operation::new("ET", vec![]),
                ],
            }
            .encode()
            .unwrap(),
        ));
        let parent_resources = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "XObject" => dictionary! { "Proof" => inherited_image },
        });
        let direct_resources = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "XObject" => dictionary! { "Proof" => direct_text_form },
        });
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            Content {
                operations: vec![Operation::new("Do", vec![Object::Name(b"Proof".to_vec())])],
            }
            .encode()
            .unwrap(),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Resources" => direct_resources,
            "Contents" => content_id,
        });
        let bytes = finish_pdf(document, pages_id, parent_resources, vec![page_id]);
        assert!(extract(&bytes, Some(&target)).is_err());

        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let valid_font = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let inherited_invalid_font = document.add_object(dictionary! { "Type" => "NotAFont" });
        let inherited_text_form = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            },
            Content {
                operations: vec![Operation::new(
                    "Tj",
                    vec![Object::string_literal("untracked text")],
                )],
            }
            .encode()
            .unwrap(),
        ));
        let direct_image = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 1,
                "Height" => 1,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![0],
        ));
        let unused_text_form = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            },
            Content {
                operations: vec![Operation::new(
                    "Tj",
                    vec![Object::string_literal("unused text")],
                )],
            }
            .encode()
            .unwrap(),
        ));
        let inherited_resources = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => inherited_invalid_font },
            "XObject" => dictionary! { "Proof" => inherited_text_form },
        });
        let direct_resources = document.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => valid_font,
                "UnusedInvalidFont" => dictionary! { "Type" => "NotAFont" },
            },
            "XObject" => dictionary! {
                "Proof" => direct_image,
                "UnusedTextForm" => unused_text_form,
            },
        });
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            Content {
                operations: vec![
                    Operation::new("Do", vec![Object::Name(b"Proof".to_vec())]),
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 10.into()]),
                    Operation::new(
                        "Tm",
                        vec![
                            1.into(),
                            0.into(),
                            0.into(),
                            1.into(),
                            40.into(),
                            700.into(),
                        ],
                    ),
                    Operation::new("Tj", vec![Object::string_literal("GEA 71 011-00831-00")]),
                    Operation::new("ET", vec![]),
                ],
            }
            .encode()
            .unwrap(),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Resources" => direct_resources,
            "Contents" => content_id,
        });
        let bytes = finish_pdf(document, pages_id, inherited_resources, vec![page_id]);
        let extracted = extract(&bytes, Some(&target)).unwrap();
        assert_eq!(extracted.source_text_rows[0].text, "GEA 71 011-00831-00");
    }

    #[test]
    fn invoked_font_and_form_count_and_decompression_budgets_fail_closed() {
        fn resource_heavy_pdf(font_count: usize, form_contents: &[Vec<u8>]) -> Vec<u8> {
            let mut document = Document::with_version("1.5");
            let pages_id = document.new_object_id();
            let font_id = document.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "Type1",
                "BaseFont" => "Courier",
            });
            let mut fonts = lopdf::Dictionary::new();
            let mut operations = Vec::new();
            for index in 0..font_count {
                let name = format!("F{index}");
                fonts.set(name.as_bytes(), font_id);
                operations.push(Operation::new(
                    "Tf",
                    vec![Object::Name(name.into_bytes()), 10.into()],
                ));
            }
            let mut xobjects = lopdf::Dictionary::new();
            for (index, form_content) in form_contents.iter().enumerate() {
                let name = format!("X{index}");
                let form_id = document.add_object(Stream::new(
                    dictionary! {
                        "Type" => "XObject",
                        "Subtype" => "Form",
                        "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                    },
                    form_content.clone(),
                ));
                xobjects.set(name.as_bytes(), form_id);
                operations.push(Operation::new("Do", vec![Object::Name(name.into_bytes())]));
            }
            operations.extend([
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F0".to_vec()), 10.into()]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        40.into(),
                        700.into(),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal("ME406 453-6603")]),
                Operation::new("ET", vec![]),
            ]);
            let resources_id = document.add_object(dictionary! {
                "Font" => fonts,
                "XObject" => xobjects,
            });
            let content_id = document.add_object(Stream::new(
                dictionary! {},
                Content { operations }.encode().unwrap(),
            ));
            let page_id = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
            });
            finish_pdf(document, pages_id, resources_id, vec![page_id])
        }

        let target = ProductIdentityTarget::new("ME406", "453-6603").unwrap();
        let too_many_fonts = resource_heavy_pdf(MAX_INVOKED_FONTS_PER_PAGE + 1, &[]);
        assert!(extract(&too_many_fonts, Some(&target)).is_err());

        let empty_form = Content {
            operations: vec![Operation::new("m", vec![0.into(), 0.into()])],
        }
        .encode()
        .unwrap();
        let too_many_forms =
            resource_heavy_pdf(1, &vec![empty_form; MAX_INVOKED_FORM_XOBJECTS_PER_PAGE + 1]);
        assert!(extract(&too_many_forms, Some(&target)).is_err());

        let large_form = b"0 0 m\n".repeat(100);
        let cumulative_form_overflow = resource_heavy_pdf(1, &[large_form.clone(), large_form]);
        assert!(extract_with_limits(
            &cumulative_form_overflow,
            Limits {
                max_page_decompressed_bytes: 1_024,
                ..LIMITS
            },
            Some(&target),
        )
        .is_err());
    }

    #[test]
    #[ignore = "requires manually downloaded official OEM PDF fixtures"]
    fn downloaded_official_oem_pdf_regressions() {
        let directory =
            std::env::var("AIRCOST_OEM_PDF_FIXTURE_DIR").expect("set AIRCOST_OEM_PDF_FIXTURE_DIR");
        let cases = [
            ("gdu1040.pdf", 3, "GDU 1040", "011-00972-00", None),
            ("gea71.pdf", 30, "GEA 71", "011-00831-00", None),
            (
                "me406.pdf",
                125,
                "ME406",
                "453-6603",
                Some((126, "ME406HM", "453-6604")),
            ),
            ("gea71b.pdf", 244, "GEA 71B", "011-03682-00", None),
            ("gsu75.pdf", 734, "GSU 75", "010-01127-00", None),
        ];
        for (file, catalog_id, model, identifier, neighbor) in cases {
            let pdf = std::fs::read(std::path::Path::new(&directory).join(file))
                .unwrap_or_else(|error| panic!("could not read {file}: {error}"));
            let target = ProductIdentityTarget::new(model, identifier).unwrap();
            let extracted = extract(&pdf, Some(&target))
                .unwrap_or_else(|error| panic!("{file} extraction failed: {error}"));
            let target_identity = OemProductIdentity {
                catalog_id,
                model,
                manufacturer_identifier: identifier,
            };
            let mut catalog = vec![target_identity];
            if let Some((catalog_id, model, manufacturer_identifier)) = neighbor {
                catalog.push(OemProductIdentity {
                    catalog_id,
                    model,
                    manufacturer_identifier,
                });
            }
            exact_oem_product_identity_row(
                &extracted.source_text_rows,
                extracted.source_text_rows_complete,
                target_identity,
                &catalog,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "{file} did not verify: {error}; retained target rows: {:?}",
                    extracted.source_text_rows
                )
            });
        }
    }
}
