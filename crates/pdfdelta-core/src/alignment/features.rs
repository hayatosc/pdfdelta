use std::collections::{HashMap, HashSet};

use crate::{
    Error, Result,
    layout::BlockId,
    normalize::{BlockText, ComparableToken},
};

pub const DEFAULT_ANCHOR_MIN_TOKENS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExactHash(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NGram(pub Vec<ComparableToken>);

pub type NGramSet = HashSet<NGram>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockFeatures {
    pub block: BlockId,
    pub exact_hash: ExactHash,
    pub canonical_tokens: Vec<ComparableToken>,
    pub matching_tokens: Vec<ComparableToken>,
    pub ngrams: NGramSet,
    pub ngram_size: usize,
    pub numeric_mask_applied: bool,
    pub has_normalization_issues: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactAnchor {
    pub old: BlockId,
    pub new: BlockId,
}

pub fn build_block_features(blocks: &[BlockText], ngram_size: usize) -> Result<Vec<BlockFeatures>> {
    validate_ngram_size(ngram_size)?;

    let mut block_ids = HashSet::with_capacity(blocks.len());
    let mut features = Vec::with_capacity(blocks.len());
    for block in blocks {
        if !block_ids.insert(block.block) {
            return Err(Error::Unresolved(format!(
                "duplicate normalized block id {}",
                block.block.0
            )));
        }
        let canonical_tokens = block.canonical.comparable_tokens()?;
        let matching_tokens = block.matching_tokens.clone();
        let has_unmapped_tokens = canonical_tokens
            .iter()
            .any(|token| matches!(token, ComparableToken::Unmapped { .. }));
        features.push(BlockFeatures {
            block: block.block,
            exact_hash: exact_hash(&canonical_tokens),
            canonical_tokens,
            ngrams: token_ngrams(&matching_tokens, ngram_size),
            matching_tokens,
            ngram_size,
            numeric_mask_applied: block.numeric_mask_applied,
            has_normalization_issues: !block.issues.is_empty() || has_unmapped_tokens,
        });
    }
    Ok(features)
}

pub fn exact_anchors(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    min_token_count: usize,
) -> Result<Vec<ExactAnchor>> {
    if min_token_count == 0 {
        return Err(Error::InvalidConfiguration(
            "anchor min_token_count must be greater than zero".to_owned(),
        ));
    }
    validate_feature_ids("old", old)?;
    validate_feature_ids("new", new)?;

    let old_counts = exact_token_counts(old, min_token_count);
    let new_counts = exact_token_counts(new, min_token_count);
    let new_blocks = new
        .iter()
        .filter(|features| {
            !features.has_normalization_issues && features.canonical_tokens.len() >= min_token_count
        })
        .map(|features| (features.canonical_tokens.clone(), features.block))
        .collect::<HashMap<_, _>>();

    Ok(old
        .iter()
        .filter(|features| {
            !features.has_normalization_issues && features.canonical_tokens.len() >= min_token_count
        })
        .filter_map(|features| {
            let tokens = &features.canonical_tokens;
            (old_counts.get(tokens) == Some(&1) && new_counts.get(tokens) == Some(&1)).then(|| {
                ExactAnchor {
                    old: features.block,
                    new: new_blocks[tokens],
                }
            })
        })
        .collect())
}

pub fn dice_similarity(left: &NGramSet, right: &NGramSet) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let shared = left.intersection(right).count();
    2.0 * shared as f64 / (left.len() + right.len()) as f64
}

fn exact_token_counts(
    features: &[BlockFeatures],
    min_token_count: usize,
) -> HashMap<Vec<ComparableToken>, usize> {
    let mut counts = HashMap::new();
    for features in features.iter().filter(|features| {
        !features.has_normalization_issues && features.canonical_tokens.len() >= min_token_count
    }) {
        *counts.entry(features.canonical_tokens.clone()).or_default() += 1;
    }
    counts
}

fn validate_feature_ids(side: &str, features: &[BlockFeatures]) -> Result<()> {
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

pub(crate) fn token_ngrams(tokens: &[ComparableToken], size: usize) -> NGramSet {
    if tokens.is_empty() {
        return HashSet::new();
    }
    if tokens.len() <= size {
        let mut grams = HashSet::from([NGram(tokens.to_vec())]);
        grams.extend(tokens.iter().cloned().map(|token| NGram(vec![token])));
        return grams;
    }
    tokens
        .windows(size)
        .map(|window| NGram(window.to_vec()))
        .collect()
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
