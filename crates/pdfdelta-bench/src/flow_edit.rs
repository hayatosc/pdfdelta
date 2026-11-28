//! Lossless content-stream edits and page merges for the metamorphic harness.
//!
//! Every function here constructs a PDF from validated byte-level evidence. A
//! caller must verify the constructed document independently (the harness
//! extracts it again and compares the glyph sequence) before treating a case as
//! valid. Unsupported constructs return an error string that becomes a
//! harness-skip reason.

use std::ops::Range;

use lopdf::{Dictionary, Document as LopdfDocument, Object, ObjectId, Stream, dictionary};

/// One text-show operator's decoded string operands.
#[derive(Clone, Debug)]
pub struct ShowStrings {
    /// Operator index within the content stream, matching glyph provenance.
    pub operator_index: u32,
    /// Decoded string bytes with their spans in the decoded content stream.
    pub strings: Vec<(Range<usize>, Vec<u8>)>,
}

#[derive(Clone, Debug)]
enum Operand {
    String { span: Range<usize>, bytes: Vec<u8> },
    Array(Vec<Self>),
    Other,
}

/// Parses a decoded content stream and returns the text-show strings.
///
/// Only `Tj`, `TJ`, `'`, and `"` are reported. The operator index counts every
/// operator in stream order, matching the extractor's glyph provenance. Inline
/// images are not supported because their binary payload would need its own
/// lexer; a stream containing `BI` is rejected.
///
/// # Errors
///
/// Returns a reason when the stream uses an unsupported or malformed construct.
pub fn parse_show_strings(content: &[u8]) -> Result<Vec<ShowStrings>, String> {
    let mut shows = Vec::new();
    let mut operands: Vec<Operand> = Vec::new();
    let mut index = 0u32;
    let mut position = 0usize;
    while let Some(byte) = content.get(position).copied() {
        if is_space(byte) {
            position += 1;
            continue;
        }
        if byte == b'%' {
            while position < content.len() && content[position] != b'\n' {
                position += 1;
            }
            continue;
        }
        if byte == b'(' {
            let (span, bytes) = parse_literal(content, position)?;
            position = span.end;
            operands.push(Operand::String { span, bytes });
            continue;
        }
        if byte == b'<' {
            if content.get(position + 1) == Some(&b'<') {
                position = skip_dictionary(content, position)?;
                operands.push(Operand::Other);
            } else {
                let (span, bytes) = parse_hex(content, position)?;
                position = span.end;
                operands.push(Operand::String { span, bytes });
            }
            continue;
        }
        if byte == b'[' {
            let (next, array) = parse_array(content, position)?;
            position = next;
            operands.push(Operand::Array(array));
            continue;
        }
        if byte == b']' || byte == b'>' || byte == b')' || byte == b'}' || byte == b'{' {
            return Err(format!("unexpected delimiter at byte {position}"));
        }
        if byte == b'/' {
            position = read_regular(content, position + 1).max(position + 1);
            operands.push(Operand::Other);
            continue;
        }
        let end = read_regular(content, position);
        if end == position {
            return Err(format!("empty token at byte {position}"));
        }
        let token = &content[position..end];
        position = end;
        if is_operand(token) {
            operands.push(Operand::Other);
            continue;
        }
        if token == b"BI" {
            return Err("inline image operator is unsupported".to_owned());
        }
        let current = index;
        index = index
            .checked_add(1)
            .ok_or_else(|| "operator index overflow".to_owned())?;
        if matches!(token, b"Tj" | b"TJ" | b"'" | b"\"") {
            let mut strings = Vec::new();
            if let Some(last) = operands.last() {
                collect_strings(last, &mut strings);
            }
            if !strings.is_empty() {
                shows.push(ShowStrings {
                    operator_index: current,
                    strings,
                });
            }
        }
        operands.clear();
    }
    Ok(shows)
}

fn collect_strings(operand: &Operand, out: &mut Vec<(Range<usize>, Vec<u8>)>) {
    match operand {
        Operand::String { span, bytes } => out.push((span.clone(), bytes.clone())),
        Operand::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Operand::Other => {}
    }
}

