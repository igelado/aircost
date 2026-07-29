//! Bounded, target-aware structural projection of publisher PDF documents.
//!
//! Generic extraction preserves `lopdf`'s physical fragments for grounded
//! research. Deterministic OEM proof instead reconstructs only visual rows
//! that contain a server-owned target component. Reconstruction groups text
//! exclusively by page and baseline; source-order adjacency is never enough.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

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
const MAX_FORM_XOBJECT_DEPTH: usize = 32;
const MAX_XOBJECT_INVOCATIONS_PER_PAGE: usize = 4_096;
const MAX_INVOKED_FONTS_PER_PAGE: usize = 64;
const MAX_CONTENT_OBJECT_DEPTH: usize = 256;
const MAX_CONTENT_OBJECTS_PER_PAGE: usize = 65_536;
const MAX_STREAM_FILTERS: usize = 16;
const MAX_PAGE_TREE_NODES: usize = MAX_PAGES * MAX_PAGE_TREE_DEPTH;

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
    let pages = strict_pages(&document, limits.max_pages)?;

    let mut extracted = String::new();
    let mut source_text_rows = Vec::new();
    let mut source_text_rows_complete = true;
    let mut next_row_ordinal = 0usize;
    let mut total_text_bytes = 0usize;
    for (page_number, page_id) in pages.iter().copied() {
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

fn strict_pages(
    document: &Document,
    max_pages: usize,
) -> GeminiInteractionsResult<Vec<(u32, ObjectId)>> {
    let catalog = document.catalog().map_err(|_| {
        GeminiInteractionsError::InvalidResponse(
            "public source PDF catalog could not be read".to_string(),
        )
    })?;
    if catalog.get(b"Type").and_then(Object::as_name).ok() != Some(b"Catalog") {
        return Err(GeminiInteractionsError::InvalidResponse(
            "public source PDF catalog has an invalid Type".to_string(),
        ));
    }
    let root = catalog
        .get(b"Pages")
        .and_then(Object::as_reference)
        .map_err(|_| {
            GeminiInteractionsError::InvalidResponse(
                "public source PDF catalog Pages is invalid".to_string(),
            )
        })?;
    let mut stack = vec![(root, None, 0usize)];
    let mut visited = HashSet::new();
    let mut pages = Vec::new();
    while let Some((id, expected_parent, depth)) = stack.pop() {
        if depth > MAX_PAGE_TREE_DEPTH || visited.len() >= MAX_PAGE_TREE_NODES {
            return Err(GeminiInteractionsError::InvalidResponse(
                "public source PDF page tree exceeded its structural bound".to_string(),
            ));
        }
        if !visited.insert(id) {
            return Err(GeminiInteractionsError::InvalidResponse(
                "public source PDF page tree contains a cycle or shared node".to_string(),
            ));
        }
        let dictionary = document.get_dictionary(id).map_err(|_| {
            GeminiInteractionsError::InvalidResponse(
                "public source PDF page tree node could not be read".to_string(),
            )
        })?;
        if let Some(expected_parent) = expected_parent {
            if dictionary
                .get(b"Parent")
                .and_then(Object::as_reference)
                .ok()
                != Some(expected_parent)
            {
                return Err(GeminiInteractionsError::InvalidResponse(
                    "public source PDF page tree Parent does not match its containing node"
                        .to_string(),
                ));
            }
        } else if dictionary.get(b"Parent").is_ok() {
            return Err(GeminiInteractionsError::InvalidResponse(
                "public source PDF root Pages node unexpectedly has a Parent".to_string(),
            ));
        }
        match dictionary.get(b"Type").and_then(Object::as_name).ok() {
            Some(b"Page") => {
                if dictionary.get(b"Kids").is_ok() {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF Page node unexpectedly has Kids".to_string(),
                    ));
                }
                pages.push(id);
                if pages.len() > max_pages {
                    return Err(GeminiInteractionsError::InvalidResponse(format!(
                        "public source PDF has more than {max_pages} pages"
                    )));
                }
            }
            Some(b"Pages") => {
                let kids = dictionary
                    .get_deref(b"Kids", document)
                    .and_then(Object::as_array)
                    .map_err(|_| {
                        GeminiInteractionsError::InvalidResponse(
                            "public source PDF Pages Kids is invalid".to_string(),
                        )
                    })?;
                if kids.is_empty() {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF Pages node has no Kids".to_string(),
                    ));
                }
                let mut child_ids = Vec::with_capacity(kids.len());
                for kid in kids {
                    child_ids.push(kid.as_reference().map_err(|_| {
                        GeminiInteractionsError::InvalidResponse(
                            "public source PDF Pages Kid is not an indirect reference".to_string(),
                        )
                    })?);
                }
                for child in child_ids.into_iter().rev() {
                    stack.push((child, Some(id), depth + 1));
                }
            }
            _ => {
                return Err(GeminiInteractionsError::InvalidResponse(
                    "public source PDF page tree node has an invalid Type".to_string(),
                ));
            }
        }
    }
    validate_page_count(pages.len(), max_pages)?;
    Ok(pages
        .into_iter()
        .enumerate()
        .map(|(index, id)| ((index + 1) as u32, id))
        .collect())
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

    fn inverse(self) -> Option<Self> {
        const EPSILON: f64 = 0.000_001;
        let determinant = self.a * self.d - self.b * self.c;
        if !determinant.is_finite() || determinant.abs() <= EPSILON {
            return None;
        }
        let inverse = Self {
            a: self.d / determinant,
            b: -self.b / determinant,
            c: -self.c / determinant,
            d: self.a / determinant,
            e: (self.c * self.f - self.d * self.e) / determinant,
            f: (self.b * self.e - self.a * self.f) / determinant,
        };
        [
            inverse.a, inverse.b, inverse.c, inverse.d, inverse.e, inverse.f,
        ]
        .into_iter()
        .all(f64::is_finite)
        .then_some(inverse)
    }
}

