//! Source-backed optional tokens for unresolved discretionary line-end hyphens.

use super::{GroupText, Side, allocation_error, charge, space_token};
use crate::{Result, alignment::BlockSeparator, normalize::ComparableToken};

/// Returns a compact linear DAG: each marked token has retain and skip edges.
/// Unrecognized or incomplete issue evidence produces no hypothesis set.
pub(super) fn optional_tokens(
    side: &Side<'_>,
    group: &GroupText,
    remaining: &mut usize,
) -> Result<Option<Vec<bool>>> {
    let mut optional = Vec::new();
    optional
        .try_reserve_exact(group.tokens.len())
        .map_err(|_| allocation_error("normalization DAG"))?;
    optional.resize(group.tokens.len(), false);
    let mut offset = 0usize;
    let mut preceding_space = false;
    for (position, block_id) in group.blocks.iter().enumerate() {
        let block_index = side.index[block_id];
        let block = &side.blocks[block_index];
        let tokens = &side.canonical[block_index];
        let separator = position > 0
            && group.separator.map(|separator| separator.at(position - 1))
                == Some(BlockSeparator::Space)
            && !preceding_space
            && !tokens.first().is_some_and(space_token);
        offset += usize::from(separator);
        if !block.issues.is_empty() {
            let work = block
                .raw
                .text
                .len()
                .saturating_add(block.canonical.text.len())
                .saturating_add(block.raw.source_map.len())
                .saturating_add(block.canonical.source_map.len())
                .saturating_add(block.normalization_events.len())
                .saturating_add(block.canonical.unmapped.len())
                .saturating_mul(block.issues.len().saturating_add(1));
            if !charge(remaining, work) {
                return Ok(None);
            }
            let Ok(ranges) = block.checked_normalization_issue_ranges() else {
                return Ok(None);
            };
            for (issue, range) in block.issues.iter().zip(ranges) {
                let start = range.start
                    + block
                        .canonical
                        .unmapped
                        .partition_point(|token| token.scalar_index <= range.start);
                let absolute = offset + start;
                let group_end = group.comparable_origin + group.tokens.len();
                let issue_start = absolute.saturating_sub(usize::from(range.start == range.end));
                let issue_end = absolute.saturating_add((range.end - range.start).max(1));
                if issue_start >= group_end || issue_end <= group.comparable_origin {
                    continue;
                }
                let raw_index = issue.raw_range.start;
                if raw_index == 0 {
                    return Ok(None);
                }
                let mut context = block.raw.text.chars().skip(raw_index - 1);
                let [preceding, raw_hyphen, following_break, following_letter] =
                    std::array::from_fn(|_| context.next());
                let supported = range.end == range.start + 1
                    && issue.raw_range.end == raw_index + 1
                    && matches!(raw_hyphen, Some('-' | '\u{2010}'))
                    && following_break == Some('\n')
                    && preceding.is_some_and(char::is_alphabetic)
                    && following_letter.is_some_and(char::is_lowercase)
                    && tokens.get(start) == raw_hyphen.map(ComparableToken::Scalar).as_ref();
                if !supported || absolute < group.comparable_origin {
                    return Ok(None);
                }
                optional[absolute - group.comparable_origin] = true;
            }
        }
        offset += tokens.len();
        preceding_space = tokens
            .last()
            .map_or(separator || preceding_space, space_token);
    }
    Ok(Some(optional))
}