fn parse_array(content: &[u8], start: usize) -> Result<(usize, Vec<Operand>), String> {
    let mut operands = Vec::new();
    let mut position = start + 1;
    while let Some(byte) = content.get(position).copied() {
        if is_space(byte) {
            position += 1;
            continue;
        }
        if byte == b'%' {
            while position < content.len() && content[position] != b'\n' {
                position += 1;
            }
            continue;
        }
        if byte == b']' {
            return Ok((position + 1, operands));
        }
        if byte == b'(' {
            let (span, bytes) = parse_literal(content, position)?;
            position = span.end;
            operands.push(Operand::String { span, bytes });
            continue;
        }
        if byte == b'<' {
            if content.get(position + 1) == Some(&b'<') {
                position = skip_dictionary(content, position)?;
                operands.push(Operand::Other);
            } else {
                let (span, bytes) = parse_hex(content, position)?;
                position = span.end;
                operands.push(Operand::String { span, bytes });
            }
            continue;
        }
        if byte == b'[' {
            let (next, array) = parse_array(content, position)?;
            position = next;
            operands.push(Operand::Array(array));
            continue;
        }
        if byte == b'/' {
            position = read_regular(content, position + 1).max(position + 1);
            operands.push(Operand::Other);
            continue;
        }
        let end = read_regular(content, position);
        if end == position {
            return Err(format!("empty array token at byte {position}"));
        }
        let token = &content[position..end];
        position = end;
        if is_operand(token) {
            operands.push(Operand::Other);
        } else {
            return Err(format!(
                "operator inside array at byte {position}: {}",
                String::from_utf8_lossy(token)
            ));
        }
    }
    Err("unterminated array".to_owned())
}

fn parse_literal(content: &[u8], start: usize) -> Result<(Range<usize>, Vec<u8>), String> {
    let mut output = Vec::new();
    let mut depth = 1usize;
    let mut position = start + 1;
    while position < content.len() {
        match content[position] {
            b'\\' => {
                position += 1;
                let Some(&escaped) = content.get(position) else {
                    return Err("unterminated escape in literal string".to_owned());
                };
                match escaped {
                    b'n' => output.push(b'\n'),
                    b'r' => output.push(b'\r'),
                    b't' => output.push(b'\t'),
                    b'b' => output.push(0x08),
                    b'f' => output.push(0x0c),
                    b'(' => output.push(b'('),
                    b')' => output.push(b')'),
                    b'\\' => output.push(b'\\'),
                    b'\r' => {
                        if content.get(position + 1) == Some(&b'\n') {
                            position += 1;
                        }
                    }
                    b'\n' => {}
                    b'0'..=b'7' => {
                        let mut value = u16::from(escaped - b'0');
                        for _ in 0..2 {
                            let Some(&next) = content.get(position + 1) else {
                                break;
                            };
                            if !(b'0'..=b'7').contains(&next) {
                                break;
                            }
                            value = value * 8 + u16::from(next - b'0');
                            position += 1;
                        }
                        output.push(u8::try_from(value & 0xff).expect("masked to u8"));
                    }
                    other => output.push(other),
                }
                position += 1;
            }
            b'(' => {
                depth += 1;
                output.push(b'(');
                position += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((start..position + 1, output));
                }
                output.push(b')');
                position += 1;
            }
            other => {
                output.push(other);
                position += 1;
            }
        }
    }
    Err("unterminated literal string".to_owned())
}

fn parse_hex(content: &[u8], start: usize) -> Result<(Range<usize>, Vec<u8>), String> {
    let mut output = Vec::new();
    let mut high: Option<u8> = None;
    let mut position = start + 1;
    while position < content.len() {
        let byte = content[position];
        if byte == b'>' {
            if let Some(high) = high {
                output.push(high << 4);
            }
            return Ok((start..position + 1, output));
        }
        if is_space(byte) {
            position += 1;
            continue;
        }
        let Some(value) = hex_value(byte) else {
            return Err(format!("invalid hex digit at byte {position}"));
        };
        if let Some(high) = high.take() {
            output.push((high << 4) | value);
        } else {
            high = Some(value);
        }
        position += 1;
    }
    Err("unterminated hex string".to_owned())
}

