//! Bounded diagnostic for candidate opaque-paint equality proofs.
//!
//! Hashes are triage observations, not text-inventory or correspondence
//! certificates. The probe does not model text clipping, optional-content
//! execution or all graphics-state dependencies and never marks text complete.

use std::{
    collections::BTreeMap,
    env,
    fs::File,
    io::{self, Read},
    sync::Arc,
};

use pdfdelta_core::pdf::{LopdfParser, ParseLimits, ParsedPdf, PdfDict, PdfObject, PdfParser};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type ProbeResult<T> = Result<T, Box<dyn std::error::Error>>;

const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESOURCE_VISITS: usize = 100_000;
const MAX_ANNOTATIONS: usize = 64;
const MAX_FIELDS: usize = 256;
const MAX_FIELD_DEPTH: usize = 8;
const MAX_AP_DECODED_BYTES: usize = 1024 * 1024;
const MAX_AP_OPERATOR_BYTES: usize = 256 * 1024;
const MAX_AP_STREAMS: usize = 64;
const MAX_AP_TOTAL_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_PAGES: usize = 16;
const MAX_IMAGE_OPERATORS: usize = 100_000;
const MAX_IMAGE_OBJECTS: usize = 128;
const MAX_IMAGE_SMASKS: usize = 64;
const MAX_IMAGE_INVOCATIONS: usize = 4_096;
const MAX_IMAGE_NAME_BYTES: usize = 4_096;
const MAX_IMAGE_METADATA_NODES: usize = 4_096;
const MAX_IMAGE_FIELD_VALUES: usize = 256;
const MAX_IMAGE_SMALL_ARRAY: usize = 4;
/// Explicit aggregate decode bound for object streams in annotation mode. The
/// per-stream parse limit already caps every decoded stream at
/// `MAX_AP_DECODED_BYTES`; this bounds the total object-stream work as well.
const MAX_ANNOTATION_OBJECT_STREAM_BYTES: usize = 8 * 1024 * 1024;

/// Shared work and byte budget for the annotation scan. Exhaustion records an
/// explicit partial reason instead of silently dropping evidence.
struct AnnotationBudget {
    ap_streams_remaining: usize,
    ap_bytes_remaining: usize,
    incomplete: Vec<String>,
}

impl AnnotationBudget {
    fn new() -> Self {
        Self {
            ap_streams_remaining: MAX_AP_STREAMS,
            ap_bytes_remaining: MAX_AP_TOTAL_BYTES,
            incomplete: Vec::new(),
        }
    }

    fn note(&mut self, reason: &str) {
        if !self.incomplete.iter().any(|item| item == reason) {
            self.incomplete.push(reason.to_owned());
        }
    }

    fn spend_ap_stream(&mut self) -> bool {
        if self.ap_streams_remaining == 0 {
            self.note("ap_stream_visit_limit");
            return false;
        }
        self.ap_streams_remaining -= 1;
        true
    }

    fn spend_ap_bytes(&mut self, bytes: usize) -> bool {
        if bytes > self.ap_bytes_remaining {
            self.note("ap_decoded_total_byte_limit");
            return false;
        }
        self.ap_bytes_remaining -= bytes;
        true
    }
}

#[path = "paint_trace_probe/properties.rs"]
mod properties;

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn content_bytes(
    pdf: &dyn ParsedPdf,
    object: &PdfObject,
    bytes: &mut Vec<u8>,
    depth: usize,
) -> ProbeResult<()> {
    if depth > 32 {
        return Err("content reference depth limit".into());
    }
    match object {
        PdfObject::Null => {}
        PdfObject::Array(items) => {
            for item in items {
                content_bytes(pdf, item, bytes, depth + 1)?;
            }
        }
        PdfObject::Reference(reference) => {
            let resolved = pdf.resolve_with_terminal(*reference)?;
            if matches!(resolved.object, PdfObject::Stream(_)) {
                let decoded = pdf.decoded_stream(resolved.reference)?;
                if bytes.len().saturating_add(decoded.bytes.len()) >= MAX_PAGE_BYTES {
                    return Err("page content byte limit".into());
                }
                bytes.extend(decoded.bytes);
                bytes.push(b'\n');
            } else {
                content_bytes(pdf, &resolved.object, bytes, depth + 1)?;
            }
        }
        _ => return Err("unsupported page content shape".into()),
    }
    Ok(())
}

fn resource_digest(
    pdf: &dyn ParsedPdf,
    object: &PdfObject,
    depth: usize,
    remaining: &mut usize,
) -> ProbeResult<String> {
    if depth > 32 || *remaining == 0 {
        return Err("resource closure work/depth limit (including cycles)".into());
    }
    *remaining -= 1;
    let value = match object {
        PdfObject::Reference(reference) => {
            let resolved = pdf.resolve_with_terminal(*reference)?;
            if matches!(resolved.object, PdfObject::Stream(_)) {
                let raw = pdf.raw_stream(resolved.reference)?;
                let dictionary = resource_digest(
                    pdf,
                    &PdfObject::Dictionary(raw.dictionary),
                    depth + 1,
                    remaining,
                )?;
                json!(["stream", dictionary, digest(&raw.bytes)])
            } else {
                return resource_digest(pdf, &resolved.object, depth + 1, remaining);
            }
        }
        PdfObject::Array(items) => {
            let items = items
                .iter()
                .map(|item| resource_digest(pdf, item, depth + 1, remaining))
                .collect::<ProbeResult<Vec<_>>>()?;
            json!(["array", items])
        }
        PdfObject::Dictionary(items) => {
            let items = items
                .iter()
                .map(|(key, value)| {
                    Ok(json!([
                        key,
                        resource_digest(pdf, value, depth + 1, remaining)?
                    ]))
                })
                .collect::<ProbeResult<Vec<_>>>()?;
            json!(["dictionary", items])
        }
        PdfObject::Stream(_) => return Err("direct stream has no readable source reference".into()),
        value => json!(["primitive", format!("{value:?}")]),
    };
    Ok(digest(&serde_json::to_vec(&value)?))
}

/// Parser-library operations remain inside this diagnostic adapter. Retained
/// state operators make equality conservative; omitted text operators mean
/// that equality is still insufficient to certify opaque text unchanged.
fn trace(bytes: &[u8], include_program: bool) -> ProbeResult<Value> {
    let content = lopdf::content::Content::decode_strict(bytes)?;
    if content.operations.len() > 100_000 {
        return Err("page operator count limit".into());
    }
    let mut counts = BTreeMap::new();
    let mut tags = BTreeMap::new();
    let mut retained = Vec::new();
    let mut text_clip = false;
    let mut marked_stack = Vec::new();
    let mut declarations = Vec::new();
    let mut paint_properties = Vec::new();
    let mut unbalanced_markers = false;
    for (index, operation) in content.operations.iter().enumerate() {
        *counts.entry(operation.operator.as_str()).or_insert(0_u32) += 1;
        if operation.operator == "Tr" {
            text_clip |= operation
                .operands
                .first()
                .and_then(|value| value.as_i64().ok())
                .is_none_or(|mode| !(0..4).contains(&mode));
        }
        if matches!(operation.operator.as_str(), "BMC" | "BDC") {
            if marked_stack.len() >= 64 {
                return Err("marked-content depth limit".into());
            }
            let tag = operation.operands.first().map(|value| format!("{value:?}"));
            *tags.entry(tag.unwrap_or_default()).or_insert(0_u32) += 1;
            let artifact = matches!(operation.operands.first(), Some(lopdf::Object::Name(name)) if name == b"Artifact");
            let property = operation.operands.get(1);
            let named = matches!(property, Some(lopdf::Object::Name(_)));
            let actual = match property {
                Some(lopdf::Object::Dictionary(dictionary)) => dictionary.get(b"ActualText").ok(),
                _ => None,
            };
            let declaration = actual.map(|value| {
                let position = declarations.len();
                declarations.push(json!({
                    "operator_index": index,
                    "value": format!("{value:?}"),
                    "string_value": matches!(value, lopdf::Object::String(_, _)),
                }));
                position
            });
            marked_stack.push((artifact, named, declaration));
        } else if operation.operator == "EMC" && marked_stack.pop().is_none() {
            unbalanced_markers = true;
        }
        if matches!(
            operation.operator.as_str(),
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "sh" | "Do" | "BI"
        ) {
            paint_properties.push(json!({
                "operator_index": index,
                "operator": operation.operator,
                "artifact_tag": marked_stack.iter().any(|frame| frame.0),
                "named_property_unresolved": marked_stack.iter().any(|frame| frame.1),
                "inline_actual_text_declarations": marked_stack.iter().filter_map(|frame| frame.2).collect::<Vec<_>>(),
            }));
        }
        if !matches!(
            operation.operator.as_str(),
            "BT" | "ET"
                | "Tc"
                | "Tw"
                | "Tz"
                | "TL"
                | "Tf"
                | "Ts"
                | "Td"
                | "TD"
                | "Tm"
                | "T*"
                | "Tj"
                | "TJ"
                | "'"
                | "\""
        ) {
            retained.push(operation.clone());
        }
    }
    let encoded = lopdf::content::Content {
        operations: &retained,
    }
    .encode()?;
    let without_markers = lopdf::content::Content {
        operations: retained
            .into_iter()
            .filter(|operation| !matches!(operation.operator.as_str(), "BMC" | "BDC" | "EMC"))
            .collect::<Vec<_>>(),
    }
    .encode()?;
    let mut result = json!({
        "operation_counts": counts,
        "marked_content_tags": tags,
        "inline_actual_text_declarations": declarations,
        "paint_marked_properties": paint_properties,
        "unbalanced_marked_content": unbalanced_markers || !marked_stack.is_empty(),
        "marked_property_scope": "page-program-only; named properties, structure dictionaries and invoked Form programs are not resolved",
        "text_clip_or_invalid_mode_seen": text_clip,
        "without_text_operators_sha256": digest(&encoded),
        "without_text_operators_bytes": encoded.len(),
        "without_text_or_markers_sha256": digest(&without_markers),
        "without_text_or_markers_bytes": without_markers.len(),
    });
    if include_program {
        result["operation_trace"] = json!(
            content
                .operations
                .iter()
                .map(|operation| operation.operator.as_str())
                .collect::<Vec<_>>()
        );
        result["without_text_or_markers_program"] = json!(String::from_utf8(without_markers)?);
    }
    Ok(result)
}

/// Probe modes share one bounded metadata/form scan. Page programs are only
/// decoded in `Full`; `DeclarationsOnly` exists because page-program scanning
/// is unrelated to trailer-reachable source declarations.
enum Mode {
    Full {
        selected_page: Option<usize>,
    },
    DeclarationsOnly,
    /// Parsed page operators with operand text for exact external geometry
    /// analysis. Values are strings so decimal text is not re-rounded.
    Operators {
        page: usize,
    },
    /// Page annotations and `AcroForm` fields for direct provenance checks.
    Annotations {
        page: usize,
    },
    /// Executed `Do` invocations per page with bounded image dictionary
    /// observations, for direct image provenance checks.
    Images {
        pages: Vec<usize>,
    },
}