fn page_display_transform(
    document: &Document,
    page_id: ObjectId,
) -> GeminiInteractionsResult<Transform> {
    let mut current_id = page_id;
    let mut visited = HashSet::new();
    let mut rotation = None;
    let mut user_unit = None;
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
        if rotation.is_none() && dictionary.get(b"Rotate").is_ok() {
            let value = dictionary
                .get(b"Rotate")
                .expect("Rotate existence was checked");
            let degrees = value.as_i64().map_err(|_| {
                GeminiInteractionsError::InvalidResponse(
                    "public source PDF page rotation is invalid".to_string(),
                )
            })?;
            let degrees = degrees.rem_euclid(360);
            rotation = Some(match degrees {
                0 => Transform::IDENTITY,
                90 => Transform {
                    a: 0.0,
                    b: 1.0,
                    c: -1.0,
                    d: 0.0,
                    e: 0.0,
                    f: 0.0,
                },
                180 => Transform {
                    a: -1.0,
                    b: 0.0,
                    c: 0.0,
                    d: -1.0,
                    e: 0.0,
                    f: 0.0,
                },
                270 => Transform {
                    a: 0.0,
                    b: -1.0,
                    c: 1.0,
                    d: 0.0,
                    e: 0.0,
                    f: 0.0,
                },
                _ => {
                    return Err(GeminiInteractionsError::InvalidResponse(
                        "public source PDF page rotation is not a right angle".to_string(),
                    ));
                }
            });
        }
        if current_id == page_id && dictionary.get(b"UserUnit").is_ok() {
            let value = dictionary
                .get(b"UserUnit")
                .expect("UserUnit existence was checked");
            let scale = number(value).filter(|scale| scale.is_finite() && *scale > 0.0);
            user_unit = Some(scale.ok_or_else(|| {
                GeminiInteractionsError::InvalidResponse(
                    "public source PDF page UserUnit is invalid".to_string(),
                )
            })?);
        }
        let parent = match dictionary.get(b"Parent") {
            Ok(parent) => Some(parent.as_reference().map_err(|_| {
                GeminiInteractionsError::InvalidResponse(
                    "public source PDF page tree Parent is invalid".to_string(),
                )
            })?),
            Err(_) => None,
        };
        let Some(parent_id) = parent else {
            let scale = user_unit.unwrap_or(1.0);
            return Ok(Transform {
                a: scale,
                d: scale,
                ..Transform::IDENTITY
            }
            .concatenate(rotation.unwrap_or(Transform::IDENTITY)));
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
    known: bool,
    line_known: bool,
}

impl Default for TextPosition {
    fn default() -> Self {
        Self {
            current: Transform::IDENTITY,
            line: Transform::IDENTITY,
            known: false,
            line_known: false,
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

#[derive(Clone)]
struct ResourceScope<'a> {
    layers: Vec<&'a lopdf::Dictionary>,
}

#[derive(Clone)]
struct FontMetrics {
    widths: [Option<f64>; 256],
    bounds: Option<FormBounds>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum FormKey {
    Indirect(ObjectId),
    Direct(usize),
}

#[derive(Clone, Copy)]
struct FormBounds {
    left: f64,
    bottom: f64,
    right: f64,
    top: f64,
}

impl FormBounds {
    fn contains(self, x: f64, y: f64) -> bool {
        x.is_finite()
            && y.is_finite()
            && x >= self.left
            && x < self.right
            && y >= self.bottom
            && y < self.top
    }
}

struct PreparedForm<'a> {
    content: Rc<Content<Vec<lopdf::content::Operation>>>,
    resources: ResourceScope<'a>,
    matrix: Transform,
    bounds: FormBounds,
    proof_visibility_supported: bool,
}

struct PageText<'a> {
    document: &'a Document,
    root_resources: ResourceScope<'a>,
    encodings: HashMap<usize, Encoding<'a>>,
    font_metrics: HashMap<usize, FontMetrics>,
    forms: HashMap<FormKey, PreparedForm<'a>>,
    content: Content<Vec<lopdf::content::Operation>>,
    display_transform: Transform,
}

struct Preparation<'a> {
    document: &'a Document,
    root_resources: ResourceScope<'a>,
    remaining_decompressed_bytes: usize,
    fonts: BTreeMap<usize, &'a lopdf::Dictionary>,
    forms: HashMap<FormKey, PreparedForm<'a>>,
    active_forms: HashSet<FormKey>,
    invoked_xobjects: HashSet<FormKey>,
}

fn invalid_pdf(message: impl Into<String>) -> GeminiInteractionsError {
    GeminiInteractionsError::InvalidResponse(message.into())
}

fn validate_stream_filters(
    stream: &lopdf::Stream,
    description: &str,
) -> GeminiInteractionsResult<()> {
    if stream.dict.get(b"Filter").is_err() {
        return Ok(());
    }
    let filters = stream.filters().map_err(|_| {
        invalid_pdf(format!(
            "public source PDF {description} has an invalid Filter entry"
        ))
    })?;
    if filters.len() > MAX_STREAM_FILTERS {
        return Err(invalid_pdf(format!(
            "public source PDF {description} exceeds the filter-chain bound"
        )));
    }
    Ok(())
}

fn strict_stream_content(
    stream: &lopdf::Stream,
    max_decompressed_bytes: usize,
    description: &str,
) -> GeminiInteractionsResult<Vec<u8>> {
    validate_stream_filters(stream, description)?;
    stream
        .get_plain_content_with_limit(max_decompressed_bytes)
        .map_err(|_| {
            invalid_pdf(format!(
                "public source PDF {description} is undecodable or exceeded its decompressed byte cap"
            ))
        })
}

fn dictionary_object<'a>(
    document: &'a Document,
    object: &'a Object,
    description: &str,
) -> GeminiInteractionsResult<&'a lopdf::Dictionary> {
    match object {
        Object::Reference(id) => document
            .get_dictionary(*id)
            .map_err(|_| invalid_pdf(format!("public source PDF {description} could not be read"))),
        Object::Dictionary(dictionary) => Ok(dictionary),
        _ => Err(invalid_pdf(format!(
            "public source PDF {description} is not a dictionary"
        ))),
    }
}

fn page_resource_scope<'a>(
    document: &'a Document,
    page_id: ObjectId,
) -> GeminiInteractionsResult<ResourceScope<'a>> {
    let mut current_id = page_id;
    let mut visited = HashSet::new();
    for _ in 0..=MAX_PAGE_TREE_DEPTH {
        if !visited.insert(current_id) {
            return Err(invalid_pdf("public source PDF page tree contains a cycle"));
        }
        let dictionary = document
            .get_dictionary(current_id)
            .map_err(|_| invalid_pdf("public source PDF page tree could not be read"))?;
        if let Ok(resources) = dictionary.get(b"Resources") {
            return Ok(ResourceScope {
                layers: vec![dictionary_object(
                    document,
                    resources,
                    "nearest page Resources",
                )?],
            });
        }
        match dictionary.get(b"Parent") {
            Ok(parent) => {
                current_id = parent
                    .as_reference()
                    .map_err(|_| invalid_pdf("public source PDF page tree Parent is invalid"))?;
            }
            Err(_) => return Ok(ResourceScope { layers: Vec::new() }),
        }
    }
    Err(invalid_pdf(
        "public source PDF page tree exceeded its depth bound",
    ))
}

fn strict_page_content(
    document: &Document,
    page_id: ObjectId,
    max_decompressed_bytes: usize,
) -> GeminiInteractionsResult<Vec<u8>> {
    fn append(
        document: &Document,
        object: &Object,
        active_references: &mut HashSet<ObjectId>,
        visited_objects: &mut usize,
        output: &mut Vec<u8>,
        limit: usize,
        depth: usize,
    ) -> GeminiInteractionsResult<()> {
        if depth > MAX_CONTENT_OBJECT_DEPTH || *visited_objects >= MAX_CONTENT_OBJECTS_PER_PAGE {
            return Err(invalid_pdf(
                "public source PDF page Contents exceeded its structural bound",
            ));
        }
        *visited_objects = visited_objects.saturating_add(1);
        match object {
            Object::Reference(id) => {
                if !active_references.insert(*id) {
                    return Err(invalid_pdf(
                        "public source PDF page Contents contains a reference cycle",
                    ));
                }
                let resolved = document.get_object(*id).map_err(|_| {
                    invalid_pdf("public source PDF page Contents reference could not be read")
                })?;
                let result = append(
                    document,
                    resolved,
                    active_references,
                    visited_objects,
                    output,
                    limit,
                    depth + 1,
                );
                active_references.remove(id);
                result
            }
            Object::Array(objects) => {
                for object in objects {
                    append(
                        document,
                        object,
                        active_references,
                        visited_objects,
                        output,
                        limit,
                        depth + 1,
                    )?;
                }
                Ok(())
            }
            Object::Stream(stream) => {
                let separator = usize::from(!output.is_empty());
                let remaining = limit
                    .checked_sub(output.len())
                    .and_then(|remaining| remaining.checked_sub(separator))
                    .ok_or_else(|| {
                        invalid_pdf(
                            "public source PDF page content exceeded its decompressed byte cap",
                        )
                    })?;
                let data = strict_stream_content(stream, remaining, "page content stream")?;
                if separator != 0 {
                    output.push(b'\n');
                }
                output.extend_from_slice(&data);
                Ok(())
            }
            _ => Err(invalid_pdf(
                "public source PDF page Contents is not a stream or stream array",
            )),
        }
    }

    let page = document
        .get_dictionary(page_id)
        .map_err(|_| invalid_pdf("public source PDF page could not be read"))?;
    let contents = page
        .get(b"Contents")
        .map_err(|_| invalid_pdf("public source PDF page is missing Contents"))?;
    let mut output = Vec::new();
    append(
        document,
        contents,
        &mut HashSet::new(),
        &mut 0,
        &mut output,
        max_decompressed_bytes,
        0,
    )?;
    Ok(output)
}

