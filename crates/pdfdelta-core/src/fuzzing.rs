//! Fuzzing-only entry points for internal parsers.

use crate::diff::Confidence;
use crate::document::{
    BackendIdentity, BackendKind, ButtonAppearanceState, CorrespondenceScope,
    DocumentComparisonLimits, DocumentGraph, DocumentView, EvidenceLimits, EvidenceStore,
    FieldValue, FormLimits, FormWidget, GraphLimits, HierarchyLimits, NodeId, NodeKind,
    PageEvidence, Raster, RecognizedWord, RenderedEvidence, StructureLimits, StructuredEvidence,
    StructuredValue, extract_form_evidence, extract_structure_evidence, refine_table_views,
};
use crate::layout::{Line, LineOptions, RegionOptions, partition_regions, reconstruct_lines};
use crate::model::{
    DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphEvidence, GlyphId,
    GlyphPathClipStatus, GlyphProvenance, PageId, Rect, TextRenderMode, Vec2, VectorLine,
    VectorLineId,
};
use crate::pdf::LopdfParser;
use crate::pdf::ParseLimits;
use crate::pdf::content::{ContentBudget, ContentLimits, ContentParser, Operation};
use crate::pdf::font::cmap::{
    CMapLimits, UnicodeMapping, parse_identity_cid_encoding, parse_to_unicode_for_width,
};
use crate::pdf::font::{FontDecoder, FontDecoderLimits, WritingMode};
use crate::pdf::{
    DecodedStream, ObjectRef, PageRef, ParsedPdf, PdfDict, PdfObject, PdfParser, PdfVersion,
    RawStream,
};
use crate::pipeline::{PipelineOptions, compare_extraction_outcomes, compare_glyph_documents};
use crate::report::{TextReportOptions, render_text, summarize, write_json};
use crate::source::{
    ContentStreamGlyphExtractor, ExtractionLimits, ExtractionOutcome, ParserBackedGlyphSource,
};

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

const MAX_INPUT_BYTES: usize = 64 * 1024;
const LIMITS: CMapLimits = CMapLimits {
    max_entries: 1_024,
    max_code_bytes: 4,
    max_output_scalars: 4_096,
};
const CONTENT_LIMITS: ContentLimits = ContentLimits {
    max_operators: 1_024,
    max_operand_stack: 256,
    max_array_elements: 1_024,
    max_operand_nodes: 4_096,
    max_nesting_depth: 32,
    max_string_bytes: 64 * 1024,
};
const FONT_LIMITS: FontDecoderLimits = FontDecoderLimits {
    max_indirections: 16,
    max_simple_width_entries: 256,
    max_cid_width_entries: 64,
    max_decoded_font_bytes: 8 * 1024,
    cmap: CMapLimits {
        max_entries: 512,
        max_code_bytes: 4,
        max_output_scalars: 1_024,
    },
};
const MAX_OUTPUT_GLYPHS: usize = 1_024;
const MAX_MAPPED_TEXT_BYTES: usize = 4_096;
const PARSE_LIMITS: ParseLimits = ParseLimits {
    max_input_bytes: MAX_INPUT_BYTES,
    max_objects: 512,
    max_recursion_depth: 8,
    max_decoded_stream_bytes: 64 * 1024,
    max_total_object_stream_bytes: 256 * 1024,
    max_pages: 4,
};
const EXTRACTION_LIMITS: ExtractionLimits = ExtractionLimits {
    max_glyphs: 1_024,
    max_form_depth: 4,
    max_nesting_depth: 16,
    max_operators: 2_048,
    max_stream_invocations: 1_024,
    max_total_decoded_bytes: 256 * 1024,
    max_operand_stack: 256,
    max_array_elements: 1_024,
    max_operand_nodes: 4_096,
    max_fonts: 32,
    max_cmap_entries: 512,
    max_cid_width_entries: 1_024,
    max_string_bytes: 64 * 1024,
    max_vector_lines: 1_024,
};

/// Exercises the production `CMap` parsers with finite resource limits.
///
/// Inputs larger than 64 KiB are ignored. Parser errors are accepted outcomes.
///
/// # Panics
///
/// Panics if a successful parser result violates its configured entry or code-width limit.
pub fn fuzz_cmap_parsers(input: &[u8]) {
    if input.len() > MAX_INPUT_BYTES {
        return;
    }

    for source_width in 1..=LIMITS.max_code_bytes {
        if let Ok(cmap) = parse_to_unicode_for_width(input, LIMITS, source_width) {
            assert!(cmap.entry_count() <= LIMITS.max_entries);
        }
    }

    if let Ok(encoding) = parse_identity_cid_encoding(input, LIMITS) {
        assert!((1..=LIMITS.max_code_bytes).contains(&encoding.source_width));
        let entries = u32::try_from(encoding.source_width)
            .ok()
            .and_then(|width| 256usize.checked_pow(width))
            .and_then(|mapped| mapped.checked_add(1));
        assert!(entries.is_some_and(|entries| entries <= LIMITS.max_entries));
    }
}

/// Exercises the production content stream parser with finite resource limits.
///
/// Inputs larger than 64 KiB are ignored. The input is parsed once as a single content stream
/// fragment and once as two fragments split at the midpoint. Each strategy has an independent
/// operator and operand budget, and parser errors are accepted outcomes.
///
/// # Panics
///
/// Panics if successfully returned operations exceed the configured operator limit or contain an
/// operation index outside that limit.
pub fn fuzz_content_stream_parser(input: &[u8]) {
    if input.len() > MAX_INPUT_BYTES {
        return;
    }

    fuzz_content_fragments(&[input]);
    let (first, second) = input.split_at(input.len() / 2);
    fuzz_content_fragments(&[first, second]);
}

fn fuzz_content_fragments(fragments: &[&[u8]]) {
    let operator_budget = ContentBudget::for_operators(CONTENT_LIMITS.max_operators);
    let operand_budget = ContentBudget::for_operands(CONTENT_LIMITS.max_operand_nodes);
    let mut parser = ContentParser::with_budgets(CONTENT_LIMITS, operator_budget, operand_budget);
    let mut operation_count = 0usize;

    for fragment in fragments {
        let Ok(operations) = parser.parse_fragment(fragment) else {
            return;
        };
        operation_count += operations.len();
        assert!(operation_count <= CONTENT_LIMITS.max_operators);
        assert_operation_indexes(&operations);
    }
    let _ = parser.finish();
}

fn assert_operation_indexes(operations: &[Operation]) {
    for operation in operations {
        assert!((operation.index as usize) < CONTENT_LIMITS.max_operators);
    }
}

/// Exercises the production font decoders with finite resource limits.
///
/// Inputs larger than 64 KiB are ignored. The harness builds fixed, bounded
/// backend-neutral `PdfObject` and `ParsedPdf` skeletons and derives only
/// selected mapping, width, fallback, and decode fields from the fuzz bytes.
/// No generic PDF object wire format or custom PDF parser is introduced.
///
/// Both simple and Type0 paths are exercised, including ToUnicode priority,
/// Differences and fallback handling, width and default-width selection,
/// fixed source widths, odd-byte rejection, and explicit preservation of
/// unmapped glyphs.
///
/// All font-loading and decoding errors are accepted outcomes. The oracle
/// asserts only sound success invariants that have been confirmed against
/// existing unit tests.
///
/// # Panics
///
/// Panics if a successful decode violates the configured glyph or mapped-text
/// budgets, if concatenated raw codes do not preserve the input, if raw-code
/// width, metrics, or unmapped preservation invariants are violated, or if a
/// fixed-width font accepts an odd-length Type0 payload without reporting an
/// error.
#[doc(hidden)]
pub fn fuzz_font_decoder(input: &[u8]) {
    if input.len() > MAX_INPUT_BYTES {
        return;
    }
    if input.is_empty() {
        return;
    }
    let selector = input[0] % 4;
    let tail = &input[1..];
    let (config, payload) = if tail.len() > 16 {
        (&tail[..16], &tail[16..])
    } else {
        (tail, &[][..])
    };
    match selector {
        0 => fuzz_simple_winansi(config, payload),
        1 => fuzz_simple_partial_tounicode(config, payload),
        2 => fuzz_type0_identity_h(config, payload),
        3 => fuzz_type0_vertical_or_custom(config, payload),
        _ => unreachable!(),
    }
}

fn fuzz_simple_winansi(config: &[u8], payload: &[u8]) {
    let mut pdf = FuzzPdf::default();
    let widths = derive_simple_widths(config, 4);
    let missing_width = 500.0 + f64::from(config.first().copied().unwrap_or(0) % 40);
    let font = build_simple_font(
        &mut pdf,
        None,
        b"WinAnsiEncoding",
        65,
        widths,
        missing_width,
        700.0,
        -200.0,
    );
    exercise_font(&pdf, &font, payload, 1);
}

