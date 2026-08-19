use crate::normalize::ComparableToken;

use super::{BlockFeatures, dice_similarity, features::token_ngrams};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockSeparator {
    Concatenate,
    Space,
}

impl BlockSeparator {
    pub(crate) fn append(self, combined: &mut Vec<ComparableToken>, next: &[ComparableToken]) {
        let insert_space = self == Self::Space
            && !combined.last().is_some_and(is_space)
            && !next.first().is_some_and(is_space);
        if insert_space {
            combined.push(ComparableToken::Scalar(' '));
        }
        combined.extend_from_slice(next);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScoreOptions {
    pub matching_weight: f64,
    pub canonical_weight: f64,
    pub min_score_margin: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct GroupScore {
    pub score: f64,
    pub canonical_similarity: f64,
    pub exact_canonical: bool,
    pub numeric_mask: bool,
    pub separator_ambiguous: bool,
    pub old_separator: Option<BlockSeparator>,
    pub new_separator: Option<BlockSeparator>,
}

#[derive(Clone)]
struct GroupVariant {
    canonical: Vec<ComparableToken>,
    matching: Vec<ComparableToken>,
    separator: Option<BlockSeparator>,
}

#[derive(Clone, Copy)]
struct VariantScore {
    score: f64,
    canonical_similarity: f64,
    exact_canonical: bool,
    old_separator: Option<BlockSeparator>,
    new_separator: Option<BlockSeparator>,
}

pub(crate) fn score_groups(
    old: &[BlockFeatures],
    new: &[BlockFeatures],
    options: ScoreOptions,
) -> GroupScore {
    let ngram_size = old
        .first()
        .or_else(|| new.first())
        .map_or(3, |features| features.ngram_size);
    let old_variants = group_variants(old);
    let new_variants = group_variants(new);
    let numeric_mask = old
        .iter()
        .chain(new)
        .any(|features| features.numeric_mask_applied);
    let split_merge = old.len() != new.len();
    let mut scores = Vec::with_capacity(old_variants.len() * new_variants.len());

    for old in &old_variants {
        for new in &new_variants {
            let exact_canonical = old.canonical == new.canonical;
            let canonical_similarity = token_similarity(&old.canonical, &new.canonical, ngram_size);
            let matching_similarity = token_similarity(&old.matching, &new.matching, ngram_size);
            let score = if exact_canonical {
                1.0
            } else {
                options.matching_weight * matching_similarity
                    + options.canonical_weight * canonical_similarity
            };
            scores.push(VariantScore {
                score,
                canonical_similarity,
                exact_canonical,
                old_separator: old.separator,
                new_separator: new.separator,
            });
        }
    }

    scores.sort_by(|left, right| right.score.total_cmp(&left.score));
    let best = scores.first().copied().unwrap_or(VariantScore {
        score: 0.0,
        canonical_similarity: 0.0,
        exact_canonical: false,
        old_separator: None,
        new_separator: None,
    });
    let separator_ambiguous = split_merge
        && scores
            .get(1)
            .is_some_and(|second| best.score - second.score < options.min_score_margin)
        && !best.exact_canonical;

    GroupScore {
        score: best.score,
        canonical_similarity: best.canonical_similarity,
        exact_canonical: best.exact_canonical,
        numeric_mask,
        separator_ambiguous,
        old_separator: best.old_separator,
        new_separator: best.new_separator,
    }
}

fn group_variants(features: &[BlockFeatures]) -> Vec<GroupVariant> {
    let Some(first) = features.first() else {
        return vec![GroupVariant {
            canonical: Vec::new(),
            matching: Vec::new(),
            separator: None,
        }];
    };
    if features.len() == 1 {
        return vec![GroupVariant {
            canonical: first.canonical_tokens.clone(),
            matching: first.matching_tokens.clone(),
            separator: None,
        }];
    }

    let second = &features[1];
    let mut variants = vec![GroupVariant {
        canonical: concatenate(
            &first.canonical_tokens,
            &second.canonical_tokens,
            BlockSeparator::Concatenate,
        ),
        matching: concatenate(
            &first.matching_tokens,
            &second.matching_tokens,
            BlockSeparator::Concatenate,
        ),
        separator: Some(BlockSeparator::Concatenate),
    }];
    let with_space = GroupVariant {
        canonical: concatenate(
            &first.canonical_tokens,
            &second.canonical_tokens,
            BlockSeparator::Space,
        ),
        matching: concatenate(
            &first.matching_tokens,
            &second.matching_tokens,
            BlockSeparator::Space,
        ),
        separator: Some(BlockSeparator::Space),
    };
    if variants[0].canonical != with_space.canonical || variants[0].matching != with_space.matching
    {
        variants.push(with_space);
    }
    variants
}

fn concatenate(
    first: &[ComparableToken],
    second: &[ComparableToken],
    separator: BlockSeparator,
) -> Vec<ComparableToken> {
    let mut combined = Vec::with_capacity(first.len() + second.len() + 1);
    combined.extend_from_slice(first);
    separator.append(&mut combined, second);
    combined
}

fn is_space(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(scalar) if scalar.is_whitespace())
}

fn token_similarity(left: &[ComparableToken], right: &[ComparableToken], ngram_size: usize) -> f64 {
    if left == right {
        return 1.0;
    }
    dice_similarity(
        &token_ngrams(left, ngram_size),
        &token_ngrams(right, ngram_size),
    )
}