fn named_resource<'a>(
    document: &'a Document,
    resources: &ResourceScope<'a>,
    category: &[u8],
    name: &[u8],
) -> GeminiInteractionsResult<&'a Object> {
    for layer in &resources.layers {
        let Ok(category_object) = layer.get(category) else {
            continue;
        };
        let category_dictionary = dictionary_object(
            document,
            category_object,
            &format!("{} resource dictionary", String::from_utf8_lossy(category)),
        )?;
        if let Ok(resource) = category_dictionary.get(name) {
            return Ok(resource);
        }
    }
    Err(invalid_pdf(format!(
        "public source PDF invoked missing {} resource {}",
        String::from_utf8_lossy(category),
        String::from_utf8_lossy(name)
    )))
}

fn font_dictionary<'a>(
    document: &'a Document,
    resources: &ResourceScope<'a>,
    name: &[u8],
) -> GeminiInteractionsResult<&'a lopdf::Dictionary> {
    dictionary_object(
        document,
        named_resource(document, resources, b"Font", name)?,
        "font resource",
    )
}

#[derive(Clone, Copy)]
enum ExpectedEncoding {
    OneByte,
    Differences,
    Unicode,
}

fn has_base14_latin_default_encoding(font: &lopdf::Dictionary, subtype: &[u8]) -> bool {
    subtype == b"Type1"
        && font.get(b"FontDescriptor").is_err()
        && font
            .get(b"BaseFont")
            .and_then(Object::as_name)
            .is_ok_and(|name| {
                matches!(
                    name,
                    b"Times-Roman"
                        | b"Times-Bold"
                        | b"Times-Italic"
                        | b"Times-BoldItalic"
                        | b"Helvetica"
                        | b"Helvetica-Bold"
                        | b"Helvetica-Oblique"
                        | b"Helvetica-BoldOblique"
                        | b"Courier"
                        | b"Courier-Bold"
                        | b"Courier-Oblique"
                        | b"Courier-BoldOblique"
                )
            })
}

fn expected_font_encoding(
    document: &Document,
    object: &Object,
    active: &mut HashSet<ObjectId>,
    depth: usize,
) -> GeminiInteractionsResult<ExpectedEncoding> {
    if depth > MAX_CONTENT_OBJECT_DEPTH {
        return Err(invalid_pdf(
            "public source PDF font Encoding exceeded its structural bound",
        ));
    }
    match object {
        Object::Reference(id) => {
            if !active.insert(*id) {
                return Err(invalid_pdf(
                    "public source PDF font Encoding contains a reference cycle",
                ));
            }
            let resolved = document
                .get_object(*id)
                .map_err(|_| invalid_pdf("public source PDF font Encoding could not be read"))?;
            let result = expected_font_encoding(document, resolved, active, depth + 1);
            active.remove(id);
            result
        }
        Object::Name(name) => match name.as_slice() {
            b"StandardEncoding" | b"MacRomanEncoding" | b"MacExpertEncoding"
            | b"WinAnsiEncoding" => Ok(ExpectedEncoding::OneByte),
            b"Identity-H" | b"Identity-V" => Ok(ExpectedEncoding::Unicode),
            _ => Err(invalid_pdf(
                "public source PDF font uses an unsupported named Encoding",
            )),
        },
        Object::Dictionary(dictionary) => {
            if dictionary.get(b"Type").and_then(Object::as_name).ok() != Some(b"Encoding") {
                return Err(invalid_pdf(
                    "public source PDF font Encoding dictionary has an invalid Type",
                ));
            }
            let base_encoding = dictionary.get(b"BaseEncoding").map_err(|_| {
                invalid_pdf("public source PDF font Encoding dictionary is missing BaseEncoding")
            })?;
            if !base_encoding.as_name().is_ok_and(|name| {
                matches!(
                    name,
                    b"StandardEncoding"
                        | b"MacRomanEncoding"
                        | b"MacExpertEncoding"
                        | b"WinAnsiEncoding"
                )
            }) {
                return Err(invalid_pdf(
                    "public source PDF font Encoding dictionary has an unsupported BaseEncoding",
                ));
            }
            let differences = dictionary
                .get(b"Differences")
                .and_then(Object::as_array)
                .map_err(|_| {
                    invalid_pdf(
                        "public source PDF font Encoding dictionary has invalid Differences",
                    )
                })?;
            let mut next_code = None;
            let mut names_after_code = false;
            for value in differences {
                match value {
                    Object::Integer(code) if (0..=255).contains(code) => {
                        if next_code.is_some() && !names_after_code {
                            return Err(invalid_pdf(
                                "public source PDF font Encoding dictionary has invalid Differences",
                            ));
                        }
                        next_code = Some(*code as u16);
                        names_after_code = false;
                    }
                    Object::Name(_) => {
                        let code = next_code.ok_or_else(|| {
                            invalid_pdf(
                                "public source PDF font Encoding dictionary has invalid Differences",
                            )
                        })?;
                        if code > 255 {
                            return Err(invalid_pdf(
                                "public source PDF font Encoding dictionary has invalid Differences",
                            ));
                        }
                        next_code = Some(code + 1);
                        names_after_code = true;
                    }
                    _ => {
                        return Err(invalid_pdf(
                            "public source PDF font Encoding dictionary has invalid Differences",
                        ));
                    }
                }
            }
            if next_code.is_none() || !names_after_code {
                return Err(invalid_pdf(
                    "public source PDF font Encoding dictionary has invalid Differences",
                ));
            }
            Ok(ExpectedEncoding::Differences)
        }
        _ => Err(invalid_pdf(
            "public source PDF font Encoding has an invalid object type",
        )),
    }
}

fn strict_font_encoding<'a>(
    document: &'a Document,
    font: &'a lopdf::Dictionary,
    max_decompressed_bytes: usize,
) -> GeminiInteractionsResult<Encoding<'a>> {
    if font.get(b"Type").and_then(Object::as_name).ok() != Some(b"Font") {
        return Err(invalid_pdf(
            "public source PDF invoked resource is not a Font",
        ));
    }
    let subtype = font
        .get(b"Subtype")
        .and_then(Object::as_name)
        .map_err(|_| invalid_pdf("public source PDF font has an invalid Subtype"))?;
    if !matches!(subtype, b"Type1" | b"MMType1" | b"TrueType") {
        return Err(invalid_pdf(
            "public source PDF font uses an unsupported Subtype",
        ));
    }
    let expected = match font.get(b"Encoding") {
        Ok(encoding) => expected_font_encoding(document, encoding, &mut HashSet::new(), 0)?,
        Err(_) if font.get(b"ToUnicode").is_ok() => ExpectedEncoding::Unicode,
        Err(_) if has_base14_latin_default_encoding(font, subtype) => ExpectedEncoding::OneByte,
        Err(_) => {
            return Err(invalid_pdf(
                "public source PDF font is missing an explicit Encoding or ToUnicode map",
            ));
        }
    };

    if font.get(b"ToUnicode").is_ok() {
        let to_unicode = font
            .get_deref(b"ToUnicode", document)
            .and_then(Object::as_stream)
            .map_err(|_| invalid_pdf("public source PDF font ToUnicode is not a stream"))?;
        validate_stream_filters(to_unicode, "font ToUnicode stream")?;
    }

    if font.get(b"Encoding").is_ok()
        && font.get(b"ToUnicode").is_ok()
        && !matches!(expected, ExpectedEncoding::Unicode)
    {
        let mut probe = font.clone();
        probe.remove(b"Encoding");
        let encoding = probe
            .get_font_encoding_with_limit(document, max_decompressed_bytes)
            .map_err(|_| {
                invalid_pdf("public source PDF font ToUnicode map could not be decoded")
            })?;
        if !matches!(encoding, Encoding::UnicodeMapEncoding(_)) {
            return Err(invalid_pdf(
                "public source PDF font ToUnicode map is malformed or unsupported",
            ));
        }
    } else if matches!(expected, ExpectedEncoding::Unicode) && font.get(b"ToUnicode").is_err() {
        return Err(invalid_pdf(
            "public source PDF identity font Encoding is missing ToUnicode",
        ));
    }

    let encoding = font
        .get_font_encoding_with_limit(document, max_decompressed_bytes)
        .map_err(|_| invalid_pdf("public source PDF font Encoding could not be decoded"))?;
    let matches_expected = matches!(
        (&expected, &encoding),
        (ExpectedEncoding::OneByte, Encoding::OneByteEncoding(_))
            | (ExpectedEncoding::Differences, Encoding::Differences(_))
            | (ExpectedEncoding::Unicode, Encoding::UnicodeMapEncoding(_))
    );
    if !matches_expected {
        return Err(invalid_pdf(
            "public source PDF font Encoding was malformed or unsupported",
        ));
    }
    Ok(encoding)
}