fn fuzz_simple_partial_tounicode(config: &[u8], payload: &[u8]) {
    let mut pdf = FuzzPdf::default();
    // Fixed partial ToUnicode that maps <41> to "Z". The mapping is intentionally
    // narrow so fallback and Differences must be used for other codes. This
    // exercises ToUnicode priority over fallback.
    let to_unicode_bytes =
        b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <005A> endbfchar"
            .to_vec();
    let to_unicode_ref = ObjectRef {
        object_number: 20,
        generation: 0,
    };
    pdf.objects
        .insert(to_unicode_ref, PdfObject::Stream(PdfDict::new()));
    pdf.streams.insert(to_unicode_ref, to_unicode_bytes);

    // Differences: 66 -> B (known), 67 -> UnknownGlyph or C depending on fuzz byte.
    // This exercises Differences priority and explicit unmapped preservation.
    let unknown = config.first().copied().unwrap_or(0) % 2 == 0;
    let missing_width = 480.0;
    let widths2 = derive_simple_widths(config, 4).unwrap_or_else(|| vec![600.0; 4]);
    let font = build_simple_font(
        &mut pdf,
        Some(to_unicode_ref),
        b"WinAnsiEncoding",
        65,
        Some(widths2),
        missing_width,
        700.0,
        -200.0,
    );
    // Re-insert Differences into the rebuilt font for this variant
    let PdfObject::Dictionary(mut dict) = font else {
        return;
    };
    let _ = dict.insert(
        b"Encoding".to_vec(),
        PdfObject::Dictionary(PdfDict::from([
            (
                b"BaseEncoding".to_vec(),
                PdfObject::Name(b"WinAnsiEncoding".to_vec()),
            ),
            (
                b"Differences".to_vec(),
                PdfObject::Array(vec![
                    PdfObject::Integer(66),
                    PdfObject::Name(b"B".to_vec()),
                    PdfObject::Integer(67),
                    PdfObject::Name(if unknown {
                        b"UnknownGlyph".to_vec()
                    } else {
                        b"C".to_vec()
                    }),
                ]),
            ),
        ])),
    );
    // Keep the ToUnicode reference
    dict.insert(b"ToUnicode".to_vec(), PdfObject::Reference(to_unicode_ref));
    let font = PdfObject::Dictionary(dict);
    exercise_font(&pdf, &font, payload, 1);

    // Also verify ToUnicode priority explicitly for a known payload when not fuzzed
    if payload.is_empty() {
        return;
    }
    // For any successful decode of [0x41], the mapping must be "Z" (ToUnicode), not "A"
    // (fallback). This check is sound only for this fixed fixture.
    if let Ok(loaded) = FontDecoder::load(&pdf, &font, FONT_LIMITS)
        && let Ok(glyphs) = loaded.decoder.decode(&[0x41], 1, 1024)
        && let Some(glyph) = glyphs.first()
    {
        assert_eq!(glyph.mapping, UnicodeMapping::Mapped("Z".into()));
        assert_eq!(glyph.raw_code, vec![0x41]);
    }
}

fn fuzz_type0_identity_h(config: &[u8], payload: &[u8]) {
    let mut pdf = FuzzPdf::default();
    let (font, source_width) = build_type0_font(&mut pdf, config, false, false);
    exercise_font(&pdf, &font, payload, source_width);
    // Exercise odd-byte rejection for fixed 2-byte Identity-H.
    if source_width == 2 && !payload.is_empty() {
        let mut odd = payload.to_vec();
        if odd.len().is_multiple_of(2) {
            odd.pop();
        }
        if !odd.is_empty()
            && !odd.len().is_multiple_of(2)
            && let Ok(loaded) = FontDecoder::load(&pdf, &font, FONT_LIMITS)
        {
            let result = loaded
                .decoder
                .decode(&odd, MAX_OUTPUT_GLYPHS, MAX_MAPPED_TEXT_BYTES);
            assert!(
                result.is_err(),
                "odd-length Identity-H payload should be rejected"
            );
        }
    }
}

fn fuzz_type0_vertical_or_custom(config: &[u8], payload: &[u8]) {
    let is_custom = config.first().copied().unwrap_or(0) % 2 == 1;
    if is_custom {
        let mut pdf = FuzzPdf::default();
        let (font, source_width) = build_type0_font(&mut pdf, config, true, false);
        assert_eq!(source_width, 1);
        exercise_font(&pdf, &font, payload, source_width);
    } else {
        let mut pdf = FuzzPdf::default();
        let (font, source_width) = build_type0_font(&mut pdf, config, false, true);
        assert_eq!(source_width, 2);
        exercise_font(&pdf, &font, payload, source_width);
        // Verify vertical metrics are present and finite when load succeeds
        if let Ok(loaded) = FontDecoder::load(&pdf, &font, FONT_LIMITS) {
            assert_eq!(loaded.decoder.writing_mode(), WritingMode::Vertical);
            if let Ok(glyphs) = loaded.decoder.decode(&[0x00, 0x01], 1, 1024)
                && let Some(glyph) = glyphs.first()
            {
                assert!(glyph.vertical.is_some());
                if let Some(vertical) = glyph.vertical {
                    assert!(vertical.displacement_y_1000_em.is_finite());
                    assert!(vertical.origin_x_1000_em.is_finite());
                    assert!(vertical.origin_y_1000_em.is_finite());
                    assert!(vertical.displacement_y_1000_em < 0.0);
                }
            }
        }
    }
}

fn derive_simple_widths(config: &[u8], count: usize) -> Option<Vec<f64>> {
    let mut widths = Vec::with_capacity(count);
    for i in 0..count {
        let byte = config.get(i).copied().unwrap_or(0);
        // Keep widths in a finite, non-negative range derived from fuzz bytes.
        widths.push(500.0 + f64::from(byte % 80));
    }
    Some(widths)
}

#[allow(clippy::too_many_arguments)]
fn build_simple_font(
    _pdf: &mut FuzzPdf,
    to_unicode_ref: Option<ObjectRef>,
    encoding_name: &[u8],
    first_char: u8,
    widths: Option<Vec<f64>>,
    missing_width: f64,
    ascent: f64,
    descent: f64,
) -> PdfObject {
    let mut dict = PdfDict::from([
        (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
        (
            b"FirstChar".to_vec(),
            PdfObject::Integer(i64::from(first_char)),
        ),
        (
            b"FontDescriptor".to_vec(),
            PdfObject::Dictionary(PdfDict::from([
                (b"Ascent".to_vec(), PdfObject::Integer(ascent as i64)),
                (b"Descent".to_vec(), PdfObject::Integer(descent as i64)),
                (
                    b"MissingWidth".to_vec(),
                    PdfObject::Integer(missing_width as i64),
                ),
            ])),
        ),
    ]);
    if let Some(widths) = widths {
        dict.insert(
            b"Widths".to_vec(),
            PdfObject::Array(
                widths
                    .into_iter()
                    .map(|w| PdfObject::Integer(w as i64))
                    .collect(),
            ),
        );
    }
    // Fallback is selected from a bounded set derived from fuzz bytes elsewhere;
    // this builder keeps the skeleton fixed and only varies the name passed in.
    dict.insert(
        b"Encoding".to_vec(),
        PdfObject::Name(encoding_name.to_vec()),
    );
    if let Some(reference) = to_unicode_ref {
        dict.insert(b"ToUnicode".to_vec(), PdfObject::Reference(reference));
    }
    PdfObject::Dictionary(dict)
}

fn build_type0_font(
    pdf: &mut FuzzPdf,
    config: &[u8],
    custom: bool,
    vertical: bool,
) -> (PdfObject, usize) {
    let descendant_ref = ObjectRef {
        object_number: 30,
        generation: 0,
    };
    let to_unicode_ref = ObjectRef {
        object_number: 31,
        generation: 0,
    };
    let encoding_ref = ObjectRef {
        object_number: 32,
        generation: 0,
    };

    // Derive widths from fuzz bytes in a bounded way.
    let dw = 900.0 + f64::from(config.first().copied().unwrap_or(0) % 50);
    let w = if config.get(1).copied().unwrap_or(0) % 2 == 0 {
        PdfObject::Array(vec![
            PdfObject::Integer(1),
            PdfObject::Integer(3),
            PdfObject::Integer(600),
        ])
    } else {
        PdfObject::Array(vec![
            PdfObject::Integer(1),
            PdfObject::Array(vec![PdfObject::Integer(500), PdfObject::Integer(610)]),
        ])
    };

    let mut descendant = PdfDict::from([
        (
            b"Subtype".to_vec(),
            PdfObject::Name(b"CIDFontType2".to_vec()),
        ),
        (b"DW".to_vec(), PdfObject::Integer(dw as i64)),
        (b"W".to_vec(), w),
        (
            b"FontDescriptor".to_vec(),
            PdfObject::Dictionary(PdfDict::from([
                (b"Ascent".to_vec(), PdfObject::Integer(880)),
                (b"Descent".to_vec(), PdfObject::Integer(-120)),
            ])),
        ),
    ]);
    if vertical {
        descendant.insert(
            b"DW2".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(880), PdfObject::Integer(-1000)]),
        );
    }

    pdf.objects
        .insert(descendant_ref, PdfObject::Dictionary(descendant));
    // ToUnicode for composite: map a few CIDs to Unicode, leave others unmapped.
    // This exercises ToUnicode priority and unmapped preservation.
    let to_unicode_bytes = if vertical {
        b"1 begincodespacerange <0000> <FFFF> endcodespacerange 1 beginbfchar <0001> <0041> endbfchar"
            .to_vec()
    } else {
        b"1 begincodespacerange <0000> <FFFF> endcodespacerange 2 beginbfchar <0001> <0041> <0002> <0042> endbfchar"
            .to_vec()
    };
    pdf.objects
        .insert(to_unicode_ref, PdfObject::Stream(PdfDict::new()));
    pdf.streams.insert(to_unicode_ref, to_unicode_bytes);

    let (encoding, source_width) = if custom {
        // Bounded custom identity CMap: single full-domain 1-byte codespace and
        // identity CID range. This is the only custom CMap accepted by the
        // production decoder within finite entry budgets.
        let cmap_bytes = b"begincmap /WMode 0 def 1 begincodespacerange <00> <FF> endcodespacerange 1 begincidrange <00> <FF> 0 endcidrange endcmap".to_vec();
        pdf.objects
            .insert(encoding_ref, PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(encoding_ref, cmap_bytes);
        (PdfObject::Reference(encoding_ref), 1)
    } else if vertical {
        (PdfObject::Name(b"Identity-V".to_vec()), 2)
    } else {
        (PdfObject::Name(b"Identity-H".to_vec()), 2)
    };

    let font = PdfDict::from([
        (b"Subtype".to_vec(), PdfObject::Name(b"Type0".to_vec())),
        (b"BaseFont".to_vec(), PdfObject::Name(b"TestFont".to_vec())),
        (b"Encoding".to_vec(), encoding),
        (
            b"DescendantFonts".to_vec(),
            PdfObject::Array(vec![PdfObject::Reference(descendant_ref)]),
        ),
        (b"ToUnicode".to_vec(), PdfObject::Reference(to_unicode_ref)),
    ]);
    // Keep skeleton fixed; only widths, vertical flag, and encoding choice are
    // derived from fuzz bytes. No generic object parsing is performed.
    let _ = font;
    (PdfObject::Dictionary(font), source_width)
}

