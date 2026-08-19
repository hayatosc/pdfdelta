use crate::normalize::ComparableToken;

use super::{BlockFeatures, dice_similarity, features::token_ngrams};

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
}

#[derive(Clone)]
struct GroupVariant {
    canonical: Vec<ComparableToken>,
    matching: Vec<ComparableToken>,
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
            scores.push((score, canonical_similarity, exact_canonical));
        }
    }

    scores.sort_by(|left, right| right.0.total_cmp(&left.0));
    let best = scores.first().copied().unwrap_or((0.0, 0.0, false));
    let separator_ambiguous = split_merge
        && scores
            .get(1)
            .is_some_and(|second| best.0 - second.0 < options.min_score_margin)
        && !best.2;

    GroupScore {
        score: best.0,
        canonical_similarity: best.1,
        exact_canonical: best.2,
        numeric_mask,
        separator_ambiguous,
    }
}

fn group_variants(features: &[BlockFeatures]) -> Vec<GroupVariant> {
    let Some(first) = features.first() else {
        return vec![GroupVariant {
            canonical: Vec::new(),
            matching: Vec::new(),
        }];
    };
    if features.len() == 1 {
        return vec![GroupVariant {
            canonical: first.canonical_tokens.clone(),
            matching: first.matching_tokens.clone(),
        }];
    }

    let second = &features[1];
    let mut variants = vec![GroupVariant {
        canonical: concatenate(&first.canonical_tokens, &second.canonical_tokens, false),
        matching: concatenate(&first.matching_tokens, &second.matching_tokens, false),
    }];
    let with_space = GroupVariant {
        canonical: concatenate(&first.canonical_tokens, &second.canonical_tokens, true),
        matching: concatenate(&first.matching_tokens, &second.matching_tokens, true),
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
    with_space: bool,
) -> Vec<ComparableToken> {
    let insert_space =
        with_space && !first.last().is_some_and(is_space) && !second.first().is_some_and(is_space);
    let mut combined = Vec::with_capacity(first.len() + second.len() + usize::from(insert_space));
    combined.extend_from_slice(first);
    if insert_space {
        combined.push(ComparableToken::Scalar(' '));
    }
    combined.extend_from_slice(second);
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