fn font_metrics(
    document: &Document,
    font: &lopdf::Dictionary,
) -> GeminiInteractionsResult<FontMetrics> {
    let mut widths = [None; 256];
    let mut bounds = None;
    let descriptor = match font.get(b"FontDescriptor") {
        Ok(value) => Some(dictionary_object(document, value, "FontDescriptor")?),
        Err(_) => None,
    };
    if let Some(descriptor) = descriptor {
        let bbox = descriptor
            .get_deref(b"FontBBox", document)
            .and_then(Object::as_array)
            .map_err(|_| invalid_pdf("public source PDF FontDescriptor has an invalid FontBBox"))?;
        if bbox.len() != 4 {
            return Err(invalid_pdf(
                "public source PDF FontDescriptor has an invalid FontBBox",
            ));
        }
        let values = bbox.iter().map(number).collect::<Option<Vec<_>>>();
        let Some(values) = values.filter(|values| values.iter().copied().all(f64::is_finite))
        else {
            return Err(invalid_pdf(
                "public source PDF FontDescriptor has an invalid FontBBox",
            ));
        };
        let candidate = FormBounds {
            left: values[0],
            bottom: values[1],
            right: values[2],
            top: values[3],
        };
        if candidate.left < candidate.right && candidate.bottom < candidate.top {
            bounds = Some(candidate);
        }
        if let Ok(value) = descriptor.get(b"MissingWidth") {
            let missing = number(value)
                .filter(|value| value.is_finite())
                .ok_or_else(|| {
                    invalid_pdf("public source PDF FontDescriptor has an invalid MissingWidth")
                })?;
            widths.fill(Some(missing));
        }
    }
    if let Ok(width_values) = font.get_deref(b"Widths", document) {
        let width_values = width_values
            .as_array()
            .map_err(|_| invalid_pdf("public source PDF font Widths is not an array"))?;
        let first = font
            .get(b"FirstChar")
            .and_then(Object::as_i64)
            .map_err(|_| invalid_pdf("public source PDF font FirstChar is invalid"))?;
        if !(0..=255).contains(&first) || first as usize + width_values.len() > widths.len() {
            return Err(invalid_pdf(
                "public source PDF font Widths range is invalid",
            ));
        }
        for (offset, value) in width_values.iter().enumerate() {
            widths[first as usize + offset] = Some(
                number(value)
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| {
                        invalid_pdf("public source PDF font Widths contains an invalid width")
                    })?,
            );
        }
    }
    let base_font = font
        .get(b"BaseFont")
        .and_then(Object::as_name)
        .ok()
        .unwrap_or_default();
    if base_font == b"Courier"
        && font.get(b"FontDescriptor").is_err()
        && font.get(b"Widths").is_err()
    {
        for width in &mut widths {
            *width = Some(600.0);
        }
        bounds.get_or_insert(FormBounds {
            left: -23.0,
            bottom: -250.0,
            right: 715.0,
            top: 805.0,
        });
    }
    Ok(FontMetrics { widths, bounds })
}

fn ext_gstate_dictionary<'a>(
    document: &'a Document,
    resources: &ResourceScope<'a>,
    name: &[u8],
) -> GeminiInteractionsResult<&'a lopdf::Dictionary> {
    dictionary_object(
        document,
        named_resource(document, resources, b"ExtGState", name)?,
        "ExtGState resource",
    )
}

fn ext_gstate_font<'a>(
    document: &'a Document,
    ext_gstate: &'a lopdf::Dictionary,
) -> GeminiInteractionsResult<Option<(&'a lopdf::Dictionary, f64)>> {
    let Ok(value) = ext_gstate.get(b"Font") else {
        return Ok(None);
    };
    let values = value.as_array().map_err(|_| {
        invalid_pdf("public source PDF ExtGState Font entry is not a font/size array")
    })?;
    if values.len() != 2 {
        return Err(invalid_pdf(
            "public source PDF ExtGState Font entry is not a font/size array",
        ));
    }
    let font = dictionary_object(document, &values[0], "ExtGState font")?;
    let size = number(&values[1])
        .filter(|size| size.is_finite())
        .ok_or_else(|| invalid_pdf("public source PDF ExtGState Font entry has an invalid size"))?;
    Ok(Some((font, size)))
}

fn ext_gstate_proof_visibility_supported(
    ext_gstate: &lopdf::Dictionary,
) -> GeminiInteractionsResult<bool> {
    for key in [b"CA".as_slice(), b"ca".as_slice()] {
        if let Ok(value) = ext_gstate.get(key) {
            let alpha = number(value)
                .filter(|alpha| alpha.is_finite())
                .ok_or_else(|| {
                    invalid_pdf("public source PDF ExtGState has an invalid alpha constant")
                })?;
            if alpha <= 0.0 {
                return Ok(false);
            }
        }
    }
    if let Ok(soft_mask) = ext_gstate.get(b"SMask") {
        if soft_mask.as_name().ok() != Some(b"None") {
            return Ok(false);
        }
    }
    if let Ok(blend_mode) = ext_gstate.get(b"BM") {
        let supported = match blend_mode {
            Object::Name(name) => matches!(name.as_slice(), b"Normal" | b"Compatible"),
            Object::Array(values) => values.iter().all(|value| {
                value
                    .as_name()
                    .is_ok_and(|name| matches!(name, b"Normal" | b"Compatible"))
            }),
            _ => false,
        };
        if !supported {
            return Ok(false);
        }
    }
    Ok(true)
}

fn resolved_xobject<'a>(
    document: &'a Document,
    resources: &ResourceScope<'a>,
    name: &[u8],
) -> GeminiInteractionsResult<(FormKey, &'a lopdf::Stream)> {
    let object = named_resource(document, resources, b"XObject", name)?;
    match object {
        Object::Reference(id) => Ok((
            FormKey::Indirect(*id),
            document
                .get_object(*id)
                .and_then(Object::as_stream)
                .map_err(|_| invalid_pdf("public source PDF XObject resource is not a stream"))?,
        )),
        Object::Stream(stream) => Ok((
            FormKey::Direct(stream as *const lopdf::Stream as usize),
            stream,
        )),
        _ => Err(invalid_pdf(
            "public source PDF XObject resource is not a stream",
        )),
    }
}

fn optional_transform(
    document: &Document,
    dictionary: &lopdf::Dictionary,
    name: &[u8],
) -> GeminiInteractionsResult<Transform> {
    if dictionary.get(name).is_err() {
        return Ok(Transform::IDENTITY);
    }
    let value = dictionary.get_deref(name, document).map_err(|_| {
        invalid_pdf(format!(
            "public source PDF Form {} could not be resolved",
            String::from_utf8_lossy(name)
        ))
    })?;
    let operands = value.as_array().map_err(|_| {
        invalid_pdf(format!(
            "public source PDF Form {} is not a matrix",
            String::from_utf8_lossy(name)
        ))
    })?;
    transform(operands).ok_or_else(|| {
        invalid_pdf(format!(
            "public source PDF Form {} is invalid",
            String::from_utf8_lossy(name)
        ))
    })
}

