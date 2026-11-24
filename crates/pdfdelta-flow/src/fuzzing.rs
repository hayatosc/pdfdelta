//! Fuzzing-only entry points for the flow engine.

use crate::{FlowToken, flow_diff};
use pdfdelta_core::model::PageId;

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_TOKENS: usize = 8_192;

/// Compares two token streams derived from the input halves.
///
/// Inputs larger than 64 KiB are ignored. Each byte becomes one single-scalar
/// token, so the two halves exercise deletion, insertion, replacement, move,
/// and layout-only classification without a parser or extractor in the loop.
/// A successful diff must replay through the same verified script contract as
/// production input.
///
/// # Panics
///
/// Panics if a successful diff violates its token or change accounting.
#[doc(hidden)]
pub fn fuzz_text_comparison(input: &[u8]) {
    if input.len() > MAX_INPUT_BYTES || input.len() < 2 {
        return;
    }
    let (left, right) = input.split_at(input.len() / 2);
    if left.is_empty() || right.is_empty() {
        return;
    }
    let old = tokens(left);
    let new = tokens(right);
    let Ok(diff) = flow_diff(&old, &new) else {
        return;
    };
    let stats = diff.stats();
    assert_eq!(stats.old_tokens, old.len());
    assert_eq!(stats.new_tokens, new.len());
    let counted = stats
        .delete
        .changes
        .saturating_add(stats.insert.changes)
        .saturating_add(stats.replace.changes)
        .saturating_add(stats.moves.changes)
        .saturating_add(stats.layout_only.changes);
    assert_eq!(counted, diff.changes.len());
    for change in &diff.changes {
        let old_range = change.old_range();
        let new_range = change.new_range();
        assert!(old_range.end <= old.len());
        assert!(new_range.end <= new.len());
        assert!(old_range.start <= old_range.end);
        assert!(new_range.start <= new_range.end);
    }
}

fn tokens(input: &[u8]) -> Vec<FlowToken> {
    input
        .iter()
        .take(MAX_TOKENS)
        .map(|byte| FlowToken {
            text: char::from(*byte).to_string(),
            page: PageId(0),
            bounds: None,
            block: 0,
            uncertain: false,
            line_break_after: false,
            font_size: None,
            raw: None,
            position: None,
            page_break_after: false,
            normalization: 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curated_flow_seeds_reach_the_verified_diff() {
        for seed in [
            b"ab".as_slice(),
            b"aaaa",
            b"abab",
            b"0123456789",
            b"\xff\x00\x80",
        ] {
            fuzz_text_comparison(seed);
        }
    }
}