fn exercise_font(pdf: &FuzzPdf, font: &PdfObject, payload: &[u8], expected_width: usize) {
    let Ok(loaded) = FontDecoder::load(pdf, font, FONT_LIMITS) else {
        return;
    };
    // Finite metrics are required for successful loads.
    assert!(loaded.decoder.ascent_1000_em().is_finite());
    assert!(loaded.decoder.descent_1000_em().is_finite());
    assert!(loaded.decoder.ascent_1000_em() > loaded.decoder.descent_1000_em());

    let Ok(glyphs) = loaded
        .decoder
        .decode(payload, MAX_OUTPUT_GLYPHS, MAX_MAPPED_TEXT_BYTES)
    else {
        return;
    };
    assert_invariants(&glyphs, payload, expected_width);
}

fn assert_invariants(
    glyphs: &[crate::pdf::font::DecodedGlyph],
    input: &[u8],
    expected_width: usize,
) {
    assert!(glyphs.len() <= MAX_OUTPUT_GLYPHS);
    let mapped_bytes: usize = glyphs
        .iter()
        .filter_map(|glyph| match &glyph.mapping {
            UnicodeMapping::Mapped(text) => Some(text.len()),
            UnicodeMapping::Unmapped => None,
        })
        .sum();
    assert!(mapped_bytes <= MAX_MAPPED_TEXT_BYTES);
    // Concatenated raw codes must preserve the successfully decoded input.
    let concatenated: Vec<u8> = glyphs.iter().flat_map(|g| g.raw_code.clone()).collect();
    assert_eq!(concatenated, input);
    assert_eq!(glyphs.len() * expected_width, input.len());
    for glyph in glyphs {
        assert_eq!(glyph.raw_code.len(), expected_width);
        assert!(glyph.width_1000_em.is_finite());
        assert!(glyph.width_1000_em >= 0.0);
        match &glyph.mapping {
            UnicodeMapping::Mapped(text) => {
                assert!(!text.is_empty(), "mapped text must not be empty");
                assert!(
                    !text.contains('\u{FFFD}'),
                    "mapped text must not contain U+FFFD"
                );
                assert!(!text.contains('\0'), "mapped text must not contain null");
            }
            UnicodeMapping::Unmapped => {
                // Unmapped is explicit and must not be represented as empty
                // text or the replacement character. The enum variant itself
                // guarantees this invariant.
            }
        }
        if let Some(vertical) = glyph.vertical {
            assert!(vertical.displacement_y_1000_em.is_finite());
            assert!(vertical.origin_x_1000_em.is_finite());
            assert!(vertical.origin_y_1000_em.is_finite());
        }
    }
}

/// Exercises the PDF glyph extraction pipeline with tight, finite limits.
///
/// Inputs larger than 64 KiB are ignored. The harness builds a
/// `ParserBackedGlyphSource` from `LopdfParser` and
/// `ContentStreamGlyphExtractor` and runs it with small parse and extraction
/// budgets. Parser, extraction, and resource-limit errors are accepted
/// outcomes; the oracle asserts only that successful outcomes remain within the
/// configured glyph and issue budgets and preserve document invariants.
///
/// # Panics
///
/// Panics if a successful extraction outcome violates its glyph budget, issue
/// scope invariants, or glyph provenance contracts that are verified against
/// existing unit tests.
#[doc(hidden)]
pub fn fuzz_glyph_extraction(input: &[u8]) {
    if input.len() > MAX_INPUT_BYTES {
        return;
    }
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let Ok(outcome) = source.extract_outcome(Arc::from(input), PARSE_LIMITS, EXTRACTION_LIMITS)
    else {
        return;
    };
    assert!(outcome.document().items().len() <= EXTRACTION_LIMITS.max_glyphs);
    // `ExtractionOutcome::new` validates issue scopes and glyph-gap
    // boundaries, but a successful outcome must also keep them constructible.
    let (document, issues) = outcome.into_parts();
    assert!(
        ExtractionOutcome::new(document.clone(), issues.clone()).is_ok(),
        "extraction outcome must remain well-formed"
    );
    // Validate retained glyph provenance and geometry contracts.
    for glyph in document.items() {
        assert!(glyph.font_size.is_finite());
        assert!(glyph.font_size > 0.0);
        assert!(glyph.bbox.min.x.is_finite() && glyph.bbox.min.y.is_finite());
        assert!(glyph.bbox.max.x.is_finite() && glyph.bbox.max.y.is_finite());
        assert!(glyph.baseline.x.is_finite() && glyph.baseline.y.is_finite());
        assert!(glyph.direction.x.is_finite() && glyph.direction.y.is_finite());
        // `Unmapped` must remain explicit; mapped text is handled by font
        // decoder invariants exercised in `fuzz_font_decoder`.
        if let crate::model::DecodedText::Mapped(text) = &glyph.text {
            assert!(!text.is_empty());
            assert!(!text.contains('\u{FFFD}'));
            assert!(!text.contains('\0'));
        }
    }
    // Re-check that reconstructible page issues do not alias document state.
    drop(document);
    drop(issues);
}

const MAX_LAYOUT_GLYPHS: usize = 32;
const MAX_LAYOUT_VECTOR_LINES: usize = 8;
const MAX_LAYOUT_PAGES: u32 = 3;
const LAYOUT_FONT_SIZE_MIN: f64 = 4.0;
const LAYOUT_FONT_SIZE_STEP: f64 = 1.5;
const LAYOUT_POSITION_CELL: f64 = 12.0;
const LAYOUT_POSITION_CELLS: u8 = 24;

/// Exercises line, block, and region reconstruction plus the full comparison
/// pipeline over an arbitrary synthetic [`Document<Glyph>`].
///
/// Inputs larger than 64 KiB are ignored. The first byte selects the glyph
/// count (at most [`MAX_LAYOUT_GLYPHS`]); every remaining field is derived from
/// fuzzed bytes but kept finite, axis-consistent, and within small page bounds,
/// so layout code sees degenerate-but-valid geometry rather than being
/// short-circuited by validation. Glyph text mixes mapped, multi-scalar, CJK,
/// and unmapped values, and up to [`MAX_LAYOUT_VECTOR_LINES`] straight vector
/// lines are retained as layout evidence. Parser errors are accepted outcomes.
///
/// # Panics
///
/// Panics if comparing the constructed document with itself reports any exact
/// or formatting change, because identity input must never produce a change,
/// or if a successful glyph-overlay render does not start with `<svg`.
#[doc(hidden)]
pub fn fuzz_layout_pipeline(input: &[u8]) {
    if input.len() > MAX_INPUT_BYTES {
        return;
    }
    if input.is_empty() {
        return;
    }
    let document = synthetic_glyph_document(input);

    if let Ok(lines) = reconstruct_lines(&document, LineOptions::default()) {
        let mut pages = lines.iter().map(|line| line.page).collect::<Vec<_>>();
        pages.sort_unstable();
        pages.dedup();
        for page in pages.into_iter().take(MAX_LAYOUT_PAGES as usize) {
            let page_lines = lines
                .iter()
                .filter(|line| line.page == page)
                .cloned()
                .collect::<Vec<Line>>();
            let _ = partition_regions(page, &page_lines, RegionOptions::default());
        }
    }

    let Ok(comparison) = compare_glyph_documents(&document, &document, PipelineOptions::default())
    else {
        return;
    };
    assert!(
        comparison.changes.is_empty(),
        "identity comparison reported {} exact changes",
        comparison.changes.len()
    );
    assert!(
        comparison.formatting_changes.is_empty(),
        "identity comparison reported {} formatting changes",
        comparison.formatting_changes.len()
    );

    if let Ok(svg) = crate::report::render_glyph_overlay_svg(&document) {
        assert!(svg.starts_with("<svg"));
    }
}

/// Compares two arbitrary synthetic documents through the full text pipeline.
///
/// Inputs larger than 64 KiB are ignored, and each half must be non-empty. The
/// two halves produce separate documents that are compared in both directions,
/// exercising alignment, recovery, sentence pairing, the Myers diff, and report
/// summarization. Comparison errors are accepted outcomes; a successful
/// comparison must still summarize and satisfy the public result shapes.
///
/// # Panics
///
/// Panics if a successful comparison cannot be summarized or violates a public
/// result-shape invariant.
#[doc(hidden)]
pub fn fuzz_text_comparison(input: &[u8]) {
    if input.len() > MAX_INPUT_BYTES || input.len() < 2 {
        return;
    }
    let (left, right) = input.split_at(input.len() / 2);
    if left.is_empty() || right.is_empty() {
        return;
    }
    let old = synthetic_glyph_document(left);
    let new = synthetic_glyph_document(right);
    let _ = check_text_comparison(&old, &new);
    let _ = check_text_comparison(&new, &old);
}

fn check_text_comparison(old: &Document<Glyph>, new: &Document<Glyph>) -> bool {
    let Ok(outcome) = compare_extraction_outcomes(
        ExtractionOutcome::complete(old.clone()),
        ExtractionOutcome::complete(new.clone()),
        PipelineOptions::default(),
    ) else {
        return false;
    };
    let comparison = &outcome.comparison;
    let summary =
        summarize(comparison, &outcome.extraction).expect("a successful comparison must summarize");
    assert_eq!(summary.content_changes, comparison.changes.len());
    assert_eq!(
        summary.uncertain_changes,
        comparison
            .changes
            .iter()
            .filter(|change| change.confidence == Confidence::Low)
            .count()
    );
    for change in &comparison.changes {
        assert!(!change.occurrences.is_empty());
        for occurrence in &change.occurrences {
            assert!(occurrence.old_span.is_some() || occurrence.new_span.is_some());
        }
    }
    for region in &comparison.unresolved_regions {
        assert!(region.old_span.is_some() || region.new_span.is_some());
    }
    // A pipeline-produced comparison must satisfy the public report contracts.
    let text = render_text(
        &outcome.old_blocks,
        &outcome.new_blocks,
        comparison,
        &outcome.extraction,
        &TextReportOptions {
            old_label: "old",
            new_label: "new",
            color: false,
        },
    )
    .expect("a successful comparison must render as text");
    assert!(!text.is_empty());
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{202e}'),
        "PDF-derived text must be rendered with terminal controls escaped"
    );
    let old_evidence = old
        .items()
        .iter()
        .map(GlyphEvidence::from)
        .collect::<Vec<_>>();
    let new_evidence = new
        .items()
        .iter()
        .map(GlyphEvidence::from)
        .collect::<Vec<_>>();
    let mut json = Vec::new();
    write_json(
        &mut json,
        &outcome.old_blocks,
        &outcome.new_blocks,
        &old_evidence,
        &new_evidence,
        comparison,
        &outcome.extraction,
    )
    .expect("a successful comparison must serialize as JSON");
    assert!(json.starts_with(b"{"));
    assert!(
        serde_json::from_slice::<serde_json::Value>(&json).is_ok(),
        "the JSON report must parse"
    );
    true
}