fn parse_args(args: &[String]) -> ProbeResult<(String, Mode)> {
    const USAGE: &str = "usage: paint_trace_probe [--declarations-only|--operators PAGE|--annotations PAGE|--images PAGE,..] INPUT.pdf [DUMP_ZERO_BASED_PAGE]";
    if args.first().is_some_and(|arg| arg == "--declarations-only") {
        return match args {
            [_, path] => Ok((path.clone(), Mode::DeclarationsOnly)),
            _ => Err(USAGE.into()),
        };
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--operators" || arg == "--annotations")
    {
        return match args {
            [flag, page, path] if flag == "--operators" => Ok((
                path.clone(),
                Mode::Operators {
                    page: page.parse::<usize>()?,
                },
            )),
            [flag, page, path] if flag == "--annotations" => Ok((
                path.clone(),
                Mode::Annotations {
                    page: page.parse::<usize>()?,
                },
            )),
            _ => Err(USAGE.into()),
        };
    }
    if args.first().is_some_and(|arg| arg == "--images") {
        return match args {
            [_, pages, path] => {
                let pages = pages
                    .split(',')
                    .map(str::parse::<usize>)
                    .collect::<Result<Vec<_>, _>>()?;
                if pages.is_empty() || pages.len() > MAX_IMAGE_PAGES {
                    return Err("image mode requires 1..=16 zero-based pages".into());
                }
                Ok((path.clone(), Mode::Images { pages }))
            }
            _ => Err(USAGE.into()),
        };
    }
    match args {
        [path] if !path.starts_with("--") => Ok((
            path.clone(),
            Mode::Full {
                selected_page: None,
            },
        )),
        [path, page] if !path.starts_with("--") => Ok((
            path.clone(),
            Mode::Full {
                selected_page: Some(page.parse::<usize>()?),
            },
        )),
        _ => Err(USAGE.into()),
    }
}

fn operand_json(object: &lopdf::Object, depth: usize) -> Value {
    if depth > 8 {
        return json!({"unsupported": "depth"});
    }
    match object {
        lopdf::Object::Null => json!({"null": true}),
        lopdf::Object::Boolean(value) => json!({"boolean": value}),
        lopdf::Object::Integer(value) => json!({"number": value.to_string()}),
        lopdf::Object::Real(value) => json!({"number": value.to_string()}),
        lopdf::Object::Name(value) => {
            json!({"name": String::from_utf8_lossy(value).into_owned()})
        }
        lopdf::Object::String(value, _) => json!({
            "string": String::from_utf8_lossy(value).into_owned(), "bytes": value.len()
        }),
        lopdf::Object::Array(items) => json!({
            "array": items.iter().take(64).map(|item| operand_json(item, depth + 1)).collect::<Vec<_>>()
        }),
        lopdf::Object::Dictionary(dictionary) => json!({
            "dictionary": dictionary
                .iter()
                .take(64)
                .map(|(key, value)| (
                    String::from_utf8_lossy(key).into_owned(),
                    operand_json(value, depth + 1)
                ))
                .collect::<serde_json::Map<_, _>>()
        }),
        lopdf::Object::Reference(_) | lopdf::Object::Stream(_) => {
            json!({"unsupported": "reference"})
        }
    }
}

/// Parsed operator text for external exact analysis. Operand numbers keep the
/// parser's shortest round-trip decimal text instead of binary floats.
fn operators_json(bytes: &[u8]) -> ProbeResult<Value> {
    let content = lopdf::content::Content::decode_strict(bytes)?;
    if content.operations.len() > 100_000 {
        return Err("page operator count limit".into());
    }
    let operators = content
        .operations
        .iter()
        .enumerate()
        .map(|(index, operation)| {
            json!({
                "index": index,
                "operator": operation.operator,
                "operands": operation
                    .operands
                    .iter()
                    .map(|operand| operand_json(operand, 0))
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"operator_count": operators.len(), "operators": operators}))
}

fn dereference(pdf: &dyn ParsedPdf, object: &PdfObject) -> ProbeResult<PdfObject> {
    match object {
        PdfObject::Reference(reference) => Ok(pdf.resolve(*reference)?),
        value => Ok(value.clone()),
    }
}

fn as_dictionary(pdf: &dyn ParsedPdf, object: &PdfObject) -> ProbeResult<Option<PdfDict>> {
    match dereference(pdf, object)? {
        PdfObject::Dictionary(items) => Ok(Some(items)),
        _ => Ok(None),
    }
}

/// PDF text strings: UTF-16 with a byte-order mark, otherwise lossy UTF-8.
/// `PDFDocEncoding` without a mark is not decoded.
fn text_from_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE_u8, 0xFF]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF_u8, 0xFE]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn object_text(object: &PdfObject) -> String {
    match object {
        PdfObject::Integer(value) => value.to_string(),
        PdfObject::Real(value) => value.to_string(),
        PdfObject::Name(value) | PdfObject::String(value) => text_from_bytes(value),
        value => format!("{value:?}"),
    }
}

fn rect_json(pdf: &dyn ParsedPdf, object: &PdfObject) -> ProbeResult<Value> {
    let resolved = dereference(pdf, object)?;
    let PdfObject::Array(items) = resolved else {
        return Ok(Value::Null);
    };
    Ok(json!(items.iter().map(object_text).collect::<Vec<_>>()))
}

/// Bounded appearance-stream dump. The annotation-mode parse limit caps every
/// decoded stream at `MAX_AP_DECODED_BYTES` (scratch), and the retained
/// aggregate is `MAX_AP_TOTAL_BYTES`; when the aggregate is exhausted no
/// further decode call is made. Decode errors and every budget stop are
/// recorded, and parsed operators are only produced within the operator byte
/// limit.
fn stream_json(
    pdf: &dyn ParsedPdf,
    reference: pdfdelta_core::pdf::ObjectRef,
    budget: &mut AnnotationBudget,
) -> ProbeResult<Value> {
    if !budget.spend_ap_stream() {
        return Ok(json!({"skipped": "ap_stream_visit_limit"}));
    }
    if budget.ap_bytes_remaining == 0 {
        return Ok(json!({"skipped": "ap_decoded_total_byte_limit"}));
    }
    let resolved = pdf.resolve_with_terminal(reference)?;
    let PdfObject::Stream(dictionary) = resolved.object else {
        return Ok(json!({"skipped": "resolved reference is not a stream"}));
    };
    let declared_length = dictionary
        .get(b"Length".as_slice())
        .and_then(|value| match value {
            PdfObject::Integer(length) => usize::try_from(*length).ok(),
            _ => None,
        });
    if declared_length.is_some_and(|length| length > MAX_AP_DECODED_BYTES) {
        return Ok(json!({"skipped": "declared_length_over_decode_limit"}));
    }
    let raw = pdf.raw_stream(resolved.reference)?;
    if raw.bytes.len() > MAX_AP_DECODED_BYTES {
        return Ok(json!({"skipped": "raw_bytes_over_decode_limit"}));
    }
    let decoded = match pdf.decoded_stream(resolved.reference) {
        Ok(decoded) => decoded,
        Err(error) => {
            budget.note("ap_decode_failed");
            return Ok(json!({
                "decode_status": "failed", "reason": error.to_string(),
            }));
        }
    };
    if !budget.spend_ap_bytes(decoded.bytes.len()) {
        return Ok(json!({"skipped": "ap_decoded_total_byte_limit"}));
    }
    let mut value = json!({
        "bytes": decoded.bytes.len(),
        "sha256": digest(&decoded.bytes),
    });
    if decoded.bytes.len() > MAX_AP_OPERATOR_BYTES {
        value["operators_skipped"] = json!("operator byte limit");
    } else {
        match operators_json(&decoded.bytes) {
            Ok(operators) => value["operators"] = operators,
            Err(error) => value["operators_error"] = json!(error.to_string()),
        }
    }
    Ok(value)
}

/// `/N`, `/R` and `/D` are resolved by shape: a stream reference is dumped, a
/// dictionary (direct or indirect) is treated as an appearance state map, and
/// any other shape records a reason instead of guessing.
fn appearance_json(
    pdf: &dyn ParsedPdf,
    object: &PdfObject,
    budget: &mut AnnotationBudget,
) -> ProbeResult<Value> {
    let Some(ap) = as_dictionary(pdf, object)? else {
        return Ok(json!({"unsupported": "AP is not a dictionary"}));
    };
    let mut result = serde_json::Map::new();
    for key in [b"N".as_slice(), b"R", b"D"] {
        let Some(value) = ap.get(key) else {
            continue;
        };
        let label = String::from_utf8_lossy(key).into_owned();
        match value {
            PdfObject::Reference(reference) => {
                let resolved = pdf.resolve(*reference)?;
                match resolved {
                    PdfObject::Stream(_) => {
                        result.insert(label, stream_json(pdf, *reference, budget)?);
                    }
                    PdfObject::Dictionary(states) => {
                        result.insert(label, state_map_json(pdf, &states, budget)?);
                    }
                    _ => {
                        result.insert(label, json!({"unsupported": "AP key shape"}));
                    }
                }
            }
            PdfObject::Dictionary(states) => {
                result.insert(label, state_map_json(pdf, states, budget)?);
            }
            PdfObject::Stream(_) => {
                result.insert(label, json!({"unsupported": "direct appearance stream"}));
            }
            _ => {
                result.insert(label, json!({"unsupported": "AP key shape"}));
            }
        }
    }
    Ok(Value::Object(result))
}

fn state_map_json(
    pdf: &dyn ParsedPdf,
    states: &PdfDict,
    budget: &mut AnnotationBudget,
) -> ProbeResult<Value> {
    let mut state_map = serde_json::Map::new();
    for (state, stream) in states {
        let label = String::from_utf8_lossy(state).into_owned();
        match stream {
            PdfObject::Reference(reference) => {
                state_map.insert(label, stream_json(pdf, *reference, budget)?);
            }
            _ => {
                state_map.insert(
                    label,
                    json!({"unsupported": "appearance state is not an indirect stream"}),
                );
            }
        }
    }
    Ok(Value::Object(state_map))
}