fn form_bounds(
    document: &Document,
    dictionary: &lopdf::Dictionary,
) -> GeminiInteractionsResult<FormBounds> {
    let values = dictionary
        .get_deref(b"BBox", document)
        .and_then(Object::as_array)
        .map_err(|_| {
            invalid_pdf("public source PDF Form XObject BBox is missing or is not an array")
        })?;
    if values.len() != 4 {
        return Err(invalid_pdf(
            "public source PDF Form XObject BBox does not contain four coordinates",
        ));
    }
    let numbers = values.iter().map(number).collect::<Option<Vec<_>>>();
    let Some(numbers) = numbers else {
        return Err(invalid_pdf(
            "public source PDF Form XObject BBox contains a nonnumeric coordinate",
        ));
    };
    let bounds = FormBounds {
        left: numbers[0],
        bottom: numbers[1],
        right: numbers[2],
        top: numbers[3],
    };
    if !numbers.into_iter().all(f64::is_finite) {
        return Err(invalid_pdf(
            "public source PDF Form XObject BBox is non-finite",
        ));
    }
    Ok(bounds)
}

fn form_resource_scope<'a>(
    preparation: &Preparation<'a>,
    stream: &'a lopdf::Stream,
) -> GeminiInteractionsResult<ResourceScope<'a>> {
    match stream.dict.get(b"Resources") {
        Ok(resources) => Ok(ResourceScope {
            layers: vec![dictionary_object(
                preparation.document,
                resources,
                "Form XObject Resources",
            )?],
        }),
        Err(_) => Ok(preparation.root_resources.clone()),
    }
}

fn validate_xobject_type<'a>(
    document: &'a Document,
    stream: &'a lopdf::Stream,
) -> GeminiInteractionsResult<&'a [u8]> {
    if stream.dict.get(b"Type").is_ok() {
        let object_type = stream
            .dict
            .get_deref(b"Type", document)
            .map_err(|_| invalid_pdf("public source PDF XObject Type could not be resolved"))?;
        if object_type.as_name().ok() != Some(b"XObject") {
            return Err(invalid_pdf("public source PDF XObject has an invalid Type"));
        }
    }
    stream
        .dict
        .get_deref(b"Subtype", document)
        .and_then(Object::as_name)
        .map_err(|_| invalid_pdf("public source PDF XObject has an invalid Subtype"))
}

fn validate_text_show_operation(
    operation: &lopdf::content::Operation,
) -> GeminiInteractionsResult<()> {
    let valid_number = |value: &Object| number(value).is_some_and(f64::is_finite);
    let valid = match operation.operator.as_str() {
        "Tj" | "'" => {
            operation.operands.len() == 1
                && matches!(operation.operands.first(), Some(Object::String(_, _)))
        }
        "TJ" => {
            operation.operands.len() == 1
                && operation
                    .operands
                    .first()
                    .and_then(|value| value.as_array().ok())
                    .is_some_and(|values| {
                        values.iter().all(|value| {
                            matches!(value, Object::String(_, _)) || valid_number(value)
                        })
                    })
        }
        "\"" => {
            operation.operands.len() == 3
                && valid_number(&operation.operands[0])
                && valid_number(&operation.operands[1])
                && matches!(operation.operands[2], Object::String(_, _))
        }
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid_pdf(format!(
            "public source PDF has an invalid {} operation",
            operation.operator
        )))
    }
}

