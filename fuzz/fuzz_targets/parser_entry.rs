#![no_main]

use std::collections::HashSet;
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use pdfdelta_core::pdf::{
    LopdfParser, ObjectRef, ParseLimits, ParsedPdf, PdfDict, PdfObject, PdfParser,
    decode_text_string,
};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_PAGES_TO_VISIT: usize = 16;
/// Bound on distinct object references whose resolution and streams are
/// exercised. Loading already bounded the object count; this only limits the
/// extra per-reference work.
const MAX_VISITED_REFERENCES: usize = 64;
const MAX_OBJECT_WALK_DEPTH: usize = 16;
const MAX_DECODED_TEXT_BYTES: usize = 4 * 1024;

const LIMITS: ParseLimits = ParseLimits {
    max_input_bytes: MAX_INPUT_BYTES,
    max_objects: 2_048,
    max_recursion_depth: 16,
    max_decoded_stream_bytes: 64 * 1024,
    max_total_object_stream_bytes: 256 * 1024,
    max_pages: MAX_PAGES_TO_VISIT,
};

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let parser = LopdfParser;
    let Ok(pdf) = parser.parse(Arc::from(data), LIMITS) else {
        return;
    };
    let _ = pdf.version();
    let _ = pdf.issues();

    let mut queue = Vec::new();
    if let Ok(trailer) = pdf.trailer() {
        collect_references(&trailer, &mut queue);
    }
    if let Ok(pages) = pdf.pages() {
        assert!(pages.len() <= LIMITS.max_pages);
        for page in pages.into_iter().take(MAX_PAGES_TO_VISIT) {
            if let Ok(dictionary) = pdf.page_dict(page) {
                collect_references(&dictionary, &mut queue);
            }
        }
    }

    let mut visited = HashSet::new();
    while let Some(reference) = queue.pop() {
        if visited.len() >= MAX_VISITED_REFERENCES {
            break;
        }
        if !visited.insert(reference) {
            continue;
        }
        exercise_reference(pdf.as_ref(), reference, &mut queue);
    }
});

/// Resolves one reference and exercises the stream and text-decoding facades.
///
/// Newly discovered references are appended to `queue`; the caller owns the
/// worklist, so this function does not recurse.
fn exercise_reference(pdf: &dyn ParsedPdf, reference: ObjectRef, queue: &mut Vec<ObjectRef>) {
    let Ok(resolved) = pdf.resolve_with_terminal(reference) else {
        // A reference that resolves to nothing must also be reported as
        // terminal-free by the dedicated accessor.
        assert!(pdf.terminal_reference(reference).is_err());
        return;
    };
    assert_eq!(
        pdf.terminal_reference(reference).ok(),
        Some(resolved.reference)
    );
    assert_eq!(pdf.resolve(reference).ok(), Some(resolved.object.clone()));
    collect_references_from_object(&resolved.object, 0, &mut |found| {
        if queue.len() < MAX_VISITED_REFERENCES {
            queue.push(found);
        }
    });

    if let Ok(stream) = pdf.decoded_stream(reference) {
        assert!(stream.bytes.len() <= LIMITS.max_decoded_stream_bytes);
        let _ = stream.dictionary.len();
    }
    let _ = pdf.raw_stream(reference);
}

fn collect_references(dictionary: &PdfDict, references: &mut Vec<ObjectRef>) {
    for value in dictionary.values() {
        collect_object_references(value, 0, references);
    }
}

fn collect_object_references(object: &PdfObject, depth: usize, references: &mut Vec<ObjectRef>) {
    collect_references_from_object(object, depth, &mut |reference| {
        if references.len() < MAX_VISITED_REFERENCES {
            references.push(reference);
        }
    });
}

fn collect_references_from_object(
    object: &PdfObject,
    depth: usize,
    visit: &mut impl FnMut(ObjectRef),
) {
    if depth > MAX_OBJECT_WALK_DEPTH {
        return;
    }
    match object {
        PdfObject::Reference(reference) => visit(*reference),
        PdfObject::Array(items) => {
            for item in items {
                collect_references_from_object(item, depth + 1, visit);
            }
        }
        PdfObject::Dictionary(dictionary) | PdfObject::Stream(dictionary) => {
            for value in dictionary.values() {
                collect_references_from_object(value, depth + 1, visit);
            }
        }
        PdfObject::String(bytes) => {
            if let Ok(text) = decode_text_string(bytes, MAX_DECODED_TEXT_BYTES) {
                assert!(text.len() <= MAX_DECODED_TEXT_BYTES);
            }
        }
        PdfObject::Null
        | PdfObject::Boolean(_)
        | PdfObject::Integer(_)
        | PdfObject::Real(_)
        | PdfObject::Name(_) => {}
    }
}
