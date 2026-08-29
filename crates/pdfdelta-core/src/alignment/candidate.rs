use std::collections::{HashMap, HashSet};

use crate::{Error, Result, layout::BlockId};

use super::features::{BlockFeatures, ExactHash, NGram, NGramCounts, multiset_dice_similarity};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CandidateSource {
    Exact,
    NGramInvertedIndex,
    MinHashLsh,
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
    weighted_ngram_totals: HashMap<BlockId, f64>,
    /// New-side short blocks in `BlockId` order, backing the short-block
    /// fallback so it visits only short blocks instead of every new block.
    short_blocks: Vec<BlockId>,
    ngram_size: Option<usize>,
}

impl InvertedIndexCandidateGenerator {
    pub fn new(new: &[BlockFeatures]) -> Result<Self> {
        let ngram_size = common_ngram_size(new)?;
        let mut new_features = HashMap::with_capacity(new.len());
        let mut exact_index = HashMap::<ExactHash, Vec<BlockId>>::new();
        let mut ngram_index = HashMap::<NGram, Vec<BlockId>>::new();
        let mut short_blocks = Vec::new();

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
            for ngram in features.ngram_counts.keys() {
                ngram_index
                    .entry(ngram.clone())
                    .or_default()
                    .push(features.block);
            }
            if is_short(features) {
                short_blocks.push(features.block);
            }
        }

        for blocks in exact_index.values_mut() {
            blocks.sort_by_key(|block| block.0);
        }
        for blocks in ngram_index.values_mut() {
            blocks.sort_by_key(|block| block.0);
        }
        short_blocks.sort_by_key(|block| block.0);
        let mut weighted_ngram_totals = HashMap::new();
        weighted_ngram_totals.try_reserve(new.len()).map_err(|_| {
            Error::Unresolved("candidate weighted n-gram totals allocation failed".to_owned())
        })?;
        for features in new {
            let mut ngrams = Vec::new();
            ngrams
                .try_reserve_exact(features.ngram_counts.len())
                .map_err(|_| {
                    Error::Unresolved("candidate n-gram ordering allocation failed".to_owned())
                })?;
            ngrams.extend(features.ngram_counts.keys());
            ngrams.sort_unstable();
            let total = weighted_ngram_total(&features.ngram_counts, &ngrams, |ngram| {
                idf(new.len(), &ngram_index, ngram)
            });
            weighted_ngram_totals.insert(features.block, total);
        }