fn field_json(
    pdf: &dyn ParsedPdf,
    object: &PdfObject,
    prefix: &str,
    depth: usize,
    remaining: &mut usize,
    output: &mut Vec<Value>,
    budget: &mut AnnotationBudget,
) -> ProbeResult<()> {
    if depth > MAX_FIELD_DEPTH {
        budget.note("field_depth_limit");
        return Ok(());
    }
    if *remaining == 0 {
        budget.note("field_visit_limit");
        return Ok(());
    }
    let Some(items) = as_dictionary(pdf, object)? else {
        return Ok(());
    };
    *remaining -= 1;
    let name = match items.get(b"T".as_slice()) {
        Some(PdfObject::String(bytes)) => {
            let text = text_from_bytes(bytes);
            if prefix.is_empty() {
                text
            } else {
                format!("{prefix}.{text}")
            }
        }
        _ => prefix.to_owned(),
    };
    let mut row = json!({"name": name});
    if let Some(field_type) = items.get(b"FT".as_slice()) {
        row["field_type"] = json!(object_text(field_type));
    }
    if let Some(rect) = items.get(b"Rect".as_slice()) {
        row["rect"] = rect_json(pdf, rect)?;
    }
    if let Some(ap) = items.get(b"AP".as_slice()) {
        row["appearance"] = appearance_json(pdf, ap, budget)?;
    }
    if let Some(kids) = items.get(b"Kids".as_slice()) {
        let kids = dereference(pdf, kids)?;
        if let PdfObject::Array(children) = kids {
            for child in children.iter().take(MAX_FIELDS) {
                field_json(pdf, child, &name, depth + 1, remaining, output, budget)?;
            }
        }
    }
    output.push(row);
    Ok(())
}

/// Page `/Annots` and catalog `/AcroForm` fields with bounded appearance
/// operator dumps, for direct provenance checks between PDF structure and XFA
/// declarations. Inherited field attributes and `/Pgo` are not resolved.
/// Resolves the trailer `/Root` catalog and records every failure mode instead
/// of silently treating a missing catalog as absent structure.
fn catalog_dictionary(pdf: &dyn ParsedPdf, budget: &mut AnnotationBudget) -> Option<PdfDict> {
    let trailer = match pdf.trailer() {
        Ok(trailer) => trailer,
        Err(error) => {
            budget.note("trailer_resolution_failed");
            eprintln!("trailer resolution failed: {error}");
            return None;
        }
    };
    let Some(root) = trailer.get(b"Root".as_slice()) else {
        budget.note("catalog_root_missing");
        return None;
    };
    match dereference(pdf, root) {
        Ok(PdfObject::Dictionary(catalog)) => Some(catalog),
        Ok(_) => {
            budget.note("catalog_root_not_dictionary");
            None
        }
        Err(error) => {
            budget.note("catalog_resolution_failed");
            eprintln!("catalog resolution failed: {error}");
            None
        }
    }
}

fn annotations_json(pdf: &dyn ParsedPdf, page: usize) -> ProbeResult<Value> {
    let pages = pdf.pages()?;
    let reference = pages.get(page).ok_or("diagnostic page limit")?;
    let snapshot = pdf.page_snapshot(*reference)?;
    let mut budget = AnnotationBudget::new();
    let mut annotations = Vec::new();
    if let Some(annots) = snapshot.dictionary.get(b"Annots".as_slice()) {
        let annots = dereference(pdf, annots)?;
        if let PdfObject::Array(items) = annots {
            if items.len() > MAX_ANNOTATIONS {
                budget.note("annotation_truncated");
            }
            for (index, item) in items.iter().take(MAX_ANNOTATIONS).enumerate() {
                let Some(items) = as_dictionary(pdf, item)? else {
                    annotations.push(json!({"index": index, "unsupported": "non-dictionary"}));
                    continue;
                };
                let mut row = json!({"index": index});
                if let Some(subtype) = items.get(b"Subtype".as_slice()) {
                    row["subtype"] = json!(object_text(subtype));
                }
                if let Some(name) = items.get(b"T".as_slice()) {
                    row["field_name"] = json!(object_text(name));
                }
                if let Some(field_type) = items.get(b"FT".as_slice()) {
                    row["field_type"] = json!(object_text(field_type));
                }
                if let Some(rect) = items.get(b"Rect".as_slice()) {
                    row["rect"] = rect_json(pdf, rect)?;
                }
                if let Some(ap) = items.get(b"AP".as_slice()) {
                    row["appearance"] = appearance_json(pdf, ap, &mut budget)?;
                }
                annotations.push(row);
            }
        }
    }
    let catalog = catalog_dictionary(pdf, &mut budget);
    let mut fields = Vec::new();
    let mut struct_tree_root = false;
    if let Some(catalog) = &catalog {
        struct_tree_root = catalog.contains_key(b"StructTreeRoot".as_slice());
        if let Some(acroform) = catalog.get(b"AcroForm".as_slice())
            && let Some(acroform) = as_dictionary(pdf, acroform)?
            && let Some(fields_object) = acroform.get(b"Fields".as_slice())
        {
            let resolved = dereference(pdf, fields_object)?;
            if let PdfObject::Array(items) = resolved {
                let mut remaining = MAX_FIELDS;
                for item in &items {
                    field_json(pdf, item, "", 0, &mut remaining, &mut fields, &mut budget)?;
                }
            }
        }
    }
    let partial = !budget.incomplete.is_empty();
    Ok(json!({
        "annotations": annotations,
        "acroform_fields": fields,
        "catalog_struct_tree_root": struct_tree_root,
        "page_struct_parents": snapshot.dictionary.contains_key(b"StructParents".as_slice()),
        "partial": partial,
        "incomplete_reasons": budget.incomplete,
        "scope": "page /Annots and catalog /AcroForm with bounded appearance dumps; inherited attributes and /Pgo are not resolved",
    }))
}

fn object_ref(reference: pdfdelta_core::pdf::ObjectRef) -> Value {
    json!({
        "object_number": reference.object_number,
        "generation": reference.generation,
    })
}

/// Shared budgets for the image mode. Every allocation is charged before it
/// happens and the first stop is recorded as a partial reason.
struct ImageBudget {
    objects: usize,
    smasks: usize,
    name_bytes: usize,
    metadata_nodes: usize,
    field_values: usize,
    invocations: usize,
    elided: bool,
    stop_reason: Option<String>,
}

impl ImageBudget {
    fn new() -> Self {
        Self {
            objects: 0,
            smasks: 0,
            name_bytes: 0,
            metadata_nodes: 0,
            field_values: 0,
            invocations: 0,
            elided: false,
            stop_reason: None,
        }
    }

    fn stop(&mut self, reason: &str) -> bool {
        if self.stop_reason.is_none() {
            self.stop_reason = Some(reason.to_owned());
        }
        false
    }

    fn stopped(&self) -> bool {
        self.stop_reason.is_some()
    }

    fn charge_object(&mut self) -> bool {
        if self.stopped() {
            return false;
        }
        if self.objects >= MAX_IMAGE_OBJECTS {
            return self.stop("image object limit");
        }
        self.objects += 1;
        true
    }

    fn charge_smask(&mut self) -> bool {
        if self.stopped() {
            return false;
        }
        if self.smasks >= MAX_IMAGE_SMASKS {
            return self.stop("image smask limit");
        }
        self.smasks += 1;
        true
    }

    fn charge_name(&mut self, bytes: usize) -> bool {
        if self.stopped() {
            return false;
        }
        if self.name_bytes.saturating_add(bytes) > MAX_IMAGE_NAME_BYTES {
            return self.stop("image name byte limit");
        }
        self.name_bytes += bytes;
        true
    }

    fn charge_invocation(&mut self) -> bool {
        if self.stopped() {
            return false;
        }
        if self.invocations >= MAX_IMAGE_INVOCATIONS {
            return self.stop("image invocation limit");
        }
        self.invocations += 1;
        true
    }

    fn charge_metadata(&mut self) -> bool {
        if self.stopped() {
            return false;
        }
        if self.metadata_nodes >= MAX_IMAGE_METADATA_NODES {
            return self.stop("image metadata node limit");
        }
        self.metadata_nodes += 1;
        true
    }

    fn charge_field(&mut self) -> bool {
        if self.stopped() {
            return false;
        }
        if self.field_values >= MAX_IMAGE_FIELD_VALUES {
            return self.stop("image field value limit");
        }
        self.field_values += 1;
        true
    }
}

fn bounded_name_json(bytes: &[u8]) -> Value {
    let take = bytes.len().min(64);
    let hex_take = bytes.len().min(32);
    json!({
        "display": String::from_utf8_lossy(&bytes[..take]).into_owned(),
        "hex": bytes[..hex_take]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "bytes": bytes.len(),
        "elided": bytes.len() > take,
    })
}

/// Only the shapes the image hypothesis needs: scalars, names/strings,
/// references and small arrays of those. Everything else stays uninspected
/// instead of expanding a generic facade conversion.
fn bounded_image_value(object: &PdfObject, budget: &mut ImageBudget, depth: usize) -> Value {
    if !budget.charge_metadata() {
        return json!({"uninspected": "metadata budget"});
    }
    if depth > 2 {
        budget.elided = true;
        return json!({"uninspected": "depth"});
    }
    match object {
        PdfObject::Null => Value::Null,
        PdfObject::Boolean(value) => json!(value),
        PdfObject::Integer(value) => json!(value),
        PdfObject::Real(value) => json!(value),
        PdfObject::Name(bytes) | PdfObject::String(bytes) => bounded_name_json(bytes),
        PdfObject::Reference(reference) => json!({
            "reference": {
                "object_number": reference.object_number,
                "generation": reference.generation,
            }
        }),
        PdfObject::Array(items) => {
            if items.len() > MAX_IMAGE_SMALL_ARRAY {
                budget.elided = true;
            }
            let mut values = Vec::new();
            for item in items.iter().take(MAX_IMAGE_SMALL_ARRAY) {
                if budget.stopped() {
                    break;
                }
                values.push(bounded_image_value(item, budget, depth + 1));
            }
            json!({
                "array": values,
                "count": items.len(),
                "elided": items.len() > MAX_IMAGE_SMALL_ARRAY,
            })
        }
        PdfObject::Dictionary(dictionary) | PdfObject::Stream(dictionary) => {
            let keep = dictionary
                .keys()
                .all(|key| DECODE_PARMS_KEYS.contains(&key.as_slice()));
            if !keep {
                budget.elided = true;
                return json!({
                    "uninspected": "dictionary",
                    "count": dictionary.len(),
                });
            }
            let mut entries = Vec::new();
            for (key, value) in dictionary.iter().take(8) {
                let take = key.len().min(64);
                if !budget.charge_name(take + 1) || !budget.charge_metadata() {
                    budget.elided = true;
                    break;
                }
                entries.push(json!([
                    bounded_name_json(&key[..take]),
                    bounded_image_value(value, budget, depth + 1),
                ]));
            }
            if dictionary.len() > 8 {
                budget.elided = true;
            }
            json!({
                "entries": entries,
                "count": dictionary.len(),
                "elided": dictionary.len() > 8,
            })
        }
    }
}

const DECODE_PARMS_KEYS: [&[u8]; 6] = [
    b"BitsPerComponent",
    b"Colors",
    b"Columns",
    b"Predictor",
    b"EarlyChange",
    b"BlackIs1",
];

