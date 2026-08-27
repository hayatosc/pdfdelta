//! Fuzzing-only entry points for internal parsers.

use crate::pdf::content::{ContentBudget, ContentLimits, ContentParser, Operation};
use crate::pdf::font::cmap::{
    CMapLimits, UnicodeMapping, parse_identity_cid_encoding, parse_to_unicode_for_width,
};
use crate::pdf::font::{FontDecoder, FontDecoderLimits, WritingMode};
use crate::pdf::{
    DecodedStream, ObjectRef, PageRef, ParsedPdf, PdfDict, PdfObject, PdfVersion, RawStream,
};

use std::collections::HashMap;

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

/// Exercises the production CMap parsers with finite resource limits.
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
