//! Fuzzing-only entry points for internal parsers.

use crate::pdf::content::{ContentBudget, ContentLimits, ContentParser, Operation};
use crate::pdf::font::cmap::{CMapLimits, parse_identity_cid_encoding, parse_to_unicode_for_width};

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
}
