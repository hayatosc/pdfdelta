use crate::normalize::ComparableToken;

use super::{BlockFeatures, NGramCounts, features::token_ngram_counts, multiset_dice_similarity};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// Precomputed so each variant's n-gram counts are built once per group
    /// instead of once per (old variant, new variant) scoring pair.
    canonical_counts: NGramCounts,
    matching_counts: NGramCounts,
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
        .or(new.first())
        .map_or(3, |features| features.ngram_size);
    // Feature construction already validated `ngram_size`; deriving it again
    // here must not turn scoring into a fallible path.
    let old_variants = group_variants(old, ngram_size);
    let new_variants = group_variants(new, ngram_size);
    let numeric_mask = old
        .iter()
        .chain(new)
        .any(|features| features.numeric_mask_applied);
    let split_merge = old.len() != new.len();
    let mut scores = Vec::with_capacity(old_variants.len() * new_variants.len());

    for old in &old_variants {
        for new in &new_variants {
            let exact_canonical = old.canonical == new.canonical;
            let canonical_similarity = precomputed_token_similarity(
                &old.canonical,
                &old.canonical_counts,
                &new.canonical,
                &new.canonical_counts,
            );
            let matching_similarity = precomputed_token_similarity(
                &old.matching,
                &old.matching_counts,
                &new.matching,
                &new.matching_counts,
            );
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

fn group_variants(features: &[BlockFeatures], ngram_size: usize) -> Vec<GroupVariant> {
    let Some(first) = features.first() else {
        return vec![empty_group_variant()];
    };
    if features.len() == 1 {
        return vec![GroupVariant {
            canonical: first.canonical_tokens.clone(),
            matching: first.matching_tokens.clone(),
            canonical_counts: token_ngram_counts(&first.canonical_tokens, ngram_size),
            matching_counts: token_ngram_counts(&first.matching_tokens, ngram_size),
            separator: None,
        }];
    }

    let mut variants = vec![group_variant(
        features,
        BlockSeparator::Concatenate,
        ngram_size,
    )];
    let with_space = group_variant(features, BlockSeparator::Space, ngram_size);
    if variants[0].canonical != with_space.canonical || variants[0].matching != with_space.matching
    {
        variants.push(with_space);
    }
    variants
}

fn empty_group_variant() -> GroupVariant {
    GroupVariant {
        canonical: Vec::new(),
        matching: Vec::new(),
        canonical_counts: NGramCounts::new(),
        matching_counts: NGramCounts::new(),
        separator: None,
    }
}

fn group_variant(
    features: &[BlockFeatures],
    separator: BlockSeparator,
    ngram_size: usize,
) -> GroupVariant {
    let first = &features[0];
    let mut canonical = first.canonical_tokens.clone();
    let mut matching = first.matching_tokens.clone();
    for next in &features[1..] {
        separator.append(&mut canonical, &next.canonical_tokens);
        separator.append(&mut matching, &next.matching_tokens);
    }
    GroupVariant {
        canonical_counts: token_ngram_counts(&canonical, ngram_size),
        matching_counts: token_ngram_counts(&matching, ngram_size),
        canonical,
        matching,
        separator: Some(separator),
    }
}

fn is_space(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(scalar) if scalar.is_whitespace())
}

fn precomputed_token_similarity(
    left: &[ComparableToken],
    left_counts: &NGramCounts,
    right: &[ComparableToken],
    right_counts: &NGramCounts,
) -> f64 {
    if left == right {
        return 1.0;
    }
    multiset_dice_similarity(left_counts, right_counts)
}
