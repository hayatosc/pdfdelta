//! Source-backed optional tokens for unresolved discretionary line-end hyphens.

use super::{GroupText, Side, allocation_error, charge_work, space_token};
use crate::{Result, alignment::BlockSeparator};

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
            if !charge_work(remaining, work) {
                return Ok(None);
            }
            let interval = group.comparable_origin.saturating_sub(offset)
                ..(group.comparable_origin + group.tokens.len()).saturating_sub(offset);
            let Ok(Some(positions)) = block.checked_optional_hyphens(tokens, interval) else {
                return Ok(None);
            };
            for position in positions {
                optional[offset + position - group.comparable_origin] = true;
            }
        }
        offset += tokens.len();
        preceding_space = tokens
            .last()
            .map_or(separator || preceding_space, space_token);
    }
    Ok(Some(optional))
}