fn image_fields(dictionary: &PdfDict, budget: &mut ImageBudget) -> Value {
    const KEYS: [&str; 11] = [
        "Width",
        "Height",
        "BitsPerComponent",
        "ColorSpace",
        "Filter",
        "DecodeParms",
        "Decode",
        "ImageMask",
        "Interpolate",
        "Mask",
        "SMask",
    ];
    let mut map = serde_json::Map::new();
    for key in KEYS {
        if let Some(value) = dictionary.get(key.as_bytes()) {
            if !budget.charge_field() {
                map.insert(key.to_owned(), json!({"uninspected": "field budget"}));
                break;
            }
            map.insert(key.to_owned(), bounded_image_value(value, budget, 0));
        }
    }
    Value::Object(map)
}

fn images_json(pdf: &dyn ParsedPdf, page: usize, budget: &mut ImageBudget) -> ProbeResult<Value> {
    let pages = pdf.pages()?;
    let reference = pages.get(page).ok_or("diagnostic page limit")?;
    let snapshot = pdf.page_snapshot(*reference)?;
    let mut content = Vec::new();
    content_bytes(
        pdf,
        snapshot
            .dictionary
            .get(b"Contents".as_slice())
            .unwrap_or(&PdfObject::Null),
        &mut content,
        0,
    )?;
    let content = lopdf::content::Content::decode_strict(&content)?;
    if content.operations.len() > MAX_IMAGE_OPERATORS {
        return Err("image operator limit".into());
    }
    let mut reasons = Vec::new();
    let resources = match snapshot.resources.as_deref() {
        Some(PdfObject::Reference(reference)) => pdf.resolve(*reference)?,
        Some(value) => value.clone(),
        None => PdfObject::Dictionary(BTreeMap::new()),
    };
    let PdfObject::Dictionary(resources) = resources else {
        reasons.push("page resources are not a dictionary".to_owned());
        return Ok(json!({
            "page": page,
            "status": "partial",
            "reasons": reasons,
            "counts": {"names": 0, "invocations": 0, "terminals": 0},
            "entries": [],
        }));
    };
    let xobjects = match resources.get(b"XObject".as_slice()) {
        Some(value) => match dereference(pdf, value) {
            Ok(PdfObject::Dictionary(dictionary)) => dictionary,
            Ok(_) => {
                reasons.push("page XObject resource is not a dictionary".to_owned());
                BTreeMap::new()
            }
            Err(error) => {
                reasons.push(format!("page XObject resource unresolved: {error}"));
                BTreeMap::new()
            }
        },
        None => BTreeMap::new(),
    };
    let mut executed: BTreeMap<Vec<u8>, Vec<usize>> = BTreeMap::new();
    let mut missing = Vec::new();
    for (index, operation) in content.operations.iter().enumerate() {
        if operation.operator != "Do" {
            continue;
        }
        let Some(name) = single_name_operand(&operation.operands) else {
            if !budget.charge_invocation() {
                break;
            }
            missing.push(json!({
                "operator_index": index,
                "reason": "Do requires exactly one name operand",
            }));
            reasons.push("Do with missing or non-name operand".to_owned());
            continue;
        };
        if !budget.charge_invocation() {
            break;
        }
        if !xobjects.contains_key(name) {
            missing.push(json!({
                "operator_index": index,
                "name": bounded_name_json(name),
                "reason": "resource name not found in page resources",
            }));
            reasons.push("Do references a name missing from the page resources".to_owned());
            continue;
        }
        if let Some(indexes) = executed.get_mut(name) {
            indexes.push(index);
        } else {
            if !budget.charge_name(name.len()) || !budget.charge_object() {
                break;
            }
            executed.insert(name.to_vec(), vec![index]);
        }
    }
    let mut entries = Vec::new();
    let mut terminals = std::collections::BTreeSet::new();
    let mut names = 0_usize;
    let mut invocations = 0_usize;
    let mut unprocessed = 0_usize;
    for (name, indexes) in &executed {
        // No terminal lookup, resolution or clone may happen after a stop:
        // even names accepted during discovery stay unprocessed, and the page
        // reports them explicitly so it cannot appear complete.
        if budget.stopped() {
            unprocessed = executed.len() - names;
            reasons.push("image entries left unprocessed after a budget stop".to_owned());
            break;
        }
        invocations += indexes.len();
        names += 1;
        let value = xobjects.get(name.as_slice()).ok_or("xobject vanished")?;
        let mut entry = json!({
            "name": bounded_name_json(name),
            "operator_indexes": indexes,
            "terminal": Value::Null,
            "kind": "unsupported",
            "image": Value::Null,
        });
        let resolved = match value {
            PdfObject::Reference(reference) => pdf.resolve_with_terminal(*reference),
            other => Ok(pdfdelta_core::pdf::ResolvedObject {
                reference: pdfdelta_core::pdf::ObjectRef {
                    object_number: 0,
                    generation: 0,
                },
                object: other.clone(),
            }),
        };
        match resolved {
            Ok(resolved) => {
                if resolved.reference.object_number != 0 {
                    entry["terminal"] = object_ref(resolved.reference);
                    terminals.insert((
                        resolved.reference.object_number,
                        resolved.reference.generation,
                    ));
                }
                match &resolved.object {
                    PdfObject::Stream(dictionary) => {
                        let subtype =
                            dictionary
                                .get(b"Subtype".as_slice())
                                .and_then(|value| match value {
                                    PdfObject::Name(bytes) => Some(bytes.as_slice()),
                                    _ => None,
                                });
                        match subtype {
                            Some(b"Image") => {
                                entry["kind"] = json!("image");
                                entry["image"] = image_fields(dictionary, budget);
                                if let Some(smask) = dictionary.get(b"SMask".as_slice()) {
                                    match smask {
                                        PdfObject::Reference(reference) => {
                                            if !budget.charge_smask() {
                                                entry["smask"] = json!({
                                                    "unsupported": "smask budget",
                                                });
                                                reasons.push("image smask budget".to_owned());
                                            } else {
                                                match pdf.resolve_with_terminal(*reference) {
                                                    Ok(resolved) => {
                                                        let mut summary = json!({
                                                            "terminal": object_ref(resolved.reference),
                                                            "image": Value::Null,
                                                            "payload": "not decoded",
                                                        });
                                                        if let PdfObject::Stream(mask_dictionary) =
                                                            &resolved.object
                                                        {
                                                            summary["image"] = image_fields(
                                                                mask_dictionary,
                                                                budget,
                                                            );
                                                        } else {
                                                            summary["unsupported"] =
                                                                json!("SMask is not a stream");
                                                            reasons.push(
                                                                "SMask reference is not a stream"
                                                                    .to_owned(),
                                                            );
                                                        }
                                                        entry["smask"] = summary;
                                                    }
                                                    Err(error) => {
                                                        entry["smask"] = json!({
                                                            "status": "unresolved",
                                                            "reason": error.to_string(),
                                                        });
                                                        reasons.push(
                                                            "SMask reference unresolved".to_owned(),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        other => {
                                            entry["smask"] = json!({
                                                "unsupported": "SMask is not an indirect stream",
                                                "shape": bounded_image_value(other, budget, 0),
                                            });
                                            reasons
                                                .push("SMask is not an indirect stream".to_owned());
                                        }
                                    }
                                }
                                if let Some(mask) = dictionary.get(b"Mask".as_slice()) {
                                    entry["mask"] = match mask {
                                        PdfObject::Reference(reference) => json!({
                                            "reference": object_ref(*reference),
                                            "payload": "not inspected",
                                        }),
                                        other => json!({
                                            "shape": bounded_image_value(other, budget, 0),
                                            "payload": "not inspected",
                                        }),
                                    };
                                    reasons.push("explicit Mask payload not inspected".to_owned());
                                }
                            }
                            Some(b"Form") => {
                                entry["kind"] = json!("form");
                                entry["unsupported"] = json!("form interior not expanded");
                                reasons.push("Form XObject interior not inspected".to_owned());
                            }
                            other => {
                                entry["unsupported_subtype"] = json!(other.map(bounded_name_json));
                            }
                        }
                    }
                    _ => {
                        entry["unsupported"] = json!("XObject is not a stream");
                    }
                }
            }
            Err(error) => {
                entry["kind"] = json!("unresolved");
                entry["reason"] = json!(error.to_string());
                reasons.push("XObject reference unresolved".to_owned());
            }
        }
        entries.push(entry);
    }
    if budget.stop_reason.is_some() {
        reasons.push(budget.stop_reason.clone().unwrap_or_default());
    }
    if budget.elided {
        reasons.push("image metadata elided as uninspected".to_owned());
    }
    let status = if reasons.is_empty() {
        "scanned"
    } else {
        "partial"
    };
    Ok(json!({
        "page": page,
        "status": status,
        "reasons": reasons,
        "operator_count": content.operations.len(),
        "counts": {
            "names": names,
            "invocations": invocations,
            "terminals": terminals.len(),
            "unprocessed": unprocessed,
        },
        "missing_resources": missing,
        "entries": entries,
    }))
}

fn single_name_operand(operands: &[lopdf::Object]) -> Option<&[u8]> {
    match operands {
        [lopdf::Object::Name(name)] => Some(name.as_slice()),
        _ => None,
    }
}

fn main() -> ProbeResult<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let (path, mode) = parse_args(&args)?;
    // Only the annotation diagnostic narrows decoded-stream limits; every
    // other mode keeps the parser defaults.
    let limits = match &mode {
        Mode::Annotations { .. } => ParseLimits {
            max_decoded_stream_bytes: MAX_AP_DECODED_BYTES,
            max_total_object_stream_bytes: MAX_ANNOTATION_OBJECT_STREAM_BYTES,
            ..ParseLimits::default()
        },
        _ => ParseLimits::default(),
    };
    let mut bytes = Vec::new();
    File::open(path)?
        .take(u64::try_from(limits.max_input_bytes)? + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_input_bytes {
        return Err("input byte limit".into());
    }
    let input_digest = digest(&bytes);
    let pdf = LopdfParser.parse(Arc::from(bytes), limits)?;
    let selected_page = match mode {
        Mode::DeclarationsOnly => {
            // The metadata/form scan carries its own work and byte limits and
            // never claims complete text inventory.
            serde_json::to_writer_pretty(
                io::stdout().lock(),
                &json!({
                    "version": 1, "mode": "declarations-only", "input_sha256": input_digest,
                    "certifies_text_inventory": false,
                    "parser_issue_count": pdf.issues().len(),
                    "reachable_text_declarations": properties::scan(pdf.as_ref()),
                }),
            )?;
            return Ok(());
        }
        Mode::Operators { page } => {
            let pages = pdf.pages()?;
            let reference = pages.get(page).ok_or("diagnostic page limit")?;
            let snapshot = pdf.page_snapshot(*reference)?;
            let mut content = Vec::new();
            content_bytes(
                pdf.as_ref(),
                snapshot
                    .dictionary
                    .get(b"Contents".as_slice())
                    .unwrap_or(&PdfObject::Null),
                &mut content,
                0,
            )?;
            let operators = operators_json(&content)?;
            serde_json::to_writer_pretty(
                io::stdout().lock(),
                &json!({
                    "version": 1, "mode": "operators", "input_sha256": input_digest,
                    "page": page, "certifies_text_inventory": false,
                    "certifies_geometry": false, "operators": operators,
                }),
            )?;
            return Ok(());
        }
        Mode::Annotations { page } => {
            let annotations = annotations_json(pdf.as_ref(), page)?;
            serde_json::to_writer_pretty(
                io::stdout().lock(),
                &json!({
                    "version": 1, "mode": "annotations", "input_sha256": input_digest,
                    "page": page, "certifies_text_inventory": false,
                    "certifies_geometry": false, "annotations": annotations,
                }),
            )?;
            return Ok(());
        }
        Mode::Images { pages } => {
            let mut budget = ImageBudget::new();
            let mut reports = Vec::new();
            let mut status = "scanned";
            let mut reasons = Vec::new();
            let mut unscanned_pages: Vec<usize> = Vec::new();
            for page in pages {
                if budget.stopped() {
                    unscanned_pages.push(page);
                    status = "partial";
                    continue;
                }
                let report = images_json(pdf.as_ref(), page, &mut budget)?;
                if report["status"] == "partial" {
                    status = "partial";
                }
                for reason in report["reasons"].as_array().into_iter().flatten() {
                    if let Some(reason) = reason.as_str()
                        && !reasons.iter().any(|existing| existing == reason)
                    {
                        reasons.push(reason.to_owned());
                    }
                }
                reports.push(report);
            }
            if budget.stop_reason.is_some() {
                status = "partial";
                if let Some(reason) = &budget.stop_reason {
                    reasons.push(reason.clone());
                }
                reasons.push("later pages were not scanned after a budget stop".to_owned());
            }
            serde_json::to_writer_pretty(
                io::stdout().lock(),
                &json!({
                    "version": 1, "mode": "images", "input_sha256": input_digest,
                    "certifies_text_inventory": false, "certifies_geometry": false,
                    "status": status,
                    "reasons": reasons,
                    "unscanned_pages": unscanned_pages,
                    "scope": "executed page-program Do invocations only; image dictionaries are read as declared geometry and references; image/SMask/Form payloads are not decoded and payload hashes are not collected; Form interiors, nested Do, explicit Mask payloads and uninvoked resources are not inspected; page Contents and object streams keep the parser's default decode limits",
                    "pages": reports,
                }),
            )?;
            return Ok(());
        }
        Mode::Full { selected_page } => selected_page,
    };
    let pages = pdf.pages()?;
    if (selected_page.is_none() && pages.len() > 200)
        || selected_page.is_some_and(|page| page >= pages.len())
    {
        return Err("diagnostic page limit".into());
    }
    let mut result = Vec::new();
    for (index, page) in pages.into_iter().enumerate() {
        if selected_page.is_some_and(|page| page != index) {
            continue;
        }
        let snapshot = pdf.page_snapshot(page)?;
        let mut bytes = Vec::new();
        content_bytes(
            pdf.as_ref(),
            snapshot
                .dictionary
                .get(b"Contents".as_slice())
                .unwrap_or(&PdfObject::Null),
            &mut bytes,
            0,
        )?;
        let mut row = trace(&bytes, selected_page.is_some())?;
        let resources = match snapshot.resources.as_deref() {
            Some(PdfObject::Reference(reference)) => pdf.resolve(*reference)?,
            Some(resources) => resources.clone(),
            None => PdfObject::Dictionary(BTreeMap::new()),
        };
        let PdfObject::Dictionary(mut resources) = resources else {
            return Err("invalid page resources".into());
        };
        resources.remove(b"Font".as_slice());
        let mut remaining = MAX_RESOURCE_VISITS;
        let closure = resource_digest(
            pdf.as_ref(),
            &PdfObject::Dictionary(resources),
            0,
            &mut remaining,
        );
        row["page"] = json!(index);
        row["non_font_resource_closure"] = match closure {
            Ok(digest) => json!({"status": "captured", "sha256": digest}),
            Err(error) => json!({"status": "failed", "reason": error.to_string()}),
        };
        result.push(row);
    }
    serde_json::to_writer_pretty(
        io::stdout().lock(),
        &json!({
            "version": 1, "input_sha256": input_digest, "certifies_text_inventory": false,
            "parser_issue_count": pdf.issues().len(), "pages": result,
            "reachable_text_declarations": properties::scan(pdf.as_ref()),
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Mode, operators_json, parse_args, trace};

    #[test]
    fn declarations_only_mode_skips_page_program_selection() {
        let args = ["--declarations-only".to_owned(), "input.pdf".to_owned()];
        let (path, mode) = parse_args(&args).unwrap();
        assert_eq!(path, "input.pdf");
        assert!(matches!(mode, Mode::DeclarationsOnly));
        let (path, mode) = parse_args(&["input.pdf".to_owned()]).unwrap();
        assert_eq!(path, "input.pdf");
        assert!(matches!(
            mode,
            Mode::Full {
                selected_page: None
            }
        ));
        let (_, mode) = parse_args(&["input.pdf".to_owned(), "3".to_owned()]).unwrap();
        assert!(matches!(
            mode,
            Mode::Full {
                selected_page: Some(3)
            }
        ));
        assert!(parse_args(&["--declarations-only".to_owned()]).is_err());
        assert!(parse_args(&["--unknown".to_owned(), "input.pdf".to_owned()]).is_err());
    }

    #[test]
    fn operator_json_preserves_operand_text_and_modes() {
        let parsed = operators_json(b"1 w 1 0 0 1 35.75 569.999 cm 0 0 m 540.5 0 l S")
            .expect("parsed operators");
        let operators = parsed["operators"].as_array().unwrap();
        assert_eq!(operators.len(), 5);
        assert_eq!(operators[0]["operator"], "w");
        assert_eq!(operators[0]["operands"][0]["number"], "1");
        assert_eq!(operators[1]["operator"], "cm");
        assert_eq!(operators[1]["operands"][4]["number"], "35.75");
        assert_eq!(operators[1]["operands"][5]["number"], "569.999");
        assert_eq!(operators[3]["operator"], "l");
        assert_eq!(operators[3]["operands"][0]["number"], "540.5");
        let (_, mode) = parse_args(&["--operators".into(), "3".into(), "a.pdf".into()]).unwrap();
        assert!(matches!(mode, Mode::Operators { page: 3 }));
        let (_, mode) = parse_args(&["--annotations".into(), "1".into(), "a.pdf".into()]).unwrap();
        assert!(matches!(mode, Mode::Annotations { page: 1 }));
        assert!(parse_args(&["--annotations".into(), "1".into()]).is_err());
    }

    #[test]
    fn native_text_changes_do_not_hide_paint_operand_changes() {
        let baseline = trace(b"BT /F1 12 Tf (old) Tj ET 1 2 3 4 re f", false).unwrap();
        let changed_text = trace(b"BT /F1 12 Tf (new) Tj ET 1 2 3 4 re f", false).unwrap();
        let changed_paint = trace(b"BT /F1 12 Tf (new) Tj ET 1 2 3 5 re f", false).unwrap();
        assert_eq!(
            baseline["without_text_operators_sha256"],
            changed_text["without_text_operators_sha256"]
        );
        assert_ne!(
            baseline["without_text_operators_sha256"],
            changed_paint["without_text_operators_sha256"]
        );
        assert_eq!(
            trace(b"7 Tr BT (clip) Tj ET", false).unwrap()["text_clip_or_invalid_mode_seen"],
            true
        );
    }
    #[test]
    fn paint_markers_retain_declarations_without_interpreting_their_meaning() {
        let observed = trace(b"/Artifact BMC 0 0 1 1 re f EMC /Span << /ActualText (minus) >> BDC 0 0 m 1 0 l S EMC /Span /P1 BDC /Im1 Do EMC", false).unwrap();
        assert_eq!(
            observed["inline_actual_text_declarations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            observed["inline_actual_text_declarations"][0]["string_value"],
            true
        );
        let paints = observed["paint_marked_properties"].as_array().unwrap();
        assert_eq!(paints.len(), 3);
        assert_eq!(paints[0]["artifact_tag"], true);
        assert_eq!(
            paints[0]["inline_actual_text_declarations"],
            serde_json::json!([])
        );
        assert_eq!(
            paints[1]["inline_actual_text_declarations"],
            serde_json::json!([0])
        );
        assert_eq!(paints[2]["named_property_unresolved"], true);
        assert_eq!(observed["unbalanced_marked_content"], false);
        assert_eq!(
            trace(b"EMC /Span BMC", false).unwrap()["unbalanced_marked_content"],
            true
        );
        assert_eq!(
            trace(b"/Span << /ActualText 42 >> BDC 0 0 1 1 re f EMC", false).unwrap()["inline_actual_text_declarations"]
                [0]["string_value"],
            false
        );
        assert!(trace(&b"/Span BMC ".repeat(65), false).is_err());
    }
}

#[cfg(test)]
mod annotation_tests {
    use super::{
        MAX_ANNOTATION_OBJECT_STREAM_BYTES, MAX_ANNOTATIONS, MAX_AP_DECODED_BYTES, annotations_json,
    };
    use pdfdelta_core::pdf::{
        DecodedStream, ObjectRef, PageRef, ParsedPdf, PdfDict, PdfObject, PdfVersion, RawStream,
    };
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn object_ref(number: u32) -> ObjectRef {
        ObjectRef {
            object_number: number,
            generation: 0,
        }
    }

    fn reference(number: u32) -> PdfObject {
        PdfObject::Reference(object_ref(number))
    }

    fn dictionary(entries: Vec<(&[u8], PdfObject)>) -> PdfDict {
        entries
            .into_iter()
            .map(|(key, value)| (key.to_vec(), value))
            .collect()
    }

    struct FakePdf {
        trailer: PdfDict,
        objects: HashMap<ObjectRef, PdfObject>,
        decode_calls: AtomicUsize,
    }

    impl FakePdf {
        fn new(page: PdfDict, objects: Vec<(u32, PdfObject)>) -> Self {
            let mut map = HashMap::new();
            map.insert(object_ref(100), PdfObject::Dictionary(page));
            map.insert(object_ref(200), PdfObject::Dictionary(PdfDict::new()));
            for (number, value) in objects {
                map.insert(object_ref(number), value);
            }
            Self {
                trailer: dictionary(vec![(b"Root", reference(200))]),
                objects: map,
                decode_calls: AtomicUsize::new(0),
            }
        }

        fn with_trailer(mut self, trailer: PdfDict) -> Self {
            self.trailer = trailer;
            self
        }

        fn with_catalog(mut self, catalog: PdfDict) -> Self {
            self.objects
                .insert(object_ref(200), PdfObject::Dictionary(catalog));
            self
        }
    }

    impl ParsedPdf for FakePdf {
        fn version(&self) -> PdfVersion {
            PdfVersion { major: 1, minor: 7 }
        }

        fn trailer(&self) -> pdfdelta_core::Result<PdfDict> {
            Ok(self.trailer.clone())
        }

        fn resolve(&self, reference: ObjectRef) -> pdfdelta_core::Result<PdfObject> {
            self.objects
                .get(&reference)
                .cloned()
                .ok_or_else(|| pdfdelta_core::Error::Unsupported("fake object missing".into()))
        }

        fn pages(&self) -> pdfdelta_core::Result<Vec<PageRef>> {
            Ok(vec![PageRef(object_ref(100))])
        }

        fn page_dict(&self, page: PageRef) -> pdfdelta_core::Result<PdfDict> {
            match self.resolve(page.0)? {
                PdfObject::Dictionary(page) => Ok(page),
                _ => Ok(PdfDict::new()),
            }
        }

        fn raw_stream(&self, reference: ObjectRef) -> pdfdelta_core::Result<RawStream> {
            match self.resolve(reference)? {
                PdfObject::Stream(dictionary) => Ok(RawStream {
                    dictionary,
                    bytes: Vec::new(),
                }),
                _ => Err(pdfdelta_core::Error::Unsupported(
                    "fake not a stream".into(),
                )),
            }
        }

        fn decoded_stream(&self, reference: ObjectRef) -> pdfdelta_core::Result<DecodedStream> {
            self.decode_calls.fetch_add(1, Ordering::Relaxed);
            match self.resolve(reference)? {
                PdfObject::Stream(dictionary) => {
                    if dictionary.contains_key(b"FakeDecodeFailure".as_slice()) {
                        return Err(pdfdelta_core::Error::Unsupported(
                            "fake decode failure".into(),
                        ));
                    }
                    Ok(DecodedStream {
                        dictionary,
                        bytes: b"q 1 0 0 1 0 0 cm Q".to_vec(),
                    })
                }
                _ => Err(pdfdelta_core::Error::Unsupported(
                    "fake not a stream".into(),
                )),
            }
        }
    }

    fn widget() -> PdfObject {
        PdfObject::Dictionary(dictionary(vec![(
            b"Subtype",
            PdfObject::Name(b"Widget".to_vec()),
        )]))
    }

    fn reasons(value: &serde_json::Value) -> Vec<String> {
        value["incomplete_reasons"]
            .as_array()
            .expect("incomplete reasons")
            .iter()
            .map(|reason| reason.as_str().unwrap_or_default().to_owned())
            .collect()
    }

    #[test]
    fn annotation_limit_records_partial_reason() {
        let annots = vec![widget(); MAX_ANNOTATIONS + 1];
        let page = dictionary(vec![(b"Annots", PdfObject::Array(annots))]);
        let value = annotations_json(&FakePdf::new(page, vec![]), 0).unwrap();
        assert_eq!(
            value["annotations"].as_array().unwrap().len(),
            MAX_ANNOTATIONS
        );
        assert_eq!(value["partial"], true);
        assert!(reasons(&value).contains(&"annotation_truncated".to_owned()));
    }

    #[test]
    fn catalog_resolution_failure_is_recorded() {
        let page = PdfDict::new();
        let mut fake = FakePdf::new(page, vec![]);
        fake.trailer = dictionary(vec![(b"Root", reference(999))]);
        let value = annotations_json(&fake, 0).unwrap();
        assert_eq!(value["partial"], true);
        assert!(reasons(&value).contains(&"catalog_resolution_failed".to_owned()));
    }

    #[test]
    fn indirect_ap_state_dictionary_is_resolved() {
        let annotation = PdfObject::Dictionary(dictionary(vec![
            (b"Subtype", PdfObject::Name(b"Widget".to_vec())),
            (
                b"AP",
                PdfObject::Dictionary(dictionary(vec![(b"N", reference(10))])),
            ),
        ]));
        let states = PdfObject::Dictionary(dictionary(vec![
            (b"On", reference(11)),
            (b"Off", reference(12)),
        ]));
        let pdf = FakePdf::new(
            dictionary(vec![(b"Annots", PdfObject::Array(vec![annotation]))]),
            vec![
                (10, states),
                (11, PdfObject::Stream(PdfDict::new())),
                (12, PdfObject::Stream(PdfDict::new())),
            ],
        );
        let value = annotations_json(&pdf, 0).unwrap();
        let appearance = &value["annotations"][0]["appearance"]["N"];
        assert!(appearance["On"]["bytes"].is_number(), "{appearance}");
        assert!(appearance["Off"]["bytes"].is_number(), "{appearance}");
        assert_eq!(
            appearance["On"]["operators"]["operator_count"].as_u64(),
            Some(3)
        );
        assert_eq!(value["partial"], false);
    }

    #[test]
    fn field_budget_limits_are_recorded() {
        let mut catalog = PdfDict::new();
        catalog.insert(
            b"AcroForm".to_vec(),
            PdfObject::Dictionary(dictionary(vec![(
                b"Fields",
                PdfObject::Array(vec![PdfObject::Dictionary(PdfDict::new()); 300]),
            )])),
        );
        let pdf = FakePdf::new(PdfDict::new(), vec![]).with_catalog(catalog);
        let value = annotations_json(&pdf, 0).unwrap();
        assert!(reasons(&value).contains(&"field_visit_limit".to_owned()));
        assert_eq!(value["partial"], true);

        let mut chain = PdfObject::Dictionary(PdfDict::new());
        for _ in 0..10 {
            chain =
                PdfObject::Dictionary(dictionary(vec![(b"Kids", PdfObject::Array(vec![chain]))]));
        }
        let mut catalog = PdfDict::new();
        catalog.insert(
            b"AcroForm".to_vec(),
            PdfObject::Dictionary(dictionary(vec![(b"Fields", PdfObject::Array(vec![chain]))])),
        );
        let pdf = FakePdf::new(PdfDict::new(), vec![]).with_catalog(catalog);
        let value = annotations_json(&pdf, 0).unwrap();
        assert!(reasons(&value).contains(&"field_depth_limit".to_owned()));
    }

    #[test]
    fn stream_limits_and_decode_failures_are_recorded() {
        let oversized = PdfObject::Stream(dictionary(vec![(
            b"Length",
            PdfObject::Integer(i64::try_from(MAX_AP_DECODED_BYTES).unwrap() + 1),
        )]));
        let failing = PdfObject::Stream(dictionary(vec![(
            b"FakeDecodeFailure",
            PdfObject::Boolean(true),
        )]));
        let annotation = |number: u32| {
            PdfObject::Dictionary(dictionary(vec![
                (b"Subtype", PdfObject::Name(b"Widget".to_vec())),
                (
                    b"AP",
                    PdfObject::Dictionary(dictionary(vec![(b"N", reference(number))])),
                ),
            ]))
        };
        let pdf = FakePdf::new(
            dictionary(vec![(
                b"Annots",
                PdfObject::Array(vec![annotation(11), annotation(12)]),
            )]),
            vec![(11, oversized), (12, failing)],
        );
        let value = annotations_json(&pdf, 0).unwrap();
        assert_eq!(
            value["annotations"][0]["appearance"]["N"]["skipped"],
            "declared_length_over_decode_limit"
        );
        assert_eq!(
            value["annotations"][1]["appearance"]["N"]["decode_status"],
            "failed"
        );
        assert_eq!(pdf.decode_calls.load(Ordering::Relaxed), 1);
    }

    fn run_length_bomb_pdf() -> Vec<u8> {
        use lopdf::{Dictionary, Document, Object, Stream};
        let decoded_len = 2 * 1024 * 1024;
        let mut encoded = Vec::new();
        for _ in 0..(decoded_len / 128) {
            encoded.push(0x81_u8);
            encoded.push(0x00_u8);
        }
        encoded.push(0x80_u8);
        assert!(encoded.len() < MAX_AP_DECODED_BYTES);
        let mut document = Document::with_version("1.5");
        let mut stream_dictionary = Dictionary::new();
        stream_dictionary.set("Filter", Object::Name(b"RunLengthDecode".to_vec()));
        stream_dictionary.set(
            "Length",
            Object::Integer(i64::try_from(encoded.len()).unwrap()),
        );
        let stream_id = document.add_object(Stream::new(stream_dictionary, encoded));
        let mut appearance = Dictionary::new();
        appearance.set("N", Object::Reference(stream_id));
        let mut annotation = Dictionary::new();
        annotation.set("Subtype", Object::Name(b"Widget".to_vec()));
        annotation.set("AP", Object::Dictionary(appearance));
        let annotation_id = document.add_object(annotation);
        let mut pages = Dictionary::new();
        pages.set("Type", Object::Name(b"Pages".to_vec()));
        pages.set("Kids", Object::Array(Vec::new()));
        pages.set("Count", Object::Integer(1));
        let pages_id = document.add_object(pages);
        let mut page = Dictionary::new();
        page.set("Type", Object::Name(b"Page".to_vec()));
        page.set("Parent", Object::Reference(pages_id));
        page.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        page.set(
            "Annots",
            Object::Array(vec![Object::Reference(annotation_id)]),
        );
        let page_id = document.add_object(page);
        if let Some(Object::Dictionary(pages)) = document.objects.get_mut(&pages_id) {
            pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
        }
        let mut catalog = Dictionary::new();
        catalog.set("Type", Object::Name(b"Catalog".to_vec()));
        catalog.set("Pages", Object::Reference(pages_id));
        let catalog_id = document.add_object(catalog);
        document.trailer.set("Root", Object::Reference(catalog_id));
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn compressed_stream_decode_limit_is_enforced_and_recorded() {
        use pdfdelta_core::pdf::{LopdfParser, ParseLimits, PdfParser};
        use std::sync::Arc;

        let bytes = run_length_bomb_pdf();
        let parser = LopdfParser;
        let annotation_limits = ParseLimits {
            max_decoded_stream_bytes: MAX_AP_DECODED_BYTES,
            max_total_object_stream_bytes: MAX_ANNOTATION_OBJECT_STREAM_BYTES,
            ..ParseLimits::default()
        };
        let pdf = parser
            .parse(Arc::from(bytes.clone()), annotation_limits)
            .unwrap();
        let value = annotations_json(pdf.as_ref(), 0).unwrap();
        assert_eq!(value["partial"], true, "{value}");
        assert!(reasons(&value).contains(&"ap_decode_failed".to_owned()));
        let appearance = &value["annotations"][0]["appearance"]["N"];
        assert_eq!(appearance["decode_status"], "failed", "{appearance}");

        // Default limits are not narrowed: the same stream decodes fully.
        let default_pdf = parser
            .parse(Arc::from(bytes), ParseLimits::default())
            .unwrap();
        let default_value = annotations_json(default_pdf.as_ref(), 0).unwrap();
        assert_eq!(default_value["partial"], false);
        let bytes_len = default_value["annotations"][0]["appearance"]["N"]["bytes"]
            .as_u64()
            .unwrap();
        assert!(bytes_len > u64::try_from(MAX_AP_DECODED_BYTES).unwrap());
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn attach_image_page(
        document: &mut lopdf::Document,
        xobjects: Vec<(&str, lopdf::ObjectId)>,
        content: &[u8],
    ) {
        use lopdf::{Object, Stream, dictionary};
        let pages_id = document.new_object_id();
        let page_id = document.new_object_id();
        let mut xdict = lopdf::Dictionary::new();
        for (name, id) in xobjects {
            xdict.set(name, Object::Reference(id));
        }
        let content_id = document.add_object(Stream::new(dictionary! {}, content.to_vec()));
        document.objects.insert(
            page_id,
            dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(612), Object::Integer(792),
                ]),
                "Resources" => dictionary! {"XObject" => Object::Dictionary(xdict)},
                "Contents" => content_id,
            }
            .into(),
        );
        document.objects.insert(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => Object::Integer(1),
            }
            .into(),
        );
        let catalog = document.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
        document.trailer.set("Root", catalog);
    }

    fn save_image_pdf(document: &mut lopdf::Document) -> Vec<u8> {
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    fn image_object(
        document: &mut lopdf::Document,
        width: i64,
        height: i64,
        extra: Vec<(&str, lopdf::Object)>,
    ) -> lopdf::ObjectId {
        use lopdf::{Object, Stream, dictionary};
        let mut dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => Object::Integer(width),
            "Height" => Object::Integer(height),
            "BitsPerComponent" => Object::Integer(8),
            "Filter" => Object::Name(b"FlateDecode".to_vec()),
        };
        for (key, value) in extra {
            dict.set(key, value);
        }
        document.add_object(Stream::new(dict, vec![0_u8; 4]))
    }

    fn parse_image_pdf(bytes: Vec<u8>) -> Box<dyn ParsedPdf> {
        LopdfParser
            .parse(Arc::from(bytes), ParseLimits::default())
            .expect("fixture parses")
    }

    #[test]
    fn image_mode_counts_only_executed_invocations() {
        let mut document = lopdf::Document::with_version("1.5");
        let used_id = image_object(&mut document, 301, 301, vec![]);
        let unused_id = image_object(&mut document, 999, 999, vec![]);
        attach_image_page(
            &mut document,
            vec![("Im1", used_id), ("Im2", unused_id)],
            b"q /Im1 Do Q /Missing Do",
        );
        let pdf = parse_image_pdf(save_image_pdf(&mut document));
        let mut budget = ImageBudget::new();
        let report = images_json(pdf.as_ref(), 0, &mut budget).unwrap();
        assert_eq!(report["counts"]["names"], 1, "{report}");
        assert_eq!(report["counts"]["invocations"], 1, "{report}");
        assert_eq!(report["counts"]["terminals"], 1, "{report}");
        assert_eq!(report["entries"][0]["name"]["display"], "Im1");
        assert_eq!(report["entries"][0]["image"]["Width"], 301);
        assert_eq!(report["entries"][0]["image"]["Height"], 301);
        let missing = report["missing_resources"].as_array().unwrap();
        assert!(
            missing
                .iter()
                .any(|entry| entry["name"]["display"] == "Missing")
        );
        assert_eq!(report["entries"][0]["operator_indexes"][0], 1);
        assert_eq!(report["status"], "partial");
    }

    #[test]
    fn image_mode_resolves_alias_and_smask_terminals() {
        use lopdf::Object;
        let mut document = lopdf::Document::with_version("1.5");
        let mask_id = image_object(
            &mut document,
            301,
            301,
            vec![("ColorSpace", Object::Name(b"DeviceGray".to_vec()))],
        );
        let real_id = image_object(
            &mut document,
            301,
            301,
            vec![("SMask", Object::Reference(mask_id))],
        );
        let alias_id = document.add_object(Object::Reference(real_id));
        attach_image_page(&mut document, vec![("Im1", alias_id)], b"/Im1 Do");
        let pdf = parse_image_pdf(save_image_pdf(&mut document));
        let mut budget = ImageBudget::new();
        let report = images_json(pdf.as_ref(), 0, &mut budget).unwrap();
        let entry = &report["entries"][0];
        let real_number = u64::try_from(real_id.0).unwrap();
        assert_eq!(entry["terminal"]["object_number"], real_number);
        let mask_number = u64::try_from(mask_id.0).unwrap();
        assert_eq!(entry["smask"]["terminal"]["object_number"], mask_number);
        assert_eq!(entry["smask"]["image"]["Width"], 301);
        assert_eq!(entry["smask"]["image"]["Height"], 301);
        assert_eq!(entry["smask"]["payload"], "not decoded");
    }

    #[test]
    fn image_mode_same_terminal_two_names_counts_one_terminal() {
        use lopdf::Object;
        let mut document = lopdf::Document::with_version("1.5");
        let real_id = image_object(&mut document, 301, 301, vec![]);
        let first_alias = document.add_object(Object::Reference(real_id));
        let second_alias = document.add_object(Object::Reference(real_id));
        attach_image_page(
            &mut document,
            vec![("ImA", first_alias), ("ImB", second_alias)],
            b"/ImA Do /ImB Do",
        );
        let pdf = parse_image_pdf(save_image_pdf(&mut document));
        let mut budget = ImageBudget::new();
        let report = images_json(pdf.as_ref(), 0, &mut budget).unwrap();
        assert_eq!(report["counts"]["names"], 2, "{report}");
        assert_eq!(report["counts"]["invocations"], 2, "{report}");
        assert_eq!(report["counts"]["terminals"], 1, "{report}");
    }

    #[test]
    fn image_mode_non_utf8_names_stay_distinct() {
        let mut document = lopdf::Document::with_version("1.5");
        let first_id = image_object(&mut document, 301, 301, vec![]);
        let second_id = image_object(&mut document, 301, 301, vec![]);
        {
            use lopdf::{Object, Stream, dictionary};
            let pages_id = document.new_object_id();
            let page_id = document.new_object_id();
            let mut xdict = lopdf::Dictionary::new();
            xdict.set(vec![0xff_u8, b'A'], Object::Reference(first_id));
            xdict.set(vec![0xfe_u8, b'A'], Object::Reference(second_id));
            let content = vec![
                b'/', 0xff_u8, b'A', b' ', b'D', b'o', b' ', b'/', 0xfe_u8, b'A', b' ', b'D', b'o',
            ];
            let content_id = document.add_object(Stream::new(dictionary! {}, content));
            document.objects.insert(
                page_id,
                dictionary! {
                    "Type" => "Page",
                    "Parent" => pages_id,
                    "MediaBox" => Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(612), Object::Integer(792),
                    ]),
                    "Resources" => dictionary! {"XObject" => Object::Dictionary(xdict)},
                    "Contents" => content_id,
                }
                .into(),
            );
            document.objects.insert(
                pages_id,
                dictionary! {
                    "Type" => "Pages",
                    "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                    "Count" => Object::Integer(1),
                }
                .into(),
            );
            let catalog =
                document.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
            document.trailer.set("Root", catalog);
        }
        let pdf = parse_image_pdf(save_image_pdf(&mut document));
        let mut budget = ImageBudget::new();
        let report = images_json(pdf.as_ref(), 0, &mut budget).unwrap();
        assert_eq!(report["counts"]["names"], 2, "{report}");
        let hexes = report["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"]["hex"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        assert!(hexes.iter().any(|hex| hex == "ff41"), "{hexes:?}");
        assert!(hexes.iter().any(|hex| hex == "fe41"), "{hexes:?}");
    }

    #[test]
    fn image_mode_extra_do_operand_is_partial() {
        let mut document = lopdf::Document::with_version("1.5");
        let image_id = image_object(&mut document, 301, 301, vec![]);
        attach_image_page(&mut document, vec![("Im1", image_id)], b"/Im1 /Im1 Do");
        let pdf = parse_image_pdf(save_image_pdf(&mut document));
        let mut budget = ImageBudget::new();
        let report = images_json(pdf.as_ref(), 0, &mut budget).unwrap();
        assert_eq!(report["status"], "partial", "{report}");
        assert_eq!(report["counts"]["invocations"], 0, "{report}");
        let missing = report["missing_resources"].as_array().unwrap();
        assert!(
            missing
                .iter()
                .any(|entry| entry["reason"] == "Do requires exactly one name operand"),
            "{report}"
        );
    }

    #[test]
    fn image_mode_missing_terminal_and_smask_keep_entries() {
        use lopdf::Object;
        let mut document = lopdf::Document::with_version("1.5");
        let good_id = image_object(
            &mut document,
            301,
            301,
            vec![("SMask", Object::Reference((9999, 0)))],
        );
        let dangling_alias = document.add_object(Object::Reference((8888, 0)));
        attach_image_page(
            &mut document,
            vec![("Good", good_id), ("Dangling", dangling_alias)],
            b"/Good Do /Dangling Do",
        );
        let pdf = parse_image_pdf(save_image_pdf(&mut document));
        let mut budget = ImageBudget::new();
        let report = images_json(pdf.as_ref(), 0, &mut budget).unwrap();
        assert_eq!(report["counts"]["names"], 2, "{report}");
        let entries = report["entries"].as_array().unwrap();
        let good = entries
            .iter()
            .find(|entry| entry["name"]["display"] == "Good")
            .unwrap();
        assert_eq!(good["kind"], "image");
        assert_eq!(good["smask"]["status"], "unresolved");
        let dangling = entries
            .iter()
            .find(|entry| entry["name"]["display"] == "Dangling")
            .unwrap();
        assert_eq!(dangling["kind"], "unresolved");
        assert_eq!(report["status"], "partial");
    }

    #[test]
    fn image_mode_budget_stop_marks_partial_and_bounds_metadata() {
        let mut document = lopdf::Document::with_version("1.5");
        let first_id = image_object(&mut document, 301, 301, vec![]);
        let second_id = image_object(&mut document, 301, 301, vec![]);
        attach_image_page(
            &mut document,
            vec![("ImA", first_id), ("ImB", second_id)],
            b"/ImA Do /ImB Do",
        );
        let pdf = parse_image_pdf(save_image_pdf(&mut document));
        let mut budget = ImageBudget::new();
        budget.objects = MAX_IMAGE_OBJECTS - 1;
        let report = images_json(pdf.as_ref(), 0, &mut budget).unwrap();
        assert_eq!(report["status"], "partial", "{report}");
        assert_eq!(report["counts"]["names"], 0, "{report}");
        assert_eq!(report["counts"]["unprocessed"], 1, "{report}");
        assert!(
            report["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == "image object limit"),
            "{report}"
        );
        assert!(
            report["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == "image entries left unprocessed after a budget stop"),
            "{report}"
        );

        let mut budget = ImageBudget::new();
        budget.metadata_nodes = MAX_IMAGE_METADATA_NODES;
        let report = images_json(pdf.as_ref(), 0, &mut budget).unwrap();
        assert_eq!(report["status"], "partial", "{report}");
        assert!(
            report["entries"][0]["image"]
                .as_object()
                .unwrap()
                .values()
                .any(|value| value["uninspected"] == "metadata budget"),
            "{report}"
        );
    }

    struct DecodeSpy<'a> {
        inner: &'a dyn ParsedPdf,
        decodes: Mutex<Vec<pdfdelta_core::pdf::ObjectRef>>,
    }

    impl ParsedPdf for DecodeSpy<'_> {
        fn version(&self) -> pdfdelta_core::pdf::PdfVersion {
            self.inner.version()
        }

        fn trailer(&self) -> pdfdelta_core::Result<PdfDict> {
            self.inner.trailer()
        }

        fn resolve(
            &self,
            reference: pdfdelta_core::pdf::ObjectRef,
        ) -> pdfdelta_core::Result<PdfObject> {
            self.inner.resolve(reference)
        }

        fn pages(&self) -> pdfdelta_core::Result<Vec<pdfdelta_core::pdf::PageRef>> {
            self.inner.pages()
        }

        fn page_dict(&self, page: pdfdelta_core::pdf::PageRef) -> pdfdelta_core::Result<PdfDict> {
            self.inner.page_dict(page)
        }

        fn raw_stream(
            &self,
            reference: pdfdelta_core::pdf::ObjectRef,
        ) -> pdfdelta_core::Result<pdfdelta_core::pdf::RawStream> {
            self.inner.raw_stream(reference)
        }

        fn decoded_stream(
            &self,
            reference: pdfdelta_core::pdf::ObjectRef,
        ) -> pdfdelta_core::Result<pdfdelta_core::pdf::DecodedStream> {
            self.decodes.lock().unwrap().push(reference);
            self.inner.decoded_stream(reference)
        }
    }

    #[test]
    fn image_mode_never_decodes_image_or_smask_payloads() {
        use lopdf::Object;
        let mut document = lopdf::Document::with_version("1.5");
        let mask_id = image_object(&mut document, 301, 301, vec![]);
        let image_id = image_object(
            &mut document,
            301,
            301,
            vec![("SMask", Object::Reference(mask_id))],
        );
        attach_image_page(&mut document, vec![("Im1", image_id)], b"/Im1 Do");
        let bytes = save_image_pdf(&mut document);
        let parsed = parse_image_pdf(bytes);
        let spy = DecodeSpy {
            inner: parsed.as_ref(),
            decodes: Mutex::new(Vec::new()),
        };
        let mut budget = ImageBudget::new();
        let report = images_json(&spy, 0, &mut budget).unwrap();
        assert_eq!(report["counts"]["names"], 1, "{report}");
        let decodes = spy.decodes.lock().unwrap();
        assert_eq!(decodes.len(), 1, "only the page Contents stream may decode");
        let image_terminal = report["entries"][0]["terminal"]["object_number"]
            .as_u64()
            .unwrap();
        assert!(
            decodes
                .iter()
                .all(|reference| u64::from(reference.object_number) != image_terminal),
            "{decodes:?}"
        );
        let mask_terminal = report["entries"][0]["smask"]["terminal"]["object_number"]
            .as_u64()
            .unwrap();
        assert!(
            decodes
                .iter()
                .all(|reference| u64::from(reference.object_number) != mask_terminal),
            "{decodes:?}"
        );
    }

    struct ResolveSpy<'a> {
        inner: &'a dyn ParsedPdf,
        xobject_resolves: Mutex<Vec<pdfdelta_core::pdf::ObjectRef>>,
    }

    impl ParsedPdf for ResolveSpy<'_> {
        fn version(&self) -> pdfdelta_core::pdf::PdfVersion {
            self.inner.version()
        }

        fn trailer(&self) -> pdfdelta_core::Result<PdfDict> {
            self.inner.trailer()
        }

        fn resolve(
            &self,
            reference: pdfdelta_core::pdf::ObjectRef,
        ) -> pdfdelta_core::Result<PdfObject> {
            self.inner.resolve(reference)
        }

        fn resolve_with_terminal(
            &self,
            reference: pdfdelta_core::pdf::ObjectRef,
        ) -> pdfdelta_core::Result<pdfdelta_core::pdf::ResolvedObject> {
            self.xobject_resolves.lock().unwrap().push(reference);
            self.inner.resolve_with_terminal(reference)
        }

        fn pages(&self) -> pdfdelta_core::Result<Vec<pdfdelta_core::pdf::PageRef>> {
            self.inner.pages()
        }

        fn page_dict(&self, page: pdfdelta_core::pdf::PageRef) -> pdfdelta_core::Result<PdfDict> {
            self.inner.page_dict(page)
        }

        fn raw_stream(
            &self,
            reference: pdfdelta_core::pdf::ObjectRef,
        ) -> pdfdelta_core::Result<pdfdelta_core::pdf::RawStream> {
            self.inner.raw_stream(reference)
        }

        fn decoded_stream(
            &self,
            reference: pdfdelta_core::pdf::ObjectRef,
        ) -> pdfdelta_core::Result<pdfdelta_core::pdf::DecodedStream> {
            self.inner.decoded_stream(reference)
        }
    }

    #[test]
    fn image_mode_object_limit_stops_before_insert_and_resolution() {
        let mut document = lopdf::Document::with_version("1.5");
        let first_id = image_object(&mut document, 301, 301, vec![]);
        let second_id = image_object(&mut document, 301, 301, vec![]);
        attach_image_page(
            &mut document,
            vec![("ImA", first_id), ("ImB", second_id)],
            b"/ImA Do /ImB Do",
        );
        let parsed = parse_image_pdf(save_image_pdf(&mut document));
        let spy = ResolveSpy {
            inner: parsed.as_ref(),
            xobject_resolves: Mutex::new(Vec::new()),
        };
        let mut budget = ImageBudget::new();
        budget.objects = MAX_IMAGE_OBJECTS - 1;
        let report = images_json(&spy, 0, &mut budget).unwrap();
        assert_eq!(report["counts"]["names"], 0, "{report}");
        assert_eq!(report["counts"]["unprocessed"], 1, "{report}");
        assert_eq!(report["status"], "partial", "{report}");
        let accepted_number = u64::from(first_id.0);
        let stopped_number = u64::from(second_id.0);
        let resolves = spy.xobject_resolves.lock().unwrap();
        assert!(
            resolves.iter().all(|reference| {
                let number = u64::from(reference.object_number);
                number != accepted_number && number != stopped_number
            }),
            "no image reference may be resolved after a discovery stop: {resolves:?}"
        );
    }

    #[test]
    fn image_mode_metadata_stop_prevents_next_resolution() {
        let mut document = lopdf::Document::with_version("1.5");
        let first_id = image_object(&mut document, 301, 301, vec![]);
        let second_id = image_object(&mut document, 301, 301, vec![]);
        attach_image_page(
            &mut document,
            vec![("ImA", first_id), ("ImB", second_id)],
            b"/ImA Do /ImB Do",
        );
        let parsed = parse_image_pdf(save_image_pdf(&mut document));
        let spy = ResolveSpy {
            inner: parsed.as_ref(),
            xobject_resolves: Mutex::new(Vec::new()),
        };
        let mut budget = ImageBudget::new();
        budget.metadata_nodes = MAX_IMAGE_METADATA_NODES;
        let report = images_json(&spy, 0, &mut budget).unwrap();
        assert_eq!(report["status"], "partial", "{report}");
        assert_eq!(report["counts"]["names"], 1, "{report}");
        assert_eq!(report["counts"]["unprocessed"], 1, "{report}");
        assert_eq!(report["entries"].as_array().unwrap().len(), 1, "{report}");
        assert!(
            report["entries"][0]["image"]
                .as_object()
                .unwrap()
                .values()
                .any(|value| value["uninspected"] == "metadata budget"),
            "{report}"
        );
        let first_number = u64::from(first_id.0);
        let second_number = u64::from(second_id.0);
        let resolves = spy.xobject_resolves.lock().unwrap();
        assert!(
            resolves
                .iter()
                .any(|reference| u64::from(reference.object_number) == first_number),
            "the processed entry must be resolved: {resolves:?}"
        );
        assert!(
            resolves
                .iter()
                .all(|reference| u64::from(reference.object_number) != second_number),
            "the next image must not be resolved after a metadata stop: {resolves:?}"
        );
    }

    #[test]
    fn image_mode_invocation_limit_is_partial() {
        let mut document = lopdf::Document::with_version("1.5");
        let image_id = image_object(&mut document, 301, 301, vec![]);
        let content = b"/Im1 Do /Im1 Do /Im1 Do";
        attach_image_page(&mut document, vec![("Im1", image_id)], content);
        let pdf = parse_image_pdf(save_image_pdf(&mut document));
        let mut budget = ImageBudget::new();
        budget.invocations = MAX_IMAGE_INVOCATIONS;
        let report = images_json(pdf.as_ref(), 0, &mut budget).unwrap();
        assert_eq!(report["status"], "partial", "{report}");
        assert!(
            report["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == "image invocation limit")
        );
    }

    #[test]
    fn image_mode_large_keys_stay_bounded() {
        use lopdf::{Object, dictionary};
        let mut document = lopdf::Document::with_version("1.5");
        let huge_key = "K".repeat(10_000);
        let mut decode_parms = lopdf::Dictionary::new();
        decode_parms.set(huge_key, Object::Integer(1));
        let image_id = image_object(
            &mut document,
            301,
            301,
            vec![("DecodeParms", Object::Dictionary(decode_parms))],
        );
        attach_image_page(&mut document, vec![("Im1", image_id)], b"/Im1 Do");
        let pdf = parse_image_pdf(save_image_pdf(&mut document));
        let mut budget = ImageBudget::new();
        let report = images_json(pdf.as_ref(), 0, &mut budget).unwrap();
        let decode_parms = &report["entries"][0]["image"]["DecodeParms"];
        assert_eq!(decode_parms["uninspected"], "dictionary", "{report}");
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(
            serialized.len() < 4_096,
            "large keys must not be copied into output: {}",
            serialized.len()
        );
        assert_eq!(report["status"], "partial");
    }

    #[test]
    fn image_mode_rejects_page_selection_over_limit() {
        let pages = (0..=MAX_IMAGE_PAGES)
            .map(|page| page.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse_args(&["--images".into(), pages, "a.pdf".into()]).is_err());
        let (_, mode) = parse_args(&["--images".into(), "0,1".into(), "a.pdf".into()]).unwrap();
        assert!(matches!(mode, Mode::Images { pages } if pages == vec![0, 1]));
    }
}