fn skip_dictionary(content: &[u8], start: usize) -> Result<usize, String> {
    let mut depth = 1usize;
    let mut position = start + 2;
    while position < content.len() {
        match content[position] {
            b'(' => {
                let (span, _) = parse_literal(content, position)?;
                position = span.end;
            }
            b'<' => {
                if content.get(position + 1) == Some(&b'<') {
                    depth += 1;
                    position += 2;
                } else {
                    let (span, _) = parse_hex(content, position)?;
                    position = span.end;
                }
            }
            b'>' => {
                if content.get(position + 1) == Some(&b'>') {
                    depth -= 1;
                    position += 2;
                    if depth == 0 {
                        return Ok(position);
                    }
                } else {
                    position += 1;
                }
            }
            _ => position += 1,
        }
    }
    Err("unterminated dictionary".to_owned())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn read_regular(content: &[u8], start: usize) -> usize {
    let mut position = start;
    while position < content.len() {
        let byte = content[position];
        if is_space(byte) || is_delimiter(byte) {
            break;
        }
        position += 1;
    }
    position
}

fn is_operand(token: &[u8]) -> bool {
    if matches!(token, b"true" | b"false" | b"null") {
        return true;
    }
    let text = std::str::from_utf8(token).unwrap_or("");
    if text.is_empty() {
        return false;
    }
    let mut chars = text.chars();
    let first = chars.next().unwrap_or('x');
    if first == '+' || first == '-' || first == '.' || first.is_ascii_digit() {
        return text.parse::<f64>().is_ok();
    }
    false
}

/// Plans one lossless single-byte edit in a decoded content stream.
#[derive(Clone, Debug)]
pub struct TextEditPlan {
    /// Page containing the edited glyph (0-based).
    pub page: u32,
    /// Content-stream object to rewrite.
    pub stream: ObjectId,
    /// Span of the string literal in the decoded stream content.
    pub span: Range<usize>,
    /// Replacement string literal bytes (a hex string).
    pub replacement: Vec<u8>,
    /// Original digit character and its single-byte code.
    pub old_digit: char,
    pub old_code: u8,
    /// Replacement digit character and its single-byte code.
    pub new_digit: char,
    pub new_code: u8,
    /// Diff token containing the original digit.
    pub old_token: String,
    /// Diff token after the digit replacement.
    pub new_token: String,
}

/// Encodes bytes as a PDF hex string literal.
#[must_use]
pub fn hex_literal(bytes: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Vec::with_capacity(bytes.len() * 2 + 2);
    out.push(b'<');
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)]);
        out.push(HEX[usize::from(byte & 0x0f)]);
    }
    out.push(b'>');
    out
}

/// Applies a planned text edit to the PDF in memory.
///
/// The stream is decoded, the string literal is replaced with the prepared hex
/// string, and the stream is written back as plain content before optional
/// Flate compression. The decoded bytes differ only at the edited code.
///
/// # Errors
///
/// Returns a reason when the stream cannot be decoded or the span is stale.
pub fn apply_text_edit(pdf: &mut LopdfDocument, plan: &TextEditPlan) -> Result<(), String> {
    let object = pdf
        .get_object_mut(plan.stream)
        .map_err(|error| format!("cannot read content stream: {error}"))?;
    let stream = object
        .as_stream_mut()
        .map_err(|error| format!("content object is not a stream: {error}"))?;
    let mut content = stream
        .get_plain_content_with_limit(64 * 1024 * 1024)
        .map_err(|error| format!("cannot decode content stream: {error}"))?;
    if plan.span.end > content.len() || plan.span.start > plan.span.end {
        return Err("text-edit span is outside the decoded stream".to_owned());
    }
    content.splice(plan.span.clone(), plan.replacement.iter().copied());
    stream.set_plain_content(content);
    // Re-compress only when it shrinks the stream; set_plain_content cleared
    // the old filter, so this is a lossless Flate re-encoding.
    stream
        .compress()
        .map_err(|error| format!("cannot re-encode content stream: {error}"))?;
    Ok(())
}

/// A validated page-merge geometry.
#[derive(Clone, Copy, Debug)]
pub struct MergeGeometry {
    /// Width of both pages.
    pub width: f64,
    /// Height of the upper (left) page.
    pub left_height: f64,
    /// Height of the lower (right) page.
    pub right_height: f64,
}

/// Checks that two adjacent pages can be merged without losing content.
///
/// # Errors
///
/// Returns a skip reason for unequal widths, non-zero boxes, rotation, groups,
/// annotations, unsupported units or unmergeable resource names.
pub fn merge_eligible(pdf: &LopdfDocument, left: u32, right: u32) -> Result<MergeGeometry, String> {
    let left_id = page_id(pdf, left)?;
    let right_id = page_id(pdf, right)?;
    let left_box = media_box(pdf, left_id)?;
    let right_box = media_box(pdf, right_id)?;
    if left_box.2 != right_box.2 {
        return Err("merged pages have different widths".to_owned());
    }
    let geometry = MergeGeometry {
        width: left_box.2,
        left_height: left_box.3,
        right_height: right_box.3,
    };
    // Reject an explicit CropBox that does not match the MediaBox: expanding
    // it during the merge could reveal content the source page hid.
    if let Ok(crop) = pdf
        .get_dictionary(left_id)
        .ok()
        .and_then(|dictionary| dictionary.get(b"CropBox").ok().cloned())
        .ok_or(())
    {
        let resolved = resolve_array(pdf, &crop)?;
        let values =
            [0, 1, 2, 3].map(|index| resolved[index].as_float().map_or(f64::NAN, f64::from));
        if values != [0.0, 0.0, left_box.2, left_box.3] {
            return Err("left page CropBox differs from its MediaBox".to_owned());
        }
    }
    let left_resources = page_resources(pdf, left_id)?;
    let right_resources = page_resources(pdf, right_id)?;
    merge_resources(pdf, &left_resources, &right_resources)?;
    Ok(geometry)
}