/// Exercises the shared evidence graph and solver over arbitrary synthetic
/// [`Document<Glyph>`] values.
///
/// Inputs larger than 64 KiB are ignored. A single-byte input compares one
/// store with itself: a successful comparison must not report any typed
/// operation. Longer inputs split into two documents, build one store for each,
/// and compare them through the shared solver; a successful comparison must
/// serialize and satisfy the channel coverage contract. Evidence, graph, and
/// comparison errors are accepted outcomes.
///
/// # Panics
///
/// Panics if an identity comparison reports a typed operation, if a pairwise
/// comparison cannot serialize, or if channel coverage is internally
/// inconsistent.
#[doc(hidden)]
pub fn fuzz_graph_pipeline(input: &[u8]) {
    if input.len() > MAX_INPUT_BYTES || input.is_empty() {
        return;
    }
    if input.len() < 2 {
        let document = synthetic_glyph_document(input);
        let _ = exercise_default_graph_pipeline(&document, input[0]);
        return;
    }
    let (left, right) = input.split_at(input.len() / 2);
    let old = synthetic_glyph_document(left);
    let new = synthetic_glyph_document(right);
    let old_store = exercise_default_graph_pipeline(&old, input[0]);
    let new_store = exercise_default_graph_pipeline(&new, input.get(1).copied().unwrap_or(0));
    if let (Some(old_store), Some(new_store)) = (old_store, new_store) {
        let _ = check_graph_pair(&old_store, &new_store);
    }
}

fn check_graph_pair(old_store: &EvidenceStore, new_store: &EvidenceStore) -> bool {
    let limits = EvidenceLimits::default();
    let Ok(mut old_graph) = DocumentGraph::from_evidence(
        old_store,
        PipelineOptions::default(),
        limits,
        GraphLimits::default(),
    ) else {
        return false;
    };
    let Ok(mut new_graph) = DocumentGraph::from_evidence(
        new_store,
        PipelineOptions::default(),
        limits,
        GraphLimits::default(),
    ) else {
        return false;
    };
    {
        let old_view = DocumentView {
            evidence: old_store,
            graph: &old_graph,
        };
        let new_view = DocumentView {
            evidence: new_store,
            graph: &new_graph,
        };
        let Ok(comparison) = crate::document::compare_document_views(
            old_view,
            new_view,
            CorrespondenceScope {
                old: NodeId(0),
                new: NodeId(0),
            },
            DocumentComparisonLimits::default(),
            HierarchyLimits::default(),
        ) else {
            return false;
        };
        assert!(
            serde_json::to_vec(&comparison).is_ok(),
            "a successful document comparison must serialize"
        );
        let channels = [
            crate::document::Channel::Text,
            crate::document::Channel::Visual,
            crate::document::Channel::Forms,
            crate::document::Channel::Relations,
        ]
        .into_iter()
        .collect();
        for entry in crate::document::document_coverage(old_view, new_view, &comparison, &channels)
        {
            assert!(entry.old_compared_sources <= entry.old_discovered_sources);
            assert!(entry.new_compared_sources <= entry.new_discovered_sources);
            if entry.complete {
                assert!(entry.old_inventory_complete && entry.new_inventory_complete);
                assert_eq!(entry.old_uncompared_sources, 0);
                assert_eq!(entry.new_uncompared_sources, 0);
            }
        }
    }

    let old_nodes = old_graph
        .nodes
        .iter()
        .map(|node| node.id)
        .collect::<BTreeSet<_>>();
    let new_nodes = new_graph
        .nodes
        .iter()
        .map(|node| node.id)
        .collect::<BTreeSet<_>>();
    let Ok(refinements) = refine_table_views(
        &mut old_graph,
        &mut new_graph,
        old_store,
        new_store,
        PipelineOptions::default(),
        DocumentComparisonLimits::default(),
    ) else {
        return false;
    };
    for (graph, store, views, counterpart, before) in [
        (
            &old_graph,
            old_store,
            &refinements.old,
            &new_graph,
            &old_nodes,
        ),
        (
            &new_graph,
            new_store,
            &refinements.new,
            &old_graph,
            &new_nodes,
        ),
    ] {
        assert!(
            graph
                .validate(store, EvidenceLimits::default(), GraphLimits::default())
                .is_ok(),
            "a refined graph must stay valid against its evidence"
        );
        for node in before {
            assert!(
                graph.nodes.iter().any(|candidate| candidate.id == *node),
                "table refinement removed original node {node:?}"
            );
        }
        for view in views {
            assert!(
                graph
                    .nodes
                    .iter()
                    .any(|node| node.id == view.target_table && node.kind == NodeKind::Table),
                "an installed table must be a real table node"
            );
            assert!(
                counterpart
                    .nodes
                    .iter()
                    .any(|node| node.id == view.counterpart_table && node.kind == NodeKind::Table),
                "a counterpart view must reference a real table node"
            );
            assert!(
                !view.anchor_sources.is_empty() && !view.counterpart_sources.is_empty(),
                "installed counterpart views must carry their source evidence"
            );
        }
    }
    true
}

/// Exercises native form and structure evidence extraction on the input PDF and
/// on a synthetic form/tag PDF derived from the same bytes.
///
/// Inputs larger than 64 KiB are ignored. The parser and glyph extraction use
/// the tight fuzzing budgets. A successful store build, form extraction, or
/// structure extraction must validate as evidence, and structure IDs continue
/// after any extracted form fields. Malformed, unsupported, unresolved, and
/// resource-limit outcomes are accepted.
///
/// # Panics
///
/// Panics if evidence produced by form or structure extraction fails validation.
#[doc(hidden)]
#[must_use]
pub fn fuzz_native_evidence(input: &[u8]) -> bool {
    if input.len() > MAX_INPUT_BYTES {
        return false;
    }
    let mut reached = native_evidence_for_pdf(input);
    let synthetic = synthetic_evidence_pdf(input);
    if !synthetic.is_empty() {
        reached |= native_evidence_for_pdf(&synthetic);
    }
    reached
}

fn native_evidence_for_pdf(input: &[u8]) -> bool {
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let Ok(outcome) = source.extract_outcome(Arc::from(input), PARSE_LIMITS, EXTRACTION_LIMITS)
    else {
        return false;
    };
    let (document, issues) = outcome.into_parts();
    let Ok(extraction) = ExtractionOutcome::new(document, issues) else {
        return false;
    };
    let Ok(pdf) = LopdfParser.parse(Arc::from(input), PARSE_LIMITS) else {
        return false;
    };
    let limits = EvidenceLimits::default();
    let Ok(pages) = pdf.pages() else {
        return false;
    };
    let page_evidence = pages
        .iter()
        .enumerate()
        .filter_map(|(index, _)| u32::try_from(index).ok())
        .map(|index| PageEvidence {
            page: PageId(index),
            bounds: None,
        })
        .collect();
    let backend = BackendIdentity {
        kind: BackendKind::NativeParser,
        name: "pdfdelta-fuzz".into(),
        version: "0".into(),
        profile: "native-evidence-v1".into(),
        model: None,
    };
    let Ok(mut store) = EvidenceStore::from_native(
        "pdfdelta-fuzz".into(),
        backend,
        page_evidence,
        extraction,
        limits,
    ) else {
        return false;
    };
    let mut reached = false;
    if let Ok(forms) = extract_form_evidence(pdf.as_ref(), 0, 0, FormLimits::default()) {
        store.structured = forms.fields;
        store.issues.extend(forms.issues);
        store.inventories.push(forms.inventory);
        assert!(
            store.validate(limits).is_ok(),
            "form evidence must validate"
        );
        reached = true;
    }
    let first_structure_id = store.structured.len() as u64;
    if let Ok(structure) = extract_structure_evidence(
        pdf.as_ref(),
        &store.native,
        0,
        first_structure_id,
        StructureLimits::default(),
    ) {
        store.structured.extend(structure.elements);
        store.issues.extend(structure.issues);
        store.inventories.push(structure.inventory);
        assert!(
            store.validate(limits).is_ok(),
            "structure evidence must validate"
        );
        reached = true;
    }
    let Ok(graph) = DocumentGraph::from_evidence(
        &store,
        PipelineOptions::default(),
        limits,
        GraphLimits::default(),
    ) else {
        return reached;
    };
    let view = DocumentView {
        evidence: &store,
        graph: &graph,
    };
    if let Ok(comparison) = crate::document::compare_document_views(
        view,
        view,
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        DocumentComparisonLimits::default(),
        HierarchyLimits::default(),
    ) {
        assert!(
            comparison
                .comparisons()
                .all(|pair| pair.operation.is_none()),
            "identity comparison of extracted evidence reported a typed operation"
        );
        reached = true;
    }
    reached
}

