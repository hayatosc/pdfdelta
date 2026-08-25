use std::collections::{HashMap, HashSet};

use crate::{Error, Result, layout::BlockId};

use super::features::{BlockFeatures, ExactHash, NGram, dice_similarity};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CandidateSource {
    Exact,
    NGramInvertedIndex,
    ShortBlockFallback,
    Exhaustive,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub block: BlockId,
    pub sources: Vec<CandidateSource>,
    pub coarse_score: f64,
}

/// Decomposed candidate visit estimate for one query block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateVisitBreakdown {
    pub exact: usize,
    pub ngram: usize,
    pub short_fallback: usize,
}

/// Total candidate visit estimate for one query block plus, when the
/// generator can decompose it, the exact/ngram/short-fallback breakdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateVisitEstimate {
    pub total: usize,
    pub breakdown: Option<CandidateVisitBreakdown>,
}

pub trait CandidateGenerator {
    /// Returns an upper bound on posting or feature visits performed by `candidates`.
    ///
    /// Implementations must never underestimate this work. They should return a conservative
    /// value, including visits that do not ultimately produce a candidate.
    fn estimated_visits(&self, old: &BlockFeatures, limit: usize) -> Result<usize>;

    /// Returns the total visit estimate plus, when the implementation can
    /// decompose it, the exact/ngram/short-fallback breakdown. The default
    /// delegates to `estimated_visits` with no breakdown.
    fn estimate_visits(&self, old: &BlockFeatures, limit: usize) -> Result<CandidateVisitEstimate> {
        Ok(CandidateVisitEstimate {
            total: self.estimated_visits(old, limit)?,
            breakdown: None,
        })
    }

    fn candidates(&self, old: &BlockFeatures, limit: usize) -> Result<Vec<Candidate>>;
}

pub struct InvertedIndexCandidateGenerator {
    new_features: HashMap<BlockId, BlockFeatures>,
    exact_index: HashMap<ExactHash, Vec<BlockId>>,
    ngram_index: HashMap<NGram, Vec<BlockId>>,
    ngram_size: Option<usize>,
}

impl InvertedIndexCandidateGenerator {
    pub fn new(new: &[BlockFeatures]) -> Result<Self> {
        let ngram_size = common_ngram_size(new)?;
        let mut new_features = HashMap::with_capacity(new.len());
        let mut exact_index = HashMap::<ExactHash, Vec<BlockId>>::new();
        let mut ngram_index = HashMap::<NGram, Vec<BlockId>>::new();

        for features in new {
            if new_features
                .insert(features.block, features.clone())
                .is_some()
            {
                return Err(Error::Unresolved(format!(
                    "duplicate candidate block id {}",
                    features.block.0
                )));
            }
            exact_index
                .entry(features.exact_hash)
                .or_default()
                .push(features.block);
            for ngram in &features.ngrams {
                ngram_index
                    .entry(ngram.clone())
                    .or_default()
                    .push(features.block);
            }
        }

        for blocks in exact_index.values_mut() {
            blocks.sort_by_key(|block| block.0);
        }
        for blocks in ngram_index.values_mut() {
            blocks.sort_by_key(|block| block.0);
        }

        Ok(Self {
            new_features,
            exact_index,
            ngram_index,
            ngram_size,
        })
    }

    fn idf(&self, ngram: &NGram) -> f64 {
        let document_count = self.new_features.len() as f64;
        let document_frequency =
            self.ngram_index.get(ngram).map_or(0, |blocks| blocks.len()) as f64;
        ((document_count + 1.0) / (document_frequency + 1.0)).ln() + 1.0
    }
}

#[derive(Default)]
struct CandidateEvidence {
    sources: HashSet<CandidateSource>,
    shared_ngram_weight: f64,
}

impl CandidateGenerator for InvertedIndexCandidateGenerator {
    fn estimate_visits(&self, old: &BlockFeatures, limit: usize) -> Result<CandidateVisitEstimate> {
        validate_query_ngram_size(self.ngram_size, old)?;
        if limit == 0 {
            return Ok(CandidateVisitEstimate {
                total: 0,
                breakdown: Some(CandidateVisitBreakdown {
                    exact: 0,
                    ngram: 0,
                    short_fallback: 0,
                }),
            });
        }

        let exact = self.exact_index.get(&old.exact_hash).map_or(0, Vec::len);
        let mut ngram = 0_usize;
        for ngram_key in &old.ngrams {
            ngram = ngram.saturating_add(self.ngram_index.get(ngram_key).map_or(0, Vec::len));
        }
        let short_fallback = if is_short(old) {
            self.new_features.len()
        } else {
            0
        };
        Ok(CandidateVisitEstimate {
            total: exact.saturating_add(ngram).saturating_add(short_fallback),
            breakdown: Some(CandidateVisitBreakdown {
                exact,
                ngram,
                short_fallback,
            }),
        })
    }

    fn estimated_visits(&self, old: &BlockFeatures, limit: usize) -> Result<usize> {
        Ok(self.estimate_visits(old, limit)?.total)
    }