/// Merges page `right` into `left` and removes the right page.
///
/// The left page's content is translated above the right page's content on a
/// taller page box. Resource dictionaries are merged only when their names do
/// not collide with different objects; content streams are kept as references.
///
/// # Errors
///
/// Returns a reason when a required structure is missing or unsafe to merge.
pub fn merge_pages(pdf: &mut LopdfDocument, left: u32, right: u32) -> Result<(), String> {
    let geometry = merge_eligible(pdf, left, right)?;
    let left_id = page_id(pdf, left)?;
    let right_id = page_id(pdf, right)?;
    let left_resources = page_resources(pdf, left_id)?;
    let right_resources = page_resources(pdf, right_id)?;
    let merged_resources = merge_resources(pdf, &left_resources, &right_resources)?;
    let left_contents = content_refs(pdf, left_id)?;
    let right_contents = content_refs(pdf, right_id)?;

    let translate = format!("q 1 0 0 1 0 {:.6} cm\n", geometry.right_height).into_bytes();
    let close = b"Q\n".to_vec();
    let open = b"q\n".to_vec();
    let translate_id = pdf.add_object(Stream::new(dictionary! {}, translate));
    let close_id = pdf.add_object(Stream::new(dictionary! {}, close.clone()));
    let open_id = pdf.add_object(Stream::new(dictionary! {}, open));
    let close_id2 = pdf.add_object(Stream::new(dictionary! {}, close));

    let mut contents = Vec::with_capacity(left_contents.len() + right_contents.len() + 4);
    contents.push(Object::Reference(translate_id));
    contents.extend(left_contents.into_iter().map(Object::Reference));
    contents.push(Object::Reference(close_id));
    contents.push(Object::Reference(open_id));
    contents.extend(right_contents.into_iter().map(Object::Reference));
    contents.push(Object::Reference(close_id2));

    {
        let dictionary = pdf
            .get_object_mut(left_id)
            .and_then(Object::as_dict_mut)
            .map_err(|error| format!("cannot read left page dictionary: {error}"))?;
        dictionary.set(b"Contents".to_vec(), Object::Array(contents));
        dictionary.set(b"Resources".to_vec(), Object::Dictionary(merged_resources));
        let box_object = Object::Array(vec![
            0.into(),
            0.into(),
            number(geometry.width),
            number(geometry.left_height + geometry.right_height),
        ]);
        dictionary.set(b"MediaBox".to_vec(), box_object.clone());
        if dictionary.get(b"CropBox".as_slice()).is_ok() {
            dictionary.set(b"CropBox".to_vec(), box_object);
        }
    }
    pdf.delete_pages(&[right]);
    Ok(())
}

fn page_id(pdf: &LopdfDocument, page: u32) -> Result<ObjectId, String> {
    pdf.get_pages()
        .get(&page)
        .copied()
        .ok_or_else(|| format!("page {page} does not exist"))
}

fn media_box(pdf: &LopdfDocument, page_id: ObjectId) -> Result<(f64, f64, f64, f64), String> {
    let dictionary = pdf
        .get_dictionary(page_id)
        .map_err(|error| format!("cannot read page dictionary: {error}"))?;
    for key in [b"MediaBox".as_slice(), b"CropBox".as_slice()] {
        if let Ok(value) = dictionary.get(key) {
            let resolved = resolve_array(pdf, value)?;
            if resolved.len() != 4 {
                return Err("page box does not have four numbers".to_owned());
            }
            let mut numbers = [0.0f64; 4];
            for (index, slot) in numbers.iter_mut().enumerate() {
                *slot = f64::from(
                    resolved[index]
                        .as_float()
                        .map_err(|error| format!("page box number: {error}"))?,
                );
            }
            if numbers[0] != 0.0 || numbers[1] != 0.0 {
                return Err("page box does not start at (0,0)".to_owned());
            }
            if numbers[2] <= 0.0 || numbers[3] <= 0.0 {
                return Err("page box has a non-positive size".to_owned());
            }
            if dictionary.get(b"Rotate".as_slice()).is_ok_and(|rotate| {
                rotate
                    .as_i64()
                    .map_or(true, |value| value.rem_euclid(360) != 0)
            }) {
                return Err("rotated pages are unsupported".to_owned());
            }
            if dictionary.get(b"Group".as_slice()).is_ok() {
                return Err("page transparency groups are unsupported".to_owned());
            }
            if dictionary.get(b"Annots".as_slice()).is_ok() {
                return Err("pages with annotations are unsupported".to_owned());
            }
            if dictionary.get(b"UserUnit".as_slice()).is_ok() {
                return Err("pages with UserUnit are unsupported".to_owned());
            }
            return Ok((numbers[0], numbers[1], numbers[2], numbers[3]));
        }
    }
    Err("page has no MediaBox".to_owned())
}

