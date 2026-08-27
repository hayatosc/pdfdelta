//! Fuzzing-only entry points for internal parsers.

use crate::pdf::font::cmap::{CMapLimits, parse_identity_cid_encoding, parse_to_unicode_for_width};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const LIMITS: CMapLimits = CMapLimits {
    max_entries: 1_024,
    max_code_bytes: 4,
    max_output_scalars: 4_096,
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
}