fn prepare_content<'a>(
    preparation: &mut Preparation<'a>,
    content: &Content<Vec<lopdf::content::Operation>>,
    resources: &ResourceScope<'a>,
    depth: usize,
) -> GeminiInteractionsResult<()> {
    if depth > MAX_FORM_XOBJECT_DEPTH {
        return Err(invalid_pdf(
            "public source PDF Form XObject depth exceeded its bound",
        ));
    }
    for operation in &content.operations {
        match operation.operator.as_str() {
            "Tf" => {
                let name = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    .ok_or_else(|| invalid_pdf("public source PDF has an invalid Tf operation"))?;
                let size =
                    operation.operands.get(1).and_then(number).ok_or_else(|| {
                        invalid_pdf("public source PDF has an invalid Tf operation")
                    })?;
                if operation.operands.len() != 2 || !size.is_finite() {
                    return Err(invalid_pdf("public source PDF has an invalid Tf operation"));
                }
                let font = font_dictionary(preparation.document, resources, name)?;
                let key = font as *const lopdf::Dictionary as usize;
                preparation.fonts.entry(key).or_insert(font);
                if preparation.fonts.len() > MAX_INVOKED_FONTS_PER_PAGE {
                    return Err(invalid_pdf(
                        "public source PDF invoked too many fonts on one page",
                    ));
                }
            }
            "Do" => {
                let name = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    .ok_or_else(|| invalid_pdf("public source PDF has an invalid Do operation"))?;
                if operation.operands.len() != 1 {
                    return Err(invalid_pdf("public source PDF has an invalid Do operation"));
                }
                let (key, stream) = resolved_xobject(preparation.document, resources, name)?;
                preparation.invoked_xobjects.insert(key);
                if preparation.invoked_xobjects.len() > MAX_INVOKED_XOBJECT_NAMES_PER_PAGE {
                    return Err(invalid_pdf(
                        "public source PDF invoked too many XObjects on one page",
                    ));
                }
                match validate_xobject_type(preparation.document, stream)? {
                    b"Image" => {}
                    b"Form" => prepare_form(preparation, key, stream, depth)?,
                    _ => {
                        return Err(invalid_pdf(
                            "public source PDF invoked an unsupported XObject subtype",
                        ));
                    }
                }
            }
            "Tr" => {
                let mode = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_i64().ok())
                    .ok_or_else(|| invalid_pdf("public source PDF has an invalid Tr operation"))?;
                if operation.operands.len() != 1 || !(0..=7).contains(&mode) {
                    return Err(invalid_pdf("public source PDF has an invalid Tr operation"));
                }
            }
            "Tj" | "TJ" | "'" | "\"" => validate_text_show_operation(operation)?,
            "Tc" | "Tw" | "Tz" | "Ts" | "TL" => {
                let value = operation
                    .operands
                    .first()
                    .and_then(number)
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| {
                        invalid_pdf(format!(
                            "public source PDF has an invalid {} operation",
                            operation.operator
                        ))
                    })?;
                if operation.operands.len() != 1
                    || (operation.operator == "Tz" && value.abs() <= f64::EPSILON)
                {
                    return Err(invalid_pdf(format!(
                        "public source PDF has an invalid {} operation",
                        operation.operator
                    )));
                }
            }
            "gs" => {
                let name = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    .ok_or_else(|| invalid_pdf("public source PDF has an invalid gs operation"))?;
                if operation.operands.len() != 1 {
                    return Err(invalid_pdf("public source PDF has an invalid gs operation"));
                }
                let ext_gstate = ext_gstate_dictionary(preparation.document, resources, name)?;
                ext_gstate_proof_visibility_supported(ext_gstate)?;
                if let Some((font, _)) = ext_gstate_font(preparation.document, ext_gstate)? {
                    let key = font as *const lopdf::Dictionary as usize;
                    preparation.fonts.entry(key).or_insert(font);
                    if preparation.fonts.len() > MAX_INVOKED_FONTS_PER_PAGE {
                        return Err(invalid_pdf(
                            "public source PDF invoked too many fonts on one page",
                        ));
                    }
                }
            }
            "BDC"
                if operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    == Some(b"OC") =>
            {
                if operation.operands.len() != 2 {
                    return Err(invalid_pdf(
                        "public source PDF has an invalid BDC operation",
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn prepare_form<'a>(
    preparation: &mut Preparation<'a>,
    key: FormKey,
    stream: &'a lopdf::Stream,
    parent_depth: usize,
) -> GeminiInteractionsResult<()> {
    if preparation.active_forms.contains(&key) {
        return Err(invalid_pdf(
            "public source PDF Form XObject graph contains a cycle",
        ));
    }
    if preparation.forms.contains_key(&key) {
        return Ok(());
    }
    if preparation.forms.len() >= MAX_INVOKED_FORM_XOBJECTS_PER_PAGE {
        return Err(invalid_pdf(
            "public source PDF invoked too many distinct Form XObjects on one page",
        ));
    }
    if stream.dict.get(b"FormType").is_ok() {
        let form_type = stream
            .dict
            .get_deref(b"FormType", preparation.document)
            .map_err(|_| {
                invalid_pdf("public source PDF Form XObject FormType could not be resolved")
            })?;
        if form_type.as_i64().ok() != Some(1) {
            return Err(invalid_pdf(
                "public source PDF Form XObject has an invalid FormType",
            ));
        }
    }
    if stream.dict.get(b"Ref").is_ok() {
        return Err(invalid_pdf(
            "public source PDF reference Form XObjects are unsupported",
        ));
    }
    let matrix = optional_transform(preparation.document, &stream.dict, b"Matrix")?;
    if matrix.inverse().is_none() {
        return Err(invalid_pdf(
            "public source PDF Form XObject Matrix is singular",
        ));
    }
    let bounds = form_bounds(preparation.document, &stream.dict)?;
    let resources = form_resource_scope(preparation, stream)?;
    if preparation.remaining_decompressed_bytes == 0 {
        return Err(invalid_pdf(
            "public source PDF Form XObjects exceeded the page decompression budget",
        ));
    }
    let data = strict_stream_content(
        stream,
        preparation.remaining_decompressed_bytes,
        "Form XObject content stream",
    )?;
    preparation.remaining_decompressed_bytes = preparation
        .remaining_decompressed_bytes
        .checked_sub(data.len())
        .ok_or_else(|| {
            invalid_pdf("public source PDF Form XObjects exceeded the page decompression budget")
        })?;
    let content =
        Rc::new(Content::decode_strict(&data).map_err(|_| {
            invalid_pdf("public source PDF Form XObject content could not be decoded")
        })?);
    let proof_visibility_supported = bounds.left < bounds.right
        && bounds.bottom < bounds.top
        && stream.dict.get(b"OC").is_err()
        && stream.dict.get(b"Group").is_err();
    preparation.forms.insert(
        key,
        PreparedForm {
            content: Rc::clone(&content),
            resources: resources.clone(),
            matrix,
            bounds,
            proof_visibility_supported,
        },
    );
    preparation.active_forms.insert(key);
    let result = prepare_content(preparation, &content, &resources, parent_depth + 1);
    preparation.active_forms.remove(&key);
    result
}

fn load_page_text<'a>(
    document: &'a Document,
    page_id: ObjectId,
    max_decompressed_bytes: usize,
) -> GeminiInteractionsResult<PageText<'a>> {
    let content_data = strict_page_content(document, page_id, max_decompressed_bytes)?;
    let content = Content::decode_strict(&content_data)
        .map_err(|_| invalid_pdf("public source PDF page content could not be decoded"))?;
    let root_resources = page_resource_scope(document, page_id)?;
    let mut preparation = Preparation {
        document,
        root_resources: root_resources.clone(),
        remaining_decompressed_bytes: max_decompressed_bytes.saturating_sub(content_data.len()),
        fonts: BTreeMap::new(),
        forms: HashMap::new(),
        active_forms: HashSet::new(),
        invoked_xobjects: HashSet::new(),
    };
    prepare_content(&mut preparation, &content, &root_resources, 0)?;
    let per_font_budget = if preparation.fonts.is_empty() {
        preparation.remaining_decompressed_bytes
    } else {
        preparation.remaining_decompressed_bytes / preparation.fonts.len()
    };
    let mut encodings = HashMap::new();
    let mut metrics = HashMap::new();
    for (key, font) in preparation.fonts {
        if per_font_budget == 0 {
            return Err(invalid_pdf(
                "public source PDF font decoding exhausted the page decompression budget",
            ));
        }
        if let Ok(encoding) = strict_font_encoding(document, font, per_font_budget) {
            encodings.insert(key, encoding);
            metrics.insert(key, font_metrics(document, font)?);
        }
    }
    Ok(PageText {
        document,
        root_resources,
        encodings,
        font_metrics: metrics,
        forms: preparation.forms,
        content,
        display_transform: page_display_transform(document, page_id)?,
    })
}

#[derive(Clone)]
struct GraphicsState {
    transform: Transform,
    font_key: Option<usize>,
    font_size: f64,
    leading: f64,
    character_spacing: f64,
    word_spacing: f64,
    horizontal_scaling: f64,
    text_rise: f64,
    text_rendering_mode: i64,
    proof_visibility_supported: bool,
    clip_constraints: Vec<FormClip>,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            transform: Transform::IDENTITY,
            font_key: None,
            font_size: 0.0,
            leading: 0.0,
            character_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scaling: 1.0,
            text_rise: 0.0,
            text_rendering_mode: 0,
            proof_visibility_supported: true,
            clip_constraints: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct FormClip {
    page_to_form: Transform,
    bounds: FormBounds,
}

#[derive(Clone, Copy)]
struct PageBounds {
    left: f64,
    bottom: f64,
    right: f64,
    top: f64,
}

impl PageBounds {
    fn include(&mut self, x: f64, y: f64) {
        self.left = self.left.min(x);
        self.bottom = self.bottom.min(y);
        self.right = self.right.max(x);
        self.top = self.top.max(y);
    }

    fn corners(self) -> [(f64, f64); 4] {
        [
            (self.left, self.bottom),
            (self.left, self.top),
            (self.right, self.bottom),
            (self.right, self.top),
        ]
    }
}

struct TextGeometry {
    painted: PageBounds,
    advance: f64,
}

struct VisitState {
    sequence: usize,
    decoded_text_bytes: usize,
    xobject_invocations: usize,
    graphics_depth: usize,
}

fn bounds_are_inside_clips(bounds: PageBounds, clips: &[FormClip]) -> bool {
    clips.iter().all(|clip| {
        bounds.corners().into_iter().all(|(page_x, page_y)| {
            let (form_x, form_y) = clip.page_to_form.point(page_x, page_y);
            clip.bounds.contains(form_x, form_y)
        })
    })
}

fn text_geometry(
    metrics: &FontMetrics,
    operands: &[Object],
    graphics: &GraphicsState,
    text_to_page: Transform,
) -> Option<TextGeometry> {
    let font_bounds = metrics.bounds?;
    let mut cursor = 0.0;
    let mut painted = PageBounds {
        left: f64::INFINITY,
        bottom: f64::INFINITY,
        right: f64::NEG_INFINITY,
        top: f64::NEG_INFINITY,
    };
    let mut has_glyph = false;
    let mut visit_string = |bytes: &[u8], cursor: &mut f64| -> Option<()> {
        for byte in bytes {
            let width = metrics.widths[*byte as usize]?;
            let x_scale = graphics.font_size * graphics.horizontal_scaling / 1_000.0;
            let y_scale = graphics.font_size / 1_000.0;
            for (x, y) in [
                (font_bounds.left, font_bounds.bottom),
                (font_bounds.left, font_bounds.top),
                (font_bounds.right, font_bounds.bottom),
                (font_bounds.right, font_bounds.top),
            ] {
                let (page_x, page_y) =
                    text_to_page.point(*cursor + x * x_scale, graphics.text_rise + y * y_scale);
                if !page_x.is_finite() || !page_y.is_finite() {
                    return None;
                }
                painted.include(page_x, page_y);
            }
            has_glyph = true;
            let spacing = graphics.character_spacing
                + if *byte == b' ' {
                    graphics.word_spacing
                } else {
                    0.0
                };
            *cursor +=
                (width * graphics.font_size / 1_000.0 + spacing) * graphics.horizontal_scaling;
            if !cursor.is_finite() {
                return None;
            }
        }
        Some(())
    };
    for operand in operands {
        match operand {
            Object::String(bytes, _) => visit_string(bytes, &mut cursor)?,
            Object::Array(values) => {
                for value in values {
                    match value {
                        Object::String(bytes, _) => visit_string(bytes, &mut cursor)?,
                        value => {
                            let adjustment = number(value)?;
                            cursor += -adjustment * graphics.font_size / 1_000.0
                                * graphics.horizontal_scaling;
                            if !cursor.is_finite() {
                                return None;
                            }
                        }
                    }
                }
            }
            _ => return None,
        }
    }
    has_glyph.then_some(TextGeometry {
        painted,
        advance: cursor,
    })
}

fn visit_content<F>(
    page: &PageText<'_>,
    content: &Content<Vec<lopdf::content::Operation>>,
    resources: &ResourceScope<'_>,
    mut graphics: GraphicsState,
    clips: &[FormClip],
    inherited_content_visibility: bool,
    state: &mut VisitState,
    max_decoded_text_bytes: usize,
    visitor: &mut F,
) -> GeminiInteractionsResult<()>
where
    F: FnMut(TextFragment),
{
    let mut position = TextPosition::default();
    let mut graphics_stack = Vec::<GraphicsState>::new();
    let mut marked_visibility_stack = Vec::<bool>::new();
    let mut content_visibility_supported = inherited_content_visibility;
    let mut path_rectangles = Vec::<FormClip>::new();
    let mut path_supported = true;
    let mut clip_pending = false;
    let mut in_text_object = false;
    for operation in &content.operations {
        match operation.operator.as_str() {
            "q" => {
                if in_text_object {
                    return Err(invalid_pdf(
                        "public source PDF changes graphics scope inside a text object",
                    ));
                }
                if state.graphics_depth >= MAX_GRAPHICS_STATE_DEPTH {
                    return Err(invalid_pdf(
                        "public source PDF graphics-state depth exceeded its bound",
                    ));
                }
                state.graphics_depth += 1;
                graphics_stack.push(graphics.clone());
            }
            "Q" => {
                if in_text_object {
                    return Err(invalid_pdf(
                        "public source PDF changes graphics scope inside a text object",
                    ));
                }
                graphics = graphics_stack.pop().ok_or_else(|| {
                    invalid_pdf("public source PDF graphics-state stack underflowed")
                })?;
                state.graphics_depth = state.graphics_depth.saturating_sub(1);
            }
            "cm" => {
                let next = transform(&operation.operands).ok_or_else(|| {
                    invalid_pdf("public source PDF contains an invalid coordinate transform")
                })?;
                graphics.transform = graphics.transform.concatenate(next);
            }
            "BT" => {
                if in_text_object {
                    return Err(invalid_pdf(
                        "public source PDF contains nested text objects",
                    ));
                }
                in_text_object = true;
                position = TextPosition::default();
            }
            "ET" => {
                if !in_text_object {
                    return Err(invalid_pdf(
                        "public source PDF closes a missing text object",
                    ));
                }
                in_text_object = false;
            }
            "Tf" => {
                let name = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    .ok_or_else(|| invalid_pdf("public source PDF has an invalid Tf operation"))?;
                let size =
                    operation.operands.get(1).and_then(number).ok_or_else(|| {
                        invalid_pdf("public source PDF has an invalid Tf operation")
                    })?;
                let font = font_dictionary(page.document, resources, name)?;
                let font_key = font as *const lopdf::Dictionary as usize;
                if operation.operands.len() != 2 || !size.is_finite() {
                    return Err(invalid_pdf("public source PDF has an invalid Tf operation"));
                }
                graphics.font_key = Some(font_key);
                graphics.font_size = size;
            }
            "Tm" => {
                let next = transform(&operation.operands).ok_or_else(|| {
                    invalid_pdf("public source PDF contains an invalid text matrix")
                })?;
                position.current = next;
                position.line = next;
                position.known = true;
                position.line_known = true;
            }
            "Td" | "TD" => {
                let (Some(x), Some(y)) = (
                    operation.operands.first().and_then(number),
                    operation.operands.get(1).and_then(number),
                ) else {
                    return Err(invalid_pdf(
                        "public source PDF contains an invalid text-line displacement",
                    ));
                };
                if operation.operands.len() != 2 || !x.is_finite() || !y.is_finite() {
                    return Err(invalid_pdf(
                        "public source PDF contains an invalid text-line displacement",
                    ));
                }
                if operation.operator == "TD" {
                    graphics.leading = -y;
                }
                let translation = Transform {
                    e: x,
                    f: y,
                    ..Transform::IDENTITY
                };
                position.line = position.line.concatenate(translation);
                position.current = position.line;
                position.known = true;
                position.line_known = true;
            }
            "TL" => {
                let leading = operation.operands.first().and_then(number).ok_or_else(|| {
                    invalid_pdf("public source PDF contains invalid text leading")
                })?;
                if operation.operands.len() != 1 || !leading.is_finite() {
                    return Err(invalid_pdf(
                        "public source PDF contains invalid text leading",
                    ));
                }
                graphics.leading = leading;
            }
            "T*" => {
                let translation = Transform {
                    e: 0.0,
                    f: -graphics.leading,
                    ..Transform::IDENTITY
                };
                position.line = position.line.concatenate(translation);
                position.current = position.line;
                position.known = position.line_known;
            }
            "Tc" => {
                graphics.character_spacing =
                    number(&operation.operands[0]).expect("Tc was validated during preparation");
            }
            "Tw" => {
                graphics.word_spacing =
                    number(&operation.operands[0]).expect("Tw was validated during preparation");
            }
            "Tz" => {
                graphics.horizontal_scaling = number(&operation.operands[0])
                    .expect("Tz was validated during preparation")
                    / 100.0;
            }
            "Ts" => {
                graphics.text_rise =
                    number(&operation.operands[0]).expect("Ts was validated during preparation");
            }
            "Tr" => {
                graphics.text_rendering_mode = operation.operands[0]
                    .as_i64()
                    .expect("Tr operations were validated during page preparation");
            }
            "gs" => {
                let name = operation.operands[0]
                    .as_name()
                    .expect("gs operations were validated during page preparation");
                let ext_gstate = ext_gstate_dictionary(page.document, resources, name)?;
                graphics.proof_visibility_supported &=
                    ext_gstate_proof_visibility_supported(ext_gstate)?;
                if let Some((font, size)) = ext_gstate_font(page.document, ext_gstate)? {
                    graphics.font_key = Some(font as *const lopdf::Dictionary as usize);
                    graphics.font_size = size;
                }
            }
            "re" => {
                let values = operation
                    .operands
                    .iter()
                    .map(number)
                    .collect::<Option<Vec<_>>>()
                    .filter(|values| {
                        values.len() == 4 && values.iter().copied().all(f64::is_finite)
                    });
                let Some(values) = values else {
                    return Err(invalid_pdf(
                        "public source PDF contains an invalid rectangle path",
                    ));
                };
                let (x, y, width, height) = (values[0], values[1], values[2], values[3]);
                let page_to_path = graphics.transform.inverse().ok_or_else(|| {
                    invalid_pdf("public source PDF clipping transform is singular")
                })?;
                path_rectangles.push(FormClip {
                    page_to_form: page_to_path,
                    bounds: FormBounds {
                        left: x.min(x + width),
                        bottom: y.min(y + height),
                        right: x.max(x + width),
                        top: y.max(y + height),
                    },
                });
            }
            "m" | "l" | "c" | "v" | "y" | "h" => {
                path_supported = false;
            }
            "W" | "W*" => {
                if !operation.operands.is_empty() {
                    return Err(invalid_pdf(
                        "public source PDF contains an invalid clipping operation",
                    ));
                }
                clip_pending = true;
            }
            "n" | "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                if clip_pending {
                    if path_supported && path_rectangles.len() == 1 {
                        graphics.clip_constraints.push(path_rectangles[0]);
                    } else {
                        // Arbitrary clipping geometry cannot establish that
                        // later decoded text is visibly painted.
                        graphics.proof_visibility_supported = false;
                    }
                }
                path_rectangles.clear();
                path_supported = true;
                clip_pending = false;
            }
            "BMC" => {
                marked_visibility_stack.push(content_visibility_supported);
            }
            "BDC" => {
                marked_visibility_stack.push(content_visibility_supported);
                if operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    == Some(b"OC")
                {
                    content_visibility_supported = false;
                }
            }
            "EMC" => {
                content_visibility_supported = marked_visibility_stack.pop().ok_or_else(|| {
                    invalid_pdf("public source PDF marked-content stack underflowed")
                })?;
            }
            "Do" => {
                if in_text_object {
                    return Err(invalid_pdf(
                        "public source PDF invokes an XObject inside a text object",
                    ));
                }
                state.xobject_invocations =
                    state.xobject_invocations.checked_add(1).ok_or_else(|| {
                        invalid_pdf("public source PDF XObject invocation count overflowed")
                    })?;
                if state.xobject_invocations > MAX_XOBJECT_INVOCATIONS_PER_PAGE {
                    return Err(invalid_pdf(
                        "public source PDF invoked too many XObjects on one page",
                    ));
                }
                let name = operation.operands[0]
                    .as_name()
                    .expect("Do operations were validated during page preparation");
                let (key, stream) = resolved_xobject(page.document, resources, name)?;
                match validate_xobject_type(page.document, stream)? {
                    b"Image" => {}
                    b"Form" => {
                        if state.graphics_depth >= MAX_GRAPHICS_STATE_DEPTH {
                            return Err(invalid_pdf(
                                "public source PDF graphics-state depth exceeded its bound",
                            ));
                        }
                        let form = page.forms.get(&key).ok_or_else(|| {
                            invalid_pdf("public source PDF invoked an unprepared Form XObject")
                        })?;
                        let form_to_page = graphics.transform.concatenate(form.matrix);
                        let page_to_form = form_to_page.inverse().ok_or_else(|| {
                            invalid_pdf("public source PDF Form XObject Matrix is singular")
                        })?;
                        let mut child_graphics = graphics.clone();
                        child_graphics.transform = form_to_page;
                        child_graphics.proof_visibility_supported &=
                            form.proof_visibility_supported;
                        let mut child_clips = clips.to_vec();
                        child_clips.push(FormClip {
                            page_to_form,
                            bounds: form.bounds,
                        });
                        state.graphics_depth += 1;
                        let result = visit_content(
                            page,
                            &form.content,
                            &form.resources,
                            child_graphics,
                            &child_clips,
                            content_visibility_supported,
                            state,
                            max_decoded_text_bytes,
                            visitor,
                        );
                        state.graphics_depth = state.graphics_depth.saturating_sub(1);
                        result?;
                    }
                    _ => unreachable!("XObject subtype was validated during preparation"),
                }
            }
            "Tj" | "TJ" | "'" | "\"" => {
                if !in_text_object {
                    return Err(invalid_pdf(
                        "public source PDF paints text outside a text object",
                    ));
                }
                if operation.operator == "\"" {
                    graphics.word_spacing = number(&operation.operands[0])
                        .expect("double-quote operation was validated during preparation");
                    graphics.character_spacing = number(&operation.operands[1])
                        .expect("double-quote operation was validated during preparation");
                }
                if matches!(operation.operator.as_str(), "'" | "\"") {
                    let translation = Transform {
                        e: 0.0,
                        f: -graphics.leading,
                        ..Transform::IDENTITY
                    };
                    position.line = position.line.concatenate(translation);
                    position.current = position.line;
                    position.known = position.line_known;
                }
                let font_key = graphics.font_key.ok_or_else(|| {
                    invalid_pdf(
                        "public source PDF text uses a missing or undecodable resolved font",
                    )
                })?;
                let Some(encoding) = page.encodings.get(&font_key) else {
                    position.known = false;
                    continue;
                };
                let operands = if operation.operator == "\"" {
                    operation.operands.get(2..).unwrap_or_default()
                } else {
                    operation.operands.as_slice()
                };
                let mut text = String::new();
                decode_text_operands(encoding, operands, &mut text)
                    .map_err(|_| invalid_pdf("public source PDF text could not be decoded"))?;
                let text = normalize_text_row(&text);
                state.decoded_text_bytes = state
                    .decoded_text_bytes
                    .checked_add(text.len())
                    .ok_or_else(|| {
                        invalid_pdf("public source PDF decoded layout text exceeded its byte cap")
                    })?;
                if state.decoded_text_bytes > max_decoded_text_bytes {
                    return Err(invalid_pdf(
                        "public source PDF decoded layout text exceeded its byte cap",
                    ));
                }
                let page_transform = graphics.transform.concatenate(position.current);
                let visual_transform = page.display_transform.concatenate(page_transform);
                let geometry = page.font_metrics.get(&font_key).and_then(|metrics| {
                    text_geometry(metrics, operands, &graphics, page_transform)
                });
                let clip_geometry_supported = (clips.is_empty()
                    && graphics.clip_constraints.is_empty())
                    || geometry.as_ref().is_some_and(|geometry| {
                        bounds_are_inside_clips(geometry.painted, clips)
                            && bounds_are_inside_clips(geometry.painted, &graphics.clip_constraints)
                    });
                if !text.is_empty()
                    && position.known
                    && visual_transform.has_horizontal_baseline()
                    && graphics.font_size.abs() > f64::EPSILON
                    && graphics.proof_visibility_supported
                    && content_visibility_supported
                    && graphics.text_rendering_mode == 0
                    && clip_geometry_supported
                {
                    let (page_x, page_y) = page_transform.point(0.0, graphics.text_rise);
                    let (x, y) = page.display_transform.point(page_x, page_y);
                    visitor(TextFragment {
                        x,
                        y,
                        sequence: state.sequence,
                        text,
                    });
                    state.sequence = state.sequence.saturating_add(1);
                }
                if position.known {
                    if let Some(geometry) = geometry {
                        position.current = position.current.concatenate(Transform {
                            e: geometry.advance,
                            ..Transform::IDENTITY
                        });
                    } else {
                        position.known = false;
                    }
                } else {
                    position.known = false;
                }
            }
            _ => {}
        }
    }
    if in_text_object
        || !graphics_stack.is_empty()
        || !marked_visibility_stack.is_empty()
        || clip_pending
    {
        return Err(invalid_pdf(
            "public source PDF has an unbalanced text, graphics-state, or marked-content scope",
        ));
    }
    Ok(())
}

fn visit_text_fragments<F>(
    page: &PageText<'_>,
    max_decompressed_bytes: usize,
    mut visitor: F,
) -> GeminiInteractionsResult<()>
where
    F: FnMut(TextFragment),
{
    let mut state = VisitState {
        sequence: 0,
        decoded_text_bytes: 0,
        xobject_invocations: 0,
        graphics_depth: 0,
    };
    visit_content(
        page,
        &page.content,
        &page.root_resources,
        GraphicsState::default(),
        &[],
        true,
        &mut state,
        max_decompressed_bytes,
        &mut visitor,
    )
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
#[path = "pdf/tests.rs"]
mod tests;