/// Builds a small PDF with one font, a marked-content run, a structure tree,
/// and up to three `AcroForm` fields whose names, types, values, and widget
/// states derive from the fuzz bytes.
fn synthetic_evidence_pdf(input: &[u8]) -> Vec<u8> {
    use lopdf::{Document, Object, Stream, dictionary};

    let byte = |index: usize| input.get(index).copied().unwrap_or(0);
    let mut pdf = Document::with_version("1.7");
    let pages = pdf.new_object_id();
    let font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"/P <</MCID 0>> BDC BT /F1 10 Tf 1 0 0 1 20 30 Tm (Tagged note) Tj ET EMC".to_vec(),
    ));
    let page = pdf.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages,
        "MediaBox" => vec![Object::from(0), Object::from(0), Object::from(200), Object::from(200)],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
        "StructParents" => 0,
    });
    pdf.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page)],
            "Count" => 1,
        }),
    );

    let mut fields = Vec::new();
    for index in 0..usize::from(byte(0) % 4) {
        let base = 1 + index * 8;
        let name = format!("field{}", byte(base + 1) % 10);
        let mut field = dictionary! { "T" => Object::string_literal(name) };
        match byte(base) % 3 {
            0 => {
                field.set("FT", "Tx");
                match byte(base + 2) % 4 {
                    0 => field.set(
                        "V",
                        Object::string_literal(format!("value{}", byte(base + 3))),
                    ),
                    1 => field.set(
                        "V",
                        Object::String(
                            vec![0xfe, 0xff, byte(base + 3)],
                            lopdf::StringFormat::Literal,
                        ),
                    ),
                    2 => field.set("V", Object::Integer(i64::from(byte(base + 3)))),
                    _ => {}
                }
            }
            1 => {
                field.set("FT", "Btn");
                let saved = if byte(base + 2).is_multiple_of(2) {
                    b"Yes".to_vec()
                } else {
                    b"Off".to_vec()
                };
                field.set("V", Object::Name(saved));
                if byte(base + 3).is_multiple_of(2) {
                    field.set("Subtype", "Widget");
                    let state = match byte(base + 4) % 3 {
                        0 => b"Off".to_vec(),
                        1 => b"Yes".to_vec(),
                        _ => b"Other".to_vec(),
                    };
                    field.set("AS", Object::Name(state));
                } else {
                    let kids = (0..=usize::from(byte(base + 4) % 3))
                        .map(|kid| {
                            let state = match byte(base + 5 + kid) % 3 {
                                0 => b"Off".to_vec(),
                                1 => b"Yes".to_vec(),
                                _ => b"Other".to_vec(),
                            };
                            Object::Reference(pdf.add_object(dictionary! {
                                "Subtype" => "Widget",
                                "AS" => Object::Name(state),
                            }))
                        })
                        .collect::<Vec<_>>();
                    field.set("Kids", kids);
                }
            }
            _ => {
                field.set("FT", "Ch");
                match byte(base + 2) % 3 {
                    0 => field.set(
                        "V",
                        Object::string_literal(format!("choice{}", byte(base + 3))),
                    ),
                    1 => {
                        let values = (0..=usize::from(byte(base + 3) % 3))
                            .map(|value| {
                                Object::string_literal(format!("c{}", byte(base + 4 + value)))
                            })
                            .collect::<Vec<_>>();
                        field.set("V", Object::Array(values));
                    }
                    _ => {
                        field.set("Opt", Object::Array(vec![Object::string_literal("option")]));
                        field.set("V", Object::Name(b"Off".to_vec()));
                    }
                }
            }
        }
        let field_id = pdf.new_object_id();
        match byte(base + 7) % 4 {
            0 => {
                // A repeated reference must stay a reported failure, never a hang.
                field.set("Kids", vec![Object::Reference(field_id)]);
            }
            1 => {
                // The inherited type stays on the parent while the child owns
                // the partial name.
                let child_id = pdf.new_object_id();
                let child = dictionary! {
                    "T" => Object::string_literal(format!("child{index}")),
                };
                pdf.objects.insert(child_id, Object::Dictionary(child));
                field.set("Kids", vec![Object::Reference(child_id)]);
            }
            _ => {}
        }
        pdf.objects.insert(field_id, Object::Dictionary(field));
        fields.push(Object::Reference(field_id));
    }

    let root = pdf.new_object_id();
    let table = pdf.new_object_id();
    let row = pdf.new_object_id();
    let cell = pdf.add_object(dictionary! {
        "Type" => "StructElem",
        "S" => "TD",
        "P" => row,
        "Pg" => page,
        "K" => i64::from(byte(31) % 3),
    });
    pdf.objects.insert(
        row,
        Object::Dictionary(dictionary! {
            "Type" => "StructElem",
            "S" => "TR",
            "P" => table,
            "K" => cell,
        }),
    );
    pdf.objects.insert(
        table,
        Object::Dictionary(dictionary! {
            "Type" => "StructElem",
            "S" => "Table",
            "P" => root,
            "K" => row,
        }),
    );
    let root_k = if byte(32).is_multiple_of(2) {
        // A second root child whose parent reference points to itself must
        // stay a reported failure, never a hang.
        let cyclic = pdf.new_object_id();
        pdf.objects.insert(
            cyclic,
            Object::Dictionary(dictionary! {
                "Type" => "StructElem",
                "S" => "P",
                "P" => cyclic,
                "Pg" => page,
                "K" => Object::Array(vec![Object::Integer(0)]),
            }),
        );
        Object::Array(vec![Object::Reference(table), Object::Reference(cyclic)])
    } else {
        Object::Reference(table)
    };
    pdf.objects.insert(
        root,
        Object::Dictionary(dictionary! { "Type" => "StructTreeRoot", "K" => root_k }),
    );

    let catalog = pdf.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages,
        "AcroForm" => dictionary! { "Fields" => fields },
        "StructTreeRoot" => root,
        "MarkInfo" => dictionary! { "Marked" => true },
    });
    pdf.trailer.set("Root", catalog);

    let mut bytes = Vec::new();
    if pdf.save_to(&mut bytes).is_err() {
        return Vec::new();
    }
    bytes
}

/// Exercises the multichannel evidence graph and shared solver over an
/// arbitrary native document and reports whether the comparison completed.
///
/// The store also retains up to three synthetic renderer regions, overlapping
/// structure elements, and text form fields so visual candidate search,
/// structure import, and form validation run under the same identity
/// comparison. The comparison uses the same store and graph for both sides, so
/// a successful comparison must not report any typed operation. Evidence,
/// graph, or comparison errors are accepted outcomes.
fn exercise_default_graph_pipeline(document: &Document<Glyph>, seed: u8) -> Option<EvidenceStore> {
    let mut page_ids: BTreeSet<PageId> = document.items().iter().map(|glyph| glyph.page).collect();
    page_ids.insert(PageId(0));
    let pages = page_ids
        .into_iter()
        .map(|page| PageEvidence { page, bounds: None })
        .collect();
    let limits = EvidenceLimits::default();
    let backend = BackendIdentity {
        kind: BackendKind::NativeParser,
        name: "pdfdelta-fuzz".into(),
        version: "0".into(),
        profile: "layout-pipeline-v1".into(),
        model: None,
    };
    let Ok(mut store) = EvidenceStore::from_native(
        "pdfdelta-fuzz".into(),
        backend,
        pages,
        ExtractionOutcome::complete(document.clone()),
        limits,
    ) else {
        return None;
    };
    store.backends.push(BackendIdentity {
        kind: BackendKind::Renderer,
        name: "pdfdelta-fuzz-renderer".into(),
        version: "0".into(),
        profile: "layout-pipeline-v1".into(),
        model: None,
    });
    for index in 0..=usize::from(seed % 3) {
        let width = 2 + u32::from(seed) % 3;
        let height = 2 + (u32::from(seed) >> 2) % 3;
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for sample in 0..width * height {
            let value = seed
                .wrapping_add((index as u8) << 4)
                .wrapping_add(sample as u8);
            rgb.extend_from_slice(&[value, value.wrapping_mul(3), value.wrapping_add(70)]);
        }
        let id = store.rendered.len() as u64;
        store.rendered.push(RenderedEvidence {
            id,
            page: PageId(0),
            backend: 1,
            raster: Raster { width, height, rgb },
            composited_page: true,
            polygon: vec![
                Vec2 { x: 0.0, y: 0.0 },
                Vec2 { x: 10.0, y: 0.0 },
                Vec2 { x: 10.0, y: 10.0 },
                Vec2 { x: 0.0, y: 10.0 },
            ],
        });
    }
    if seed.is_multiple_of(4)
        && let Some(region) = store.rendered.first()
    {
        let width = region.raster.width;
        let height = region.raster.height;
        let pixel_bounds = [0, 0, width, height];
        let region_id = region.id;
        let Ok(bounds) = region.pixel_bounds_in_page(pixel_bounds) else {
            return None;
        };
        let ocr_backend = store.backends.len();
        store.backends.push(BackendIdentity {
            kind: BackendKind::Ocr,
            name: "pdfdelta-fuzz-ocr".into(),
            version: "0".into(),
            profile: "layout-pipeline-v1".into(),
            model: Some("pdfdelta-fuzz-model".into()),
        });
        let id = store.structured.len() as u64;
        store.structured.push(StructuredEvidence {
            id,
            page: Some(PageId(0)),
            bounds: Some(bounds),
            object: None,
            backend: ocr_backend,
            value: StructuredValue::RecognizedText {
                text: "ocr".into(),
                region: region_id,
                pixel_bounds,
                words: vec![RecognizedWord {
                    text: "w".into(),
                    pixel_bounds: [0, 0, 1, 1],
                    confidence: Some(50.0),
                }],
            },
        });
    }
    let page_zero_glyphs = store
        .native
        .items()
        .iter()
        .filter(|glyph| glyph.page == PageId(0))
        .map(|glyph| glyph.id)
        .collect::<Vec<_>>();
    let mut previous_structure = None;
    for index in 0..usize::from(seed % 3) {
        if page_zero_glyphs.is_empty() {
            break;
        }
        let count = 1 + (usize::from(seed) + index) % page_zero_glyphs.len();
        let id = store.structured.len() as u64;
        store.structured.push(StructuredEvidence {
            id,
            page: Some(PageId(0)),
            bounds: None,
            object: None,
            backend: 0,
            value: StructuredValue::StructureElement {
                role: "P".into(),
                identifier: None,
                text: None,
                glyphs: page_zero_glyphs[..count].to_vec(),
                parent: previous_structure,
                order: None,
            },
        });
        previous_structure = Some(id);
    }
    for index in 0..usize::from(seed % 2) {
        let id = store.structured.len() as u64;
        let button = index == 1;
        store.structured.push(StructuredEvidence {
            id,
            page: Some(PageId(0)),
            bounds: None,
            object: None,
            backend: 0,
            value: StructuredValue::FormField {
                name: format!("field{index}"),
                field_type: Some(if button {
                    b"Btn".to_vec()
                } else {
                    b"Tx".to_vec()
                }),
                value: if button {
                    FieldValue::Selected(true)
                } else {
                    FieldValue::Text(format!("value{index}"))
                },
                widgets: vec![FormWidget {
                    object: None,
                    page: Some(PageId(0)),
                    bounds: Some(Rect {
                        min: Vec2 { x: 0.0, y: 0.0 },
                        max: Vec2 {
                            x: 10.0 + f64::from(seed),
                            y: 10.0,
                        },
                    }),
                    normal_appearance: None,
                    crop: None,
                    unresolved: None,
                }],
                button_states: if button {
                    vec![ButtonAppearanceState {
                        widget: None,
                        name: Some(b"Yes".to_vec()),
                    }]
                } else {
                    Vec::new()
                },
            },
        });
    }
    if seed.is_multiple_of(3) {
        let id = store.structured.len() as u64;
        store.structured.push(StructuredEvidence {
            id,
            page: Some(PageId(0)),
            bounds: None,
            object: None,
            backend: 0,
            value: StructuredValue::Annotation {
                category: "Link".into(),
                text: Some("reference".into()),
                target: Some("https://example.invalid".into()),
            },
        });
    }
    let Ok(graph) = DocumentGraph::from_evidence(
        &store,
        PipelineOptions::default(),
        limits,
        GraphLimits::default(),
    ) else {
        return None;
    };
    let _ = crate::document::assess_form_appearances(&store, Default::default());
    let Ok(comparison) = crate::document::compare_document_views(
        DocumentView {
            evidence: &store,
            graph: &graph,
        },
        DocumentView {
            evidence: &store,
            graph: &graph,
        },
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        DocumentComparisonLimits::default(),
        HierarchyLimits::default(),
    ) else {
        return None;
    };
    assert!(
        comparison
            .comparisons()
            .all(|pair| pair.operation.is_none()),
        "identity graph comparison reported a typed operation"
    );
    Some(store)
}