        Ok(Self {
            new_features,
            exact_index,
            ngram_index,
            weighted_ngram_totals,
            short_blocks,
            ngram_size,
        })
    }

    fn idf(&self, ngram: &NGram) -> f64 {
        idf(self.new_features.len(), &self.ngram_index, ngram)
    }

    fn weighted_ngram_similarity(
        &self,
        block: BlockId,
        old_weight: f64,
        shared_weight: f64,
    ) -> f64 {
        let Some(new_weight) = self.weighted_ngram_totals.get(&block).copied() else {
            return 0.0;
        };
        let total = old_weight + new_weight;
        if total == 0.0 {
            return 1.0;
        }
        let score = 2.0 * shared_weight / total;
        if score.is_finite() {
            score.clamp(0.0, 1.0)
        } else {
            0.0
        }
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
        for ngram_key in old.ngram_counts.keys() {
            ngram = ngram.saturating_add(self.ngram_index.get(ngram_key).map_or(0, Vec::len));
        }
        let short_fallback = if is_short(old) {
            self.short_blocks.len()
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

        let mut old_ngrams = Vec::new();
        old_ngrams
            .try_reserve_exact(old.ngram_counts.len())
            .map_err(|_| Error::Unresolved("query n-gram ordering allocation failed".to_owned()))?;
        old_ngrams.extend(old.ngram_counts.keys());
        old_ngrams.sort_unstable();
        let old_weight =
            weighted_ngram_total(&old.ngram_counts, &old_ngrams, |ngram| self.idf(ngram));
        for ngram in &old_ngrams {
            let Some(blocks) = self.ngram_index.get(ngram) else {
                continue;
            };
            let old_count = old.ngram_counts.get(*ngram).copied().unwrap_or(0);
            let weight = self.idf(ngram);
            for block in blocks {
                let candidate = evidence.entry(*block).or_default();
                candidate
                    .sources
                    .insert(CandidateSource::NGramInvertedIndex);
                let new_count = self
                    .new_features
                    .get(block)
                    .and_then(|features| features.ngram_counts.get(*ngram))
                    .copied()
                    .unwrap_or(0);
                candidate.shared_ngram_weight += old_count.min(new_count) as f64 * weight;
            }
        }
        if is_short(old) {
            for block in &self.short_blocks {
                evidence
                    .entry(*block)
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
                } else {
                    self.weighted_ngram_similarity(block, old_weight, evidence.shared_ngram_weight)
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
                coarse_score: multiset_dice_similarity(&old.ngram_counts, &new.ngram_counts),
            })
            .collect::<Vec<_>>();
        candidates.sort_by(candidate_order);
        candidates.truncate(limit);
        Ok(candidates)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MinHashLshOptions {
    pub num_hashes: usize,
    pub num_bands: usize,
}

impl Default for MinHashLshOptions {
    fn default() -> Self {
        Self {
            num_hashes: 64,
            num_bands: 16,
        }
    }
}

pub struct MinHashLshCandidateGenerator {
    new_features: HashMap<BlockId, BlockFeatures>,
    exact_index: HashMap<ExactHash, Vec<BlockId>>,
    buckets: HashMap<(u32, u64), Vec<BlockId>>,
    short_blocks: Vec<BlockId>,
    ngram_size: Option<usize>,
    options: MinHashLshOptions,
}

impl MinHashLshCandidateGenerator {
    pub fn new(new: &[BlockFeatures]) -> Result<Self> {
        Self::with_options(new, MinHashLshOptions::default())
    }

    pub fn with_options(new: &[BlockFeatures], options: MinHashLshOptions) -> Result<Self> {
        if options.num_hashes == 0 || options.num_bands == 0 {
            return Err(Error::InvalidConfiguration(
                "MinHash LSH requires non-zero num_hashes and num_bands".to_owned(),
            ));
        }
        if !options.num_hashes.is_multiple_of(options.num_bands) {
            return Err(Error::InvalidConfiguration(
                "MinHash LSH num_hashes must be divisible by num_bands".to_owned(),
            ));
        }

        let ngram_size = common_ngram_size(new)?;
        let rows_per_band = options.num_hashes / options.num_bands;
        let mut new_features = HashMap::with_capacity(new.len());
        let mut exact_index = HashMap::<ExactHash, Vec<BlockId>>::new();
        let mut buckets = HashMap::<(u32, u64), Vec<BlockId>>::new();
        let mut short_blocks = Vec::new();

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

            if is_short(features) {
                short_blocks.push(features.block);
            }

            let signature = compute_minhash_signature(&features.ngram_counts, options.num_hashes);
            for band in 0..options.num_bands {
                let start = band * rows_per_band;
                let end = start + rows_per_band;
                let band_hash = hash_u64_slice(&signature[start..end]);
                buckets
                    .entry((band as u32, band_hash))
                    .or_default()
                    .push(features.block);
            }
        }

        for blocks in exact_index.values_mut() {
            blocks.sort_by_key(|block| block.0);
        }
        for blocks in buckets.values_mut() {
            blocks.sort_by_key(|block| block.0);
        }
        short_blocks.sort_by_key(|block| block.0);

        Ok(Self {
            new_features,
            exact_index,
            buckets,
            short_blocks,
            ngram_size,
            options,
        })
    }
}

impl CandidateGenerator for MinHashLshCandidateGenerator {
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
        let rows_per_band = self.options.num_hashes / self.options.num_bands;
        let signature = compute_minhash_signature(&old.ngram_counts, self.options.num_hashes);
        let mut lsh = 0_usize;
        for band in 0..self.options.num_bands {
            let start = band * rows_per_band;
            let end = start + rows_per_band;
            let band_hash = hash_u64_slice(&signature[start..end]);
            lsh = lsh.saturating_add(
                self.buckets
                    .get(&(band as u32, band_hash))
                    .map_or(0, Vec::len),
            );
        }

        let short_fallback = if is_short(old) {
            self.short_blocks.len()
        } else {
            0
        };

        Ok(CandidateVisitEstimate {
            total: exact.saturating_add(lsh).saturating_add(short_fallback),
            breakdown: Some(CandidateVisitBreakdown {
                exact,
                ngram: lsh,
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

        let mut evidence = HashMap::<BlockId, HashSet<CandidateSource>>::new();
        if let Some(blocks) = self.exact_index.get(&old.exact_hash) {
            for block in blocks {
                let Some(features) = self.new_features.get(block) else {
                    continue;
                };
                if features.canonical_tokens == old.canonical_tokens {
                    evidence
                        .entry(*block)
                        .or_default()
                        .insert(CandidateSource::Exact);
                }
            }
        }

        let rows_per_band = self.options.num_hashes / self.options.num_bands;
        let signature = compute_minhash_signature(&old.ngram_counts, self.options.num_hashes);
        for band in 0..self.options.num_bands {
            let start = band * rows_per_band;
            let end = start + rows_per_band;
            let band_hash = hash_u64_slice(&signature[start..end]);
            if let Some(blocks) = self.buckets.get(&(band as u32, band_hash)) {
                for block in blocks {
                    evidence
                        .entry(*block)
                        .or_default()
                        .insert(CandidateSource::MinHashLsh);
                }
            }
        }

        if is_short(old) {
            for block in &self.short_blocks {
                evidence
                    .entry(*block)
                    .or_default()
                    .insert(CandidateSource::ShortBlockFallback);
            }
        }

        let mut candidates = evidence
            .into_iter()
            .map(|(block, sources_set)| {
                let exact = sources_set.contains(&CandidateSource::Exact);
                let coarse_score = if exact {
                    1.0
                } else if let Some(features) = self.new_features.get(&block) {
                    multiset_dice_similarity(&old.ngram_counts, &features.ngram_counts)
                } else {
                    0.0
                };
                let mut sources = sources_set.into_iter().collect::<Vec<_>>();
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

fn compute_minhash_signature(ngrams: &NGramCounts, num_hashes: usize) -> Vec<u64> {
    if ngrams.is_empty() {
        return vec![0; num_hashes];
    }
    let ngram_hashes: Vec<u64> = ngrams.keys().map(hash_ngram_value).collect();
    let mut signature = vec![u64::MAX; num_hashes];
    for &h in &ngram_hashes {
        for (i, slot) in signature.iter_mut().enumerate() {
            let val = mix64(h ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15));
            if val < *slot {
                *slot = val;
            }
        }
    }
    signature
}

fn weighted_ngram_total(
    counts: &NGramCounts,
    ordered_ngrams: &[&NGram],
    weight: impl Fn(&NGram) -> f64,
) -> f64 {
    ordered_ngrams
        .iter()
        .map(|ngram| counts.get(*ngram).copied().unwrap_or(0) as f64 * weight(ngram))
        .sum()
}

fn idf(document_count: usize, index: &HashMap<NGram, Vec<BlockId>>, ngram: &NGram) -> f64 {
    let document_count = document_count as f64;
    let document_frequency = index.get(ngram).map_or(0, Vec::len) as f64;
    ((document_count + 1.0) / (document_frequency + 1.0)).ln() + 1.0
}

fn hash_ngram_value(ngram: &NGram) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ngram.hash(&mut hasher);
    hasher.finish()
}

fn hash_u64_slice(slice: &[u64]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    slice.hash(&mut hasher);
    hasher.finish()
}

fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

fn common_ngram_size(features: &[BlockFeatures]) -> Result<Option<usize>> {
    let Some(first) = features.first() else {
        return Ok(None);
    };
    if first.ngram_size == 0
        || features.iter().any(|features| {
            features.ngram_size != first.ngram_size || !valid_ngram_features(features)
        })
    {
        return Err(Error::InvalidConfiguration(
            "candidate features must use one non-zero ngram_size".to_owned(),
        ));
    }
    Ok(Some(first.ngram_size))
}

fn validate_query_ngram_size(configured: Option<usize>, query: &BlockFeatures) -> Result<()> {
    if configured.is_some_and(|size| size != query.ngram_size) || !valid_ngram_features(query) {
        return Err(Error::InvalidConfiguration(
            "query and candidate features must use the same ngram_size".to_owned(),
        ));
    }
    Ok(())
}

fn valid_ngram_features(features: &BlockFeatures) -> bool {
    features.ngram_counts.values().all(|count| *count > 0)
}

fn candidate_source_rank(source: &CandidateSource) -> u8 {
    match source {
        CandidateSource::Exact => 0,
        CandidateSource::NGramInvertedIndex => 1,
        CandidateSource::MinHashLsh => 2,
        CandidateSource::ShortBlockFallback => 3,
        CandidateSource::Exhaustive => 4,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::ComparableToken;

    fn feature(block: u64, counts: &[(char, usize)]) -> BlockFeatures {
        let ngram_counts = counts
            .iter()
            .map(|(scalar, count)| (NGram(vec![ComparableToken::Scalar(*scalar)]), *count))
            .collect::<NGramCounts>();
        BlockFeatures {
            block: BlockId(block),
            exact_hash: ExactHash(block),
            canonical_tokens: Vec::new(),
            matching_tokens: Vec::new(),
            ngram_counts,
            ngram_size: 1,
            numeric_mask_applied: false,
            has_normalization_issues: false,
        }
    }

    #[test]
    fn inverted_ranking_uses_ngram_multiplicity() {
        let old = feature(10, &[('a', 1), ('b', 1)]);
        let inflated = feature(1, &[('a', 8), ('b', 1)]);
        let balanced = feature(2, &[('a', 1), ('b', 1)]);
        let generator = InvertedIndexCandidateGenerator::new(&[inflated, balanced])
            .expect("consistent n-gram features build an index");

        let candidates = generator
            .candidates(&old, 2)
            .expect("candidate scoring succeeds");

        assert_eq!(candidates[0].block, BlockId(2));
        assert_eq!(candidates[0].coarse_score, 1.0);
        assert!(candidates[0].coarse_score > candidates[1].coarse_score);
    }
}