    fn candidates(&self, old: &BlockFeatures, limit: usize) -> Result<Vec<Candidate>> {
        validate_query_ngram_size(self.ngram_size, old)?;
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut evidence = HashMap::<BlockId, CandidateEvidence>::new();
        if let Some(blocks) = self.exact_index.get(&old.exact_hash) {
            for block in blocks {
                let Some(features) = self.new_features.get(block) else {
                    continue;
                };
                if features.canonical_tokens == old.canonical_tokens {
                    evidence
                        .entry(*block)
                        .or_default()
                        .sources
                        .insert(CandidateSource::Exact);
                }
            }
        }

        let mut old_ngrams = old.ngrams.iter().collect::<Vec<_>>();
        old_ngrams.sort_unstable();
        let total_old_weight = old_ngrams.iter().map(|ngram| self.idf(ngram)).sum::<f64>();
        for ngram in old_ngrams {
            let Some(blocks) = self.ngram_index.get(ngram) else {
                continue;
            };
            let weight = self.idf(ngram);
            for block in blocks {
                let candidate = evidence.entry(*block).or_default();
                candidate
                    .sources
                    .insert(CandidateSource::NGramInvertedIndex);
                candidate.shared_ngram_weight += weight;
            }
        }
        if is_short(old) {
            for features in self
                .new_features
                .values()
                .filter(|features| is_short(features))
            {
                evidence
                    .entry(features.block)
                    .or_default()
                    .sources
                    .insert(CandidateSource::ShortBlockFallback);
            }
        }

        let mut candidates = evidence
            .into_iter()
            .map(|(block, evidence)| {
                let exact = evidence.sources.contains(&CandidateSource::Exact);
                let coarse_score = if exact {
                    1.0
                } else if total_old_weight > 0.0 {
                    evidence.shared_ngram_weight / total_old_weight
                } else {
                    0.0
                };
                let mut sources = evidence.sources.into_iter().collect::<Vec<_>>();
                sources.sort_by_key(candidate_source_rank);
                Candidate {
                    block,
                    sources,
                    coarse_score,
                }
            })
            .collect::<Vec<_>>();
        candidates.sort_by(candidate_order);
        candidates.truncate(limit);
        Ok(candidates)
    }
}

pub struct ExhaustiveCandidateGenerator {
    new_features: Vec<BlockFeatures>,
    ngram_size: Option<usize>,
}

impl ExhaustiveCandidateGenerator {
    pub fn new(new: &[BlockFeatures]) -> Result<Self> {
        let ngram_size = common_ngram_size(new)?;
        let mut ids = HashSet::with_capacity(new.len());
        for features in new {
            if !ids.insert(features.block) {
                return Err(Error::Unresolved(format!(
                    "duplicate exhaustive candidate block id {}",
                    features.block.0
                )));
            }
        }
        Ok(Self {
            new_features: new.to_vec(),
            ngram_size,
        })
    }
}

impl CandidateGenerator for ExhaustiveCandidateGenerator {
    fn estimated_visits(&self, old: &BlockFeatures, limit: usize) -> Result<usize> {
        validate_query_ngram_size(self.ngram_size, old)?;
        Ok(if limit == 0 {
            0
        } else {
            self.new_features.len()
        })
    }

    fn candidates(&self, old: &BlockFeatures, limit: usize) -> Result<Vec<Candidate>> {
        validate_query_ngram_size(self.ngram_size, old)?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut candidates = self
            .new_features
            .iter()
            .map(|new| Candidate {
                block: new.block,
                sources: vec![CandidateSource::Exhaustive],
                coarse_score: dice_similarity(&old.ngrams, &new.ngrams),
            })
            .collect::<Vec<_>>();
        candidates.sort_by(candidate_order);
        candidates.truncate(limit);
        Ok(candidates)
    }
}

fn common_ngram_size(features: &[BlockFeatures]) -> Result<Option<usize>> {
    let Some(first) = features.first() else {
        return Ok(None);
    };
    if first.ngram_size == 0
        || features
            .iter()
            .any(|features| features.ngram_size != first.ngram_size)
    {
        return Err(Error::InvalidConfiguration(
            "candidate features must use one non-zero ngram_size".to_owned(),
        ));
    }
    Ok(Some(first.ngram_size))
}

fn validate_query_ngram_size(configured: Option<usize>, query: &BlockFeatures) -> Result<()> {
    if configured.is_some_and(|size| size != query.ngram_size) {
        return Err(Error::InvalidConfiguration(
            "query and candidate features must use the same ngram_size".to_owned(),
        ));
    }
    Ok(())
}

fn candidate_source_rank(source: &CandidateSource) -> u8 {
    match source {
        CandidateSource::Exact => 0,
        CandidateSource::NGramInvertedIndex => 1,
        CandidateSource::ShortBlockFallback => 2,
        CandidateSource::Exhaustive => 3,
    }
}

fn is_short(features: &BlockFeatures) -> bool {
    features.matching_tokens.len() <= features.ngram_size
}

fn candidate_order(left: &Candidate, right: &Candidate) -> std::cmp::Ordering {
    let left_exact = left.sources.contains(&CandidateSource::Exact);
    let right_exact = right.sources.contains(&CandidateSource::Exact);
    right_exact
        .cmp(&left_exact)
        .then(right.coarse_score.total_cmp(&left.coarse_score))
        .then(left.block.0.cmp(&right.block.0))
}