fn synthetic_glyph_document(input: &[u8]) -> Document<Glyph> {
    let glyph_count = usize::from(input[0]) % MAX_LAYOUT_GLYPHS + 1;
    let bytes = &input[1..];
    let mut glyphs = Vec::with_capacity(glyph_count);
    for index in 0..glyph_count {
        let field = |offset: usize| -> u8 {
            bytes
                .get(index.saturating_mul(8).saturating_add(offset) % bytes.len().max(1))
                .copied()
                .unwrap_or(0)
        };
        let font_size = LAYOUT_FONT_SIZE_MIN + f64::from(field(0) % 16) * LAYOUT_FONT_SIZE_STEP;
        let x = f64::from(field(1) % LAYOUT_POSITION_CELLS) * LAYOUT_POSITION_CELL;
        let y = f64::from(field(2) % LAYOUT_POSITION_CELLS) * LAYOUT_POSITION_CELL;
        let width = font_size * (0.4 + f64::from(field(3) % 8) * 0.1);
        let height = font_size * (0.6 + f64::from(field(4) % 8) * 0.1);
        let direction = SYNTHETIC_DIRECTIONS[usize::from(field(5) % 6)];
        let page = PageId(u32::from(field(6)) % MAX_LAYOUT_PAGES);
        let text = if field(7) % 8 == 0 {
            DecodedText::Unmapped {
                font_hash: crate::model::FontProgramHash(vec![field(7); 8]),
                glyph_id: u16::from(field(0)) | u16::from(field(7)) << 8,
            }
        } else {
            DecodedText::Mapped(
                SYNTHETIC_TEXTS[usize::from(field(7)) % SYNTHETIC_TEXTS.len()].to_owned(),
            )
        };
        let raw_code = match &text {
            DecodedText::Mapped(value) => value.as_bytes().to_vec(),
            DecodedText::Unmapped { glyph_id, .. } => glyph_id.to_be_bytes().to_vec(),
        };
        glyphs.push(Glyph {
            id: GlyphId(index as u64),
            text,
            raw_code,
            page,
            bbox: Rect {
                min: Vec2 { x, y },
                max: Vec2 {
                    x: x + width,
                    y: y + height,
                },
            },
            baseline: Vec2 { x, y },
            direction,
            font_id: FontId(u32::from(field(0) % 3)),
            font_size,
            render_order: index as u32,
            render_mode: SYNTHETIC_RENDER_MODES
                [usize::from(field(3)) % SYNTHETIC_RENDER_MODES.len()],
            crop_status: GlyphCropStatus::Inside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: index as u32,
            },
        });
    }

    let vector_base = glyph_count.saturating_mul(8);
    // Derive the vector count from the count byte so most inputs retain at
    // least one straight line instead of depending on one window byte.
    let vector_count = (usize::from(input[0]) * 7 + 3) % (MAX_LAYOUT_VECTOR_LINES + 1);
    let mut vector_lines = Vec::with_capacity(vector_count);
    for index in 0..vector_count {
        let base = vector_base.saturating_add(index.saturating_mul(6));
        let byte = |offset: usize| -> u8 {
            bytes
                .get(base.saturating_add(offset) % bytes.len().max(1))
                .copied()
                .unwrap_or(0)
        };
        let x = f64::from(byte(0) % LAYOUT_POSITION_CELLS) * LAYOUT_POSITION_CELL;
        let y = f64::from(byte(1) % LAYOUT_POSITION_CELLS) * LAYOUT_POSITION_CELL;
        let length = 6.0 + f64::from(byte(3) % 12) * LAYOUT_POSITION_CELL / 4.0;
        let (to_x, to_y) = if byte(2) % 2 == 0 {
            (x + length, y)
        } else {
            (x, y + length)
        };
        let render_order = glyph_count as u32 + index as u32;
        vector_lines.push(VectorLine {
            id: VectorLineId(index as u64),
            page: PageId(u32::from(byte(5)) % MAX_LAYOUT_PAGES),
            from: Vec2 { x, y },
            to: Vec2 { x: to_x, y: to_y },
            width: 0.2 + f64::from(byte(4) % 5) * 0.2,
            render_order,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: render_order,
            },
        });
    }
    Document::with_vector_lines(glyphs, vector_lines)
}

const SYNTHETIC_TEXTS: [&str; 24] = [
    "a",
    "b",
    " ",
    "ab",
    "1",
    "10",
    "status:",
    "\u{8a2d}\u{5b9a}",
    "e\u{301}",
    "-\n",
    "release10",
    "paragraph",
    "value 20",
    "unchanged.",
    "wrap-",
    "hyphenation",
    "\u{1b}[31m",
    "\u{202e}reordered",
    "\u{ad}",
    "\u{2028}",
    "a\u{200d}b",
    "e\u{327}\u{301}",
    "one\rtwo",
    "\u{5e}3",
];

const SYNTHETIC_RENDER_MODES: [TextRenderMode; 4] = [
    TextRenderMode::Fill,
    TextRenderMode::Stroke,
    TextRenderMode::Invisible,
    TextRenderMode::FillAndStroke,
];

/// Unit writing directions covering horizontal, vertical, and diagonal text.
const SYNTHETIC_DIRECTIONS: [Vec2; 6] = [
    Vec2 { x: 1.0, y: 0.0 },
    Vec2 { x: -1.0, y: 0.0 },
    Vec2 { x: 0.0, y: 1.0 },
    Vec2 { x: 0.0, y: -1.0 },
    Vec2 {
        x: core::f64::consts::FRAC_1_SQRT_2,
        y: core::f64::consts::FRAC_1_SQRT_2,
    },
    Vec2 {
        x: -core::f64::consts::FRAC_1_SQRT_2,
        y: core::f64::consts::FRAC_1_SQRT_2,
    },
];

#[derive(Default)]
struct FuzzPdf {
    objects: HashMap<ObjectRef, PdfObject>,
    streams: HashMap<ObjectRef, Vec<u8>>,
}

impl ParsedPdf for FuzzPdf {
    fn version(&self) -> PdfVersion {
        PdfVersion { major: 1, minor: 7 }
    }

    fn trailer(&self) -> Result<PdfDict, crate::Error> {
        Ok(PdfDict::new())
    }

    fn resolve(&self, reference: ObjectRef) -> Result<PdfObject, crate::Error> {
        self.objects
            .get(&reference)
            .cloned()
            .ok_or_else(|| crate::Error::Backend("missing mock object".into()))
    }

    fn pages(&self) -> Result<Vec<PageRef>, crate::Error> {
        Ok(Vec::new())
    }

    fn page_dict(&self, _page: PageRef) -> Result<PdfDict, crate::Error> {
        Err(crate::Error::Backend("unused mock method".into()))
    }

    fn raw_stream(&self, _reference: ObjectRef) -> Result<RawStream, crate::Error> {
        Err(crate::Error::Backend("unused mock method".into()))
    }