fn resolve_array<'a>(pdf: &'a LopdfDocument, value: &'a Object) -> Result<&'a Vec<Object>, String> {
    if let Ok(reference) = value.as_reference() {
        return pdf
            .get_object(reference)
            .and_then(Object::as_array)
            .map_err(|error| format!("page box reference: {error}"));
    }
    value
        .as_array()
        .map_err(|error| format!("page box value: {error}"))
}

fn page_resources(pdf: &LopdfDocument, page_id: ObjectId) -> Result<Dictionary, String> {
    // Check the page's own Resources first: it may be an indirect reference,
    // which `get_page_resources` does not return as a direct dictionary.
    // Then walk the parent chain for inherited resources.
    let mut current = page_id;
    for _ in 0..64 {
        let dictionary = pdf
            .get_dictionary(current)
            .map_err(|error| format!("cannot read resources holder: {error}"))?;
        if let Ok(resources) = dictionary.get(b"Resources") {
            if let Ok(reference) = resources.as_reference() {
                return pdf
                    .get_dictionary(reference)
                    .cloned()
                    .map_err(|error| format!("cannot resolve resources: {error}"));
            }
            if let Ok(direct) = resources.as_dict() {
                return Ok(direct.clone());
            }
        }
        let Some(parent) = dictionary
            .get(b"Parent")
            .ok()
            .and_then(|parent| parent.as_reference().ok())
        else {
            break;
        };
        current = parent;
    }
    Ok(Dictionary::new())
}

fn merge_resources(
    pdf: &LopdfDocument,
    left: &Dictionary,
    right: &Dictionary,
) -> Result<Dictionary, String> {
    let mut merged = Dictionary::new();
    for (key, value) in left {
        merged.set(key.clone(), value.clone());
    }
    for (key, value) in right {
        if key.as_slice() == b"ProcSet" {
            continue;
        }
        let right_map = resolve_dictionary(pdf, value)?;
        if let Some(existing) = merged.get(key.as_slice()).ok().cloned() {
            let mut left_map = resolve_dictionary(pdf, &existing)?;
            for (name, object) in &right_map {
                if let Ok(previous) = left_map.get(name.as_slice()) {
                    if previous != object {
                        return Err(format!(
                            "resource name collision in /{}",
                            String::from_utf8_lossy(key)
                        ));
                    }
                } else {
                    left_map.set(name.clone(), object.clone());
                }
            }
            merged.set(key.clone(), Object::Dictionary(left_map));
        } else {
            merged.set(key.clone(), Object::Dictionary(right_map));
        }
    }
    Ok(merged)
}

fn resolve_dictionary(pdf: &LopdfDocument, value: &Object) -> Result<Dictionary, String> {
    if let Ok(reference) = value.as_reference() {
        return pdf
            .get_dictionary(reference)
            .cloned()
            .map_err(|error| format!("cannot resolve resource dictionary: {error}"));
    }
    value
        .as_dict()
        .cloned()
        .map_err(|error| format!("resource entry is not a dictionary: {error}"))
}

fn content_refs(pdf: &LopdfDocument, page_id: ObjectId) -> Result<Vec<ObjectId>, String> {
    let dictionary = pdf
        .get_dictionary(page_id)
        .map_err(|error| format!("cannot read page dictionary: {error}"))?;
    let Ok(contents) = dictionary.get(b"Contents") else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    match contents {
        Object::Reference(id) => ids.push(*id),
        Object::Array(items) => {
            for item in items {
                let reference = item
                    .as_reference()
                    .map_err(|error| format!("page content is not a stream reference: {error}"))?;
                ids.push(reference);
            }
        }
        _ => return Err("page content is not a stream reference".to_owned()),
    }
    Ok(ids)
}

fn number(value: f64) -> Object {
    Object::Real(value as f32)
}
