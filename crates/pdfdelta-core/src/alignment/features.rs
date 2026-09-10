use std::collections::{HashMap, HashSet};

use crate::{
    Error, Result,
    layout::{BlockId, BlockRole},
    normalize::{BlockText, ComparableToken, FontSizeSignature, PositionSignature},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExactHash(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NGram(pub Vec<ComparableToken>);

pub type NGramSet = HashSet<NGram>;
pub type NGramCounts = HashMap<NGram, usize>;
const PAGE_POSITION_SCALE: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockFeatures {
    pub block: BlockId,
    pub role: BlockRole,
    pub exact_hash: ExactHash,
    pub canonical_tokens: Vec<ComparableToken>,
    pub matching_tokens: Vec<ComparableToken>,
    pub ngram_counts: NGramCounts,
    pub ngram_size: usize,
    pub page_position: Option<u16>,
    pub numeric_mask_applied: bool,
    pub has_normalization_issues: bool,
    pub first_position: Option<PositionSignature>,
    pub last_position: Option<PositionSignature>,
    pub first_page: Option<u32>,
    pub last_page: Option<u32>,
    pub first_font_size: Option<FontSizeSignature>,
    pub last_font_size: Option<FontSizeSignature>,
}

pub fn build_block_features(blocks: &[BlockText], ngram_size: usize) -> Result<Vec<BlockFeatures>> {
    validate_ngram_size(ngram_size)?;

    let mut block_ids = HashSet::with_capacity(blocks.len());
    let mut features = Vec::with_capacity(blocks.len());
    let page_bounds = blocks
        .iter()
        .flat_map(|block| block.pages.iter().copied())
        .fold(None::<(u32, u32)>, |bounds, page| match bounds {
            Some((min, max)) => Some((min.min(page), max.max(page))),
            None => Some((page, page)),
        });
    for block in blocks {
        if !block_ids.insert(block.block) {
            return Err(Error::Unresolved(format!(
                "duplicate normalized block id {}",
                block.block.0
            )));
        }
        let canonical_tokens = block.canonical.comparable_tokens()?;
        let matching_tokens = block.matching_tokens.clone();
        let ngram_counts = token_ngram_counts(&matching_tokens, ngram_size);
        features.push(BlockFeatures {
            block: block.block,
            role: block.role,
            exact_hash: exact_hash(&canonical_tokens),
            canonical_tokens,
            ngram_counts,
            matching_tokens,
            ngram_size,
            page_position: relative_page_position(block.pages.first().copied(), page_bounds),
            numeric_mask_applied: block.numeric_mask_applied,
            has_normalization_issues: !block.issues.is_empty(),
            first_position: block
                .position_signatures
                .as_ref()
                .and_then(|positions| positions.first())
                .copied(),
            last_position: block
                .position_signatures
                .as_ref()
                .and_then(|positions| positions.last())
                .copied(),
            first_page: block.pages.first().copied(),
            last_page: block.pages.last().copied(),
            first_font_size: block
                .font_size_signatures
                .as_ref()
                .and_then(|sizes| sizes.first())
                .cloned(),
            last_font_size: block
                .font_size_signatures
                .as_ref()
                .and_then(|sizes| sizes.last())
                .cloned(),
        });
    }
    Ok(features)
}

fn relative_page_position(page: Option<u32>, bounds: Option<(u32, u32)>) -> Option<u16> {
    let page = page?;
    let (min, max) = bounds?;
    let span = u64::from(max.checked_sub(min)?);
    if span == 0 {
        return Some(0);
    }
    let offset = u64::from(page.checked_sub(min)?);
    let scaled = offset.checked_mul(PAGE_POSITION_SCALE)?.checked_div(span)?;
    u16::try_from(scaled).ok()
}

#[must_use]
pub fn dice_similarity(left: &NGramSet, right: &NGramSet) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let shared = left.intersection(right).count();
    2.0 * shared as f64 / (left.len() + right.len()) as f64
}

#[must_use]
pub fn multiset_dice_similarity(left: &NGramCounts, right: &NGramCounts) -> f64 {
    // Shared mass is symmetric, so iterating the smaller multiset minimizes
    // lookups without changing the result.
    let (queried, indexed) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    let left_total = left.values().map(|count| *count as f64).sum::<f64>();
    let right_total = right.values().map(|count| *count as f64).sum::<f64>();
    let total = left_total + right_total;
    if total == 0.0 {
        return 1.0;
    }
    let shared = queried
        .iter()
        .map(|(ngram, count)| *count.min(indexed.get(ngram).unwrap_or(&0)) as f64)
        .sum::<f64>();
    2.0 * shared / total
}

pub(crate) fn validate_feature_ids(side: &str, features: &[BlockFeatures]) -> Result<()> {
    let mut ids = HashSet::with_capacity(features.len());
    for features in features {
        if !ids.insert(features.block) {
            return Err(Error::Unresolved(format!(
                "duplicate {side} block feature id {}",
                features.block.0
            )));
        }
    }
    Ok(())
}

pub(crate) fn token_ngram_counts(tokens: &[ComparableToken], size: usize) -> NGramCounts {
    if tokens.is_empty() {
        return HashMap::new();
    }
    if tokens.len() <= size {
        let mut grams = HashMap::new();
        grams.insert(NGram(tokens.to_vec()), 1);
        if tokens.len() > 1 {
            for token in tokens {
                *grams.entry(NGram(vec![token.clone()])).or_default() += 1;
            }
        }
        return grams;
    }
    let mut grams = HashMap::new();
    for window in tokens.windows(size) {
        *grams.entry(NGram(window.to_vec())).or_default() += 1;
    }
    grams
}

pub(crate) fn validate_ngram_size(size: usize) -> Result<()> {
    if size == 0 {
        return Err(Error::InvalidConfiguration(
            "ngram_size must be greater than zero".to_owned(),
        ));
    }
    Ok(())
}

pub fn estimate_ngram_token_elements(
    blocks: &[BlockText],
    size: usize,
    limit: usize,
) -> Result<usize> {
    validate_ngram_size(size)?;
    let mut total = 0_usize;
    for block in blocks {
        let token_count = block.matching_tokens.len();
        let elements = if token_count == 0 {
            Some(0)
        } else if token_count <= size {
            token_count.checked_mul(2)
        } else {
            token_count
                .checked_sub(size)
                .and_then(|windows| windows.checked_add(1))
                .and_then(|windows| windows.checked_mul(size))
        }
        .ok_or(Error::LimitExceeded {
            resource: "alignment n-gram token elements",
            limit,
        })?;
        total = total.checked_add(elements).ok_or(Error::LimitExceeded {
            resource: "alignment n-gram token elements",
            limit,
        })?;
    }
    Ok(total)
}

fn exact_hash(tokens: &[ComparableToken]) -> ExactHash {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    let mut write = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };

    for token in tokens {
        match token {
            ComparableToken::Scalar(scalar) => {
                write(&[0]);
                write(&u32::from(*scalar).to_le_bytes());
            }
            ComparableToken::Unmapped {
                font_hash,
                glyph_id,
            } => {
                write(&[1]);
                write(&(font_hash.0.len() as u64).to_le_bytes());
                write(&font_hash.0);
                write(&glyph_id.to_le_bytes());
            }
        }
    }
    ExactHash(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_ngrams_preserve_counts_and_affect_similarity() {
        let token = ComparableToken::Scalar('a');
        let repeated = token_ngram_counts(&[token.clone(), token.clone(), token.clone()], 2);
        let single = HashMap::from([(NGram(vec![token.clone(), token]), 1)]);

        assert_eq!(repeated.values().copied().collect::<Vec<_>>(), [2]);
        assert_eq!(multiset_dice_similarity(&single, &repeated), 2.0 / 3.0);
    }

    #[test]
    fn page_positions_are_relative_to_each_document() {
        assert_eq!(relative_page_position(Some(10), Some((10, 20))), Some(0));
        assert_eq!(
            relative_page_position(Some(15), Some((10, 20))),
            Some(5_000)
        );
        assert_eq!(
            relative_page_position(Some(20), Some((10, 20))),
            Some(10_000)
        );
        assert_eq!(relative_page_position(Some(7), Some((7, 7))), Some(0));
        assert_eq!(relative_page_position(None, Some((7, 7))), None);
    }
}