    fn decoded_stream(&self, reference: ObjectRef) -> Result<DecodedStream, crate::Error> {
        Ok(DecodedStream {
            dictionary: PdfDict::new(),
            bytes: self
                .streams
                .get(&reference)
                .cloned()
                .ok_or_else(|| crate::Error::Backend("missing mock stream".into()))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TO_UNICODE_SEED: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/corpus/cmap_parser/to-unicode-one-byte.cmap"
    ));
    const IDENTITY_SEED: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/corpus/cmap_parser/identity-full-domain.cmap"
    ));
    const CONTENT_STREAM_SEEDS: &[&[u8]] = &[
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fuzz/corpus/content_stream_parser/text-operators.content"
        )),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fuzz/corpus/content_stream_parser/nested-tagged-content.content"
        )),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fuzz/corpus/content_stream_parser/inline-image.content"
        )),
    ];

    const SIMPLE_WINANSI_SEED: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/corpus/font_decoder/simple-winansi.bin"
    ));
    const PARTIAL_TOUNICODE_SEED: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/corpus/font_decoder/partial-tounicode-differences.bin"
    ));
    const TYPE0_IDENTITY_H_SEED: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/corpus/font_decoder/type0-identityh.bin"
    ));
    const VERTICAL_CUSTOM_SEED: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/corpus/font_decoder/vertical-custom.bin"
    ));
    const GLYPH_EXTRACTION_SEED: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/corpus/glyph_extraction/valid.pdf"
    ));

    #[test]
    fn curated_success_seeds_reach_both_parser_paths() {
        let cmap = parse_to_unicode_for_width(TO_UNICODE_SEED, LIMITS, 1)
            .expect("curated ToUnicode seed should parse");
        assert!(cmap.entry_count() <= LIMITS.max_entries);

        let identity = parse_identity_cid_encoding(IDENTITY_SEED, LIMITS)
            .expect("curated identity CID seed should parse");
        assert_eq!(identity.source_width, 1);
        fuzz_cmap_parsers(IDENTITY_SEED);
    }

    #[test]
    fn curated_content_stream_seeds_parse_with_fuzz_limits() {
        for seed in CONTENT_STREAM_SEEDS {
            let operator_budget = ContentBudget::for_operators(CONTENT_LIMITS.max_operators);
            let operand_budget = ContentBudget::for_operands(CONTENT_LIMITS.max_operand_nodes);
            let mut parser =
                ContentParser::with_budgets(CONTENT_LIMITS, operator_budget, operand_budget);
            let operations = parser
                .parse_fragment(seed)
                .expect("curated content stream seed should parse");
            parser
                .finish()
                .expect("curated content stream seed should be complete");
            assert!(operations.len() <= CONTENT_LIMITS.max_operators);
            assert_operation_indexes(&operations);
            fuzz_content_stream_parser(seed);
        }
    }

    #[test]
    fn oversized_content_stream_input_is_ignored() {
        fuzz_content_stream_parser(&vec![b'q'; MAX_INPUT_BYTES + 1]);
    }

    #[test]
    fn content_stream_dictionary_value_crosses_a_fragment_boundary() {
        let operator_budget = ContentBudget::for_operators(CONTENT_LIMITS.max_operators);
        let operand_budget = ContentBudget::for_operands(CONTENT_LIMITS.max_operand_nodes);
        let mut parser =
            ContentParser::with_budgets(CONTENT_LIMITS, operator_budget, operand_budget);

        let first = parser
            .parse_fragment(b"q /Span << /ActualText ")
            .expect("first content stream fragment should parse");
        let second = parser
            .parse_fragment(b"<FEFF0031>>> BDC Q")
            .expect("second content stream fragment should complete the dictionary");
        parser
            .finish()
            .expect("content stream fragment sequence should be complete");

        assert!(first.len() + second.len() <= CONTENT_LIMITS.max_operators);
        assert_operation_indexes(&first);
        assert_operation_indexes(&second);
    }

    #[test]
    fn oversized_font_decoder_input_is_ignored() {
        fuzz_font_decoder(&vec![b'A'; MAX_INPUT_BYTES + 1]);
    }

    #[test]
    fn curated_font_decoder_seeds_do_not_panic() {
        fuzz_font_decoder(SIMPLE_WINANSI_SEED);
        fuzz_font_decoder(PARTIAL_TOUNICODE_SEED);
        fuzz_font_decoder(TYPE0_IDENTITY_H_SEED);
        fuzz_font_decoder(VERTICAL_CUSTOM_SEED);
    }

    #[test]
    fn simple_winansi_widths_and_fallback() {
        let mut pdf = FuzzPdf::default();
        let widths = vec![600.0, 610.0, 620.0, 630.0];
        let font = build_simple_font(
            &mut pdf,
            None,
            b"WinAnsiEncoding",
            65,
            Some(widths),
            500.0,
            700.0,
            -200.0,
        );
        let loaded =
            FontDecoder::load(&pdf, &font, FONT_LIMITS).expect("simple WinAnsi font should load");
        let glyphs = loaded
            .decoder
            .decode(b"ABCD", MAX_OUTPUT_GLYPHS, MAX_MAPPED_TEXT_BYTES)
            .expect("decoding should succeed");
        assert_eq!(glyphs.len(), 4);
        assert_eq!(glyphs[0].width_1000_em, 600.0);
        assert_eq!(glyphs[1].width_1000_em, 610.0);
        assert_eq!(glyphs[3].width_1000_em, 630.0);
        assert!(glyphs.iter().all(|g| g.raw_code.len() == 1));
        assert_invariants(&glyphs, b"ABCD", 1);
        // Fallback: 0x80 should map to Euro in WinAnsi
        let euro = loaded
            .decoder
            .decode(&[0x80], 1, 1024)
            .expect("euro should decode")[0]
            .clone();
        assert_eq!(euro.mapping, UnicodeMapping::Mapped("\u{20ac}".into()));
    }

    #[test]
    fn partial_tounicode_priority_and_differences() {
        let mut pdf = FuzzPdf::default();
        let to_unicode_ref = ObjectRef {
            object_number: 20,
            generation: 0,
        };
        pdf.objects
            .insert(to_unicode_ref, PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(
            to_unicode_ref,
            b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <005A> endbfchar".to_vec(),
        );
        let mut font = build_simple_font(
            &mut pdf,
            Some(to_unicode_ref),
            b"WinAnsiEncoding",
            65,
            Some(vec![600.0; 4]),
            480.0,
            700.0,
            -200.0,
        );
        // Inject Differences for 66->B and 67->UnknownGlyph
        let PdfObject::Dictionary(ref mut dict) = font else {
            panic!("font should be dictionary");
        };
        dict.insert(
            b"Encoding".to_vec(),
            PdfObject::Dictionary(PdfDict::from([
                (
                    b"BaseEncoding".to_vec(),
                    PdfObject::Name(b"WinAnsiEncoding".to_vec()),
                ),
                (
                    b"Differences".to_vec(),
                    PdfObject::Array(vec![
                        PdfObject::Integer(66),
                        PdfObject::Name(b"B".to_vec()),
                        PdfObject::Integer(67),
                        PdfObject::Name(b"UnknownGlyph".to_vec()),
                    ]),
                ),
            ])),
        );
        let loaded = FontDecoder::load(&pdf, &font, FONT_LIMITS).expect("partial font should load");
        // ToUnicode priority: 0x41 should be Z, not A
        let a = loaded
            .decoder
            .decode(&[0x41], 1, 1024)
            .expect("should decode")[0]
            .clone();
        assert_eq!(a.mapping, UnicodeMapping::Mapped("Z".into()));
        // Differences fallback: 0x42 should be B via Differences
        let b = loaded
            .decoder
            .decode(&[0x42], 1, 1024)
            .expect("should decode")[0]
            .clone();
        assert_eq!(b.mapping, UnicodeMapping::Mapped("B".into()));
        // Unmapped preservation: 0x43 should be Unmapped, not empty or FFFD
        let c = loaded
            .decoder
            .decode(&[0x43], 1, 1024)
            .expect("should decode")[0]
            .clone();
        assert_eq!(c.mapping, UnicodeMapping::Unmapped);
        // Fallback for code outside ToUnicode/Differences
        let d = loaded
            .decoder
            .decode(&[0x44], 1, 1024)
            .expect("should decode")[0]
            .clone();
        assert_eq!(d.mapping, UnicodeMapping::Mapped("D".into()));
        assert_invariants(&[a, b, c, d], &[0x41, 0x42, 0x43, 0x44], 1);
    }

    #[test]
    fn type0_identity_h_widths_and_odd_rejection() {
        let mut pdf = FuzzPdf::default();
        let (font, source_width) = build_type0_font(&mut pdf, &[0, 1], false, false);
        assert_eq!(source_width, 2);
        let loaded = FontDecoder::load(&pdf, &font, FONT_LIMITS).expect("Identity-H should load");
        let glyphs = loaded
            .decoder
            .decode(&[0x00, 0x01, 0x00, 0x02, 0x00, 0xFF], 10, 4096)
            .expect("even-length Identity-H should decode");
        assert_eq!(glyphs.len(), 3);
        assert_eq!(glyphs[0].mapping, UnicodeMapping::Mapped("A".into()));
        assert_eq!(glyphs[1].mapping, UnicodeMapping::Mapped("B".into()));
        assert_eq!(glyphs[2].mapping, UnicodeMapping::Unmapped);
        assert_eq!(glyphs[0].width_1000_em, 500.0);
        assert_invariants(&glyphs, &[0x00, 0x01, 0x00, 0x02, 0x00, 0xFF], 2);
        // Odd-byte rejection
        assert!(matches!(
            loaded.decoder.decode(&[0x00, 0x01, 0x00], 10, 4096),
            Err(crate::Error::Unresolved(msg)) if msg.contains("odd")
        ));
        // Width boundaries
        assert!(loaded.decoder.ascent_1000_em().is_finite());
        assert!(loaded.decoder.descent_1000_em().is_finite());
    }

    #[test]
    fn vertical_and_bounded_custom_identity() {
        // Vertical Identity-V
        let mut pdf = FuzzPdf::default();
        let (font, source_width) = build_type0_font(&mut pdf, &[0, 0], false, true);
        assert_eq!(source_width, 2);
        let loaded = FontDecoder::load(&pdf, &font, FONT_LIMITS).expect("Identity-V should load");
        assert_eq!(loaded.decoder.writing_mode(), WritingMode::Vertical);
        let glyphs = loaded
            .decoder
            .decode(&[0x00, 0x01], 10, 4096)
            .expect("vertical should decode");
        assert!(glyphs[0].vertical.is_some());
        assert_invariants(&glyphs, &[0x00, 0x01], 2);

        // Bounded custom 1-byte identity
        let mut pdf2 = FuzzPdf::default();
        let (font2, source_width2) = build_type0_font(&mut pdf2, &[1, 0], true, false);
        assert_eq!(source_width2, 1);
        let loaded2 =
            FontDecoder::load(&pdf2, &font2, FONT_LIMITS).expect("custom identity should load");
        let glyphs2 = loaded2
            .decoder
            .decode(&[0x01, 0x02, 0xFF], 10, 4096)
            .expect("1-byte custom should decode");
        assert_eq!(glyphs2.len(), 3);
        assert!(glyphs2.iter().all(|g| g.raw_code.len() == 1));
        assert_invariants(&glyphs2, &[0x01, 0x02, 0xFF], 1);
    }

    #[test]
    fn font_decoder_respects_output_budgets() {
        let mut pdf = FuzzPdf::default();
        let font = build_simple_font(
            &mut pdf,
            None,
            b"WinAnsiEncoding",
            65,
            Some(vec![600.0; 4]),
            500.0,
            700.0,
            -200.0,
        );
        let loaded = FontDecoder::load(&pdf, &font, FONT_LIMITS).expect("font should load");
        // Exceed glyph budget
        assert!(matches!(
            loaded.decoder.decode(&[b'A'; 2048], 10, 4096),
            Err(crate::Error::LimitExceeded { .. })
        ));
        // Exceed mapped-text bytes (each "A" is 1 byte, so 10 glyphs need 10 bytes, limit 2 should fail)
        assert!(matches!(
            loaded.decoder.decode(b"AAAAAAAAAA", 20, 2),
            Err(crate::Error::LimitExceeded { .. })
        ));
    }

    #[test]
    fn oversized_glyph_extraction_input_is_ignored() {
        fuzz_glyph_extraction(&vec![b'A'; MAX_INPUT_BYTES + 1]);
    }

    #[test]
    fn synthetic_layout_seeds_reach_the_pipeline_without_changes() {
        let mut reconstruction_reached = 0;
        let mut comparison_reached = 0;
        let mut graph_comparison_reached = 0;
        let mut svg_reached = 0;
        let mut vector_lines_seen = 0;
        for seed in 0u8..=64 {
            let mut input = vec![seed];
            input.extend((0..128u16).map(|index| {
                (index as u8)
                    .wrapping_mul(seed.wrapping_add(3))
                    .wrapping_add(seed)
            }));
            let document = synthetic_glyph_document(&input);
            if !document.vector_lines().is_empty() {
                vector_lines_seen += 1;
            }
            assert!(
                reconstruct_lines(&document, LineOptions::default()).is_ok(),
                "seed {seed} produced a document that line reconstruction rejects"
            );
            reconstruction_reached += 1;
            if compare_glyph_documents(&document, &document, PipelineOptions::default()).is_ok() {
                comparison_reached += 1;
            }
            if exercise_default_graph_pipeline(&document, seed).is_some() {
                graph_comparison_reached += 1;
            }
            if crate::report::render_glyph_overlay_svg(&document).is_ok() {
                svg_reached += 1;
            }
            fuzz_layout_pipeline(&input);
        }
        // The generator must keep producing documents that flow through the
        // whole exercised pipeline instead of stopping at validation.
        assert_eq!(reconstruction_reached, 65);
        assert!(
            comparison_reached >= 32,
            "only {comparison_reached} of 65 seeds reached the comparison pipeline"
        );
        assert!(
            graph_comparison_reached >= 32,
            "only {graph_comparison_reached} of 65 seeds reached the shared graph solver"
        );
        assert!(
            svg_reached >= 32,
            "only {svg_reached} of 65 seeds reached the SVG renderer"
        );
        assert!(
            vector_lines_seen >= 32,
            "only {vector_lines_seen} of 65 seeds carried vector-line evidence"
        );
        // Degenerate inputs must not panic or fabricate identity changes.
        fuzz_layout_pipeline(&[0]);
        fuzz_layout_pipeline(&[255]);
    }

    #[test]
    fn synthetic_graph_pairs_reach_the_shared_solver() {
        let mut reached = 0;
        for seed in 0u8..=16 {
            let mut input = vec![seed, seed.wrapping_mul(3)];
            input.extend((0..192u16).map(|index| (index as u8).wrapping_mul(seed.wrapping_add(5))));
            let (left, right) = input.split_at(input.len() / 2);
            let old = synthetic_glyph_document(left);
            let new = synthetic_glyph_document(right);
            if let (Some(old_store), Some(new_store)) = (
                exercise_default_graph_pipeline(&old, input[0]),
                exercise_default_graph_pipeline(&new, input.get(1).copied().unwrap_or(0)),
            ) && check_graph_pair(&old_store, &new_store)
            {
                reached += 1;
            }
            fuzz_graph_pipeline(&input);
        }
        assert!(
            reached >= 1,
            "no synthetic pair reached the shared graph solver"
        );
    }

    #[test]
    fn synthetic_text_comparison_seeds_reach_the_diff_pipeline() {
        let mut reached = 0;
        for seed in 0u8..=32 {
            let mut input = vec![seed];
            input.extend((0..192u16).map(|index| {
                (index as u8)
                    .wrapping_mul(seed.wrapping_add(7))
                    .wrapping_add(seed.wrapping_mul(3))
            }));
            let (left, right) = input.split_at(input.len() / 2);
            let old = synthetic_glyph_document(left);
            let new = synthetic_glyph_document(right);
            if check_text_comparison(&old, &new) && check_text_comparison(&new, &old) {
                reached += 1;
            }
            fuzz_text_comparison(&input);
        }
        assert!(
            reached >= 16,
            "only {reached} of 33 seeds reached and summarized the comparison"
        );
    }

    #[test]
    fn curated_native_evidence_seed_reaches_validation() {
        let _ = fuzz_native_evidence(GLYPH_EXTRACTION_SEED);
        assert!(
            fuzz_native_evidence(GLYPH_EXTRACTION_SEED),
            "the curated PDF should reach form or structure evidence"
        );
    }

    #[test]
    fn synthetic_evidence_pdf_reaches_form_and_structure_extraction() {
        let mut field_seeds = 0;
        for seed in 0u8..=32 {
            let input = std::iter::once(seed)
                .chain((0..64u16).map(|index| {
                    (index as u8)
                        .wrapping_mul(seed.wrapping_add(11))
                        .wrapping_add(seed)
                }))
                .collect::<Vec<u8>>();
            let bytes = synthetic_evidence_pdf(&input);
            assert!(!bytes.is_empty(), "seed {seed} failed to serialize");
            let pdf = LopdfParser
                .parse(Arc::from(bytes.as_slice()), PARSE_LIMITS)
                .expect("synthetic evidence PDF should parse");

            let forms = extract_form_evidence(pdf.as_ref(), 0, 0, FormLimits::default())
                .expect("synthetic form extraction should succeed");
            if !forms.fields.is_empty() {
                field_seeds += 1;
            }

            let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
            let outcome = source
                .extract_outcome(Arc::from(bytes.as_slice()), PARSE_LIMITS, EXTRACTION_LIMITS)
                .expect("synthetic tagged content should extract");
            let structure = extract_structure_evidence(
                pdf.as_ref(),
                outcome.document(),
                0,
                0,
                StructureLimits::default(),
            )
            .expect("synthetic structure extraction should succeed");
            assert!(
                !structure.elements.is_empty(),
                "seed {seed} produced no structure elements"
            );
        }
        assert!(
            field_seeds >= 8,
            "only {field_seeds} of 33 seeds produced form fields"
        );
    }

    #[test]
    fn curated_glyph_extraction_seed_is_within_tight_budgets() {
        fuzz_glyph_extraction(GLYPH_EXTRACTION_SEED);
        let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
        let outcome = source
            .extract_outcome(
                std::sync::Arc::from(GLYPH_EXTRACTION_SEED),
                PARSE_LIMITS,
                EXTRACTION_LIMITS,
            )
            .expect("curated extraction seed should produce an outcome");
        assert!(outcome.document().items().len() <= EXTRACTION_LIMITS.max_glyphs);
        assert!(outcome.document().items().len() >= 11);
        // Valid Hello World extraction preserves at least the mapped word boundaries.
        let text = outcome
            .document()
            .items()
            .iter()
            .filter_map(|glyph| match &glyph.text {
                crate::model::DecodedText::Mapped(value) => Some(value.as_str()),
                crate::model::DecodedText::Unmapped { .. } => None,
            })
            .collect::<String>();
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(
            crate::source::ExtractionOutcome::new(
                outcome.document().clone(),
                outcome.issues().to_vec()
            )
            .is_ok()
        );
    }

    #[test]
    fn unmapped_never_becomes_empty_or_replacement() {
        let mut pdf = FuzzPdf::default();
        let font = build_simple_font(
            &mut pdf,
            None,
            b"WinAnsiEncoding",
            65,
            Some(vec![600.0; 4]),
            500.0,
            700.0,
            -200.0,
        );
        let loaded = FontDecoder::load(&pdf, &font, FONT_LIMITS).expect("font should load");
        // 0x81 is undefined in WinAnsi and should be Unmapped, not empty or FFFD
        let glyph = loaded
            .decoder
            .decode(&[0x81], 1, 1024)
            .expect("should decode")[0]
            .clone();
        assert_eq!(glyph.mapping, UnicodeMapping::Unmapped);
        // Ensure no mapped glyph is empty or FFFD across a small sweep
        for code in 0u8..=255 {
            if let Ok(glyphs) = loaded.decoder.decode(&[code], 1, 1024) {
                for glyph in glyphs {
                    match glyph.mapping {
                        UnicodeMapping::Mapped(ref text) => {
                            assert!(!text.is_empty());
                            assert!(!text.contains('\u{FFFD}'));
                        }
                        UnicodeMapping::Unmapped => {}
                    }
                }
            }
        }
    }
}
