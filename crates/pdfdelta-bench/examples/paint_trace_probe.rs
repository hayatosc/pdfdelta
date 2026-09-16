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

use pdfdelta_core::pdf::{LopdfParser, ParseLimits, ParsedPdf, PdfObject, PdfParser};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type ProbeResult<T> = Result<T, Box<dyn std::error::Error>>;

const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESOURCE_VISITS: usize = 100_000;

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

fn main() -> ProbeResult<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let (path, selected_page) = match args.as_slice() {
        [path] => (path, None),
        [path, page] => (path, Some(page.parse::<usize>()?)),
        _ => return Err("usage: paint_trace_probe INPUT.pdf [DUMP_ZERO_BASED_PAGE]".into()),
    };
    let limits = ParseLimits::default();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(u64::try_from(limits.max_input_bytes)? + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_input_bytes {
        return Err("input byte limit".into());
    }
    let input_digest = digest(&bytes);
    let pdf = LopdfParser.parse(Arc::from(bytes), limits)?;
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
    use super::trace;

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
