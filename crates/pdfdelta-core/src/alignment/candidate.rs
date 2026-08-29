use std::collections::{HashMap, HashSet};

use crate::{
    Error, Result,
    layout::{BlockId, BlockRole},
};

use super::features::{BlockFeatures, ExactHash, NGram, NGramCounts, multiset_dice_similarity};

/// Fraction of the bounded candidate set reserved for n-gram candidates near
/// the strongest textual seed in new-document order.
const LOCAL_CANDIDATE_DIVISOR: usize = 4;

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

    /// Returns at most `limit` candidates in implementation-defined ranking order.
    ///
    /// Implementations must consider only blocks with an alignment-compatible role before
    /// ranking and applying `limit`. Ordered alignment filters incompatible blocks returned by
    /// custom generators, but cannot recover compatible blocks omitted by premature limiting.
    fn candidates(&self, old: &BlockFeatures, limit: usize) -> Result<Vec<Candidate>>;
}

pub struct InvertedIndexCandidateGenerator {
    new_features: HashMap<BlockId, BlockFeatures>,
    exact_index: HashMap<BlockRole, HashMap<ExactHash, Vec<BlockId>>>,
    ngram_index: HashMap<BlockRole, HashMap<NGram, Vec<BlockId>>>,
    role_document_counts: HashMap<BlockRole, usize>,
    weighted_ngram_totals: HashMap<BlockId, f64>,
    new_order: HashMap<BlockId, usize>,
    /// All new-side short blocks in `BlockId` order, retained for queries that
    /// have no page evidence and therefore cannot use structural locality.
    short_blocks: Vec<BlockId>,
    /// New-side short blocks with page evidence in relative page order.
    positioned_short_blocks: Vec<(u16, BlockId)>,
    ngram_size: Option<usize>,
}

impl InvertedIndexCandidateGenerator {
    pub fn new(new: &[BlockFeatures]) -> Result<Self> {
        let ngram_size = common_ngram_size(new)?;
        let mut new_features = HashMap::with_capacity(new.len());
        let mut exact_index = HashMap::<BlockRole, HashMap<ExactHash, Vec<BlockId>>>::new();
        let mut ngram_index = HashMap::<BlockRole, HashMap<NGram, Vec<BlockId>>>::new();
        let mut role_document_counts = HashMap::<BlockRole, usize>::new();
        let mut new_order = HashMap::with_capacity(new.len());
        let mut short_blocks = Vec::new();
        let mut positioned_short_blocks = Vec::new();

        for (index, features) in new.iter().enumerate() {
            if new_features
                .insert(features.block, features.clone())
                .is_some()
            {
                return Err(Error::Unresolved(format!(
                    "duplicate candidate block id {}",
                    features.block.0
                )));
            }
            new_order.insert(features.block, index);
            *role_document_counts.entry(features.role).or_default() += 1;
            exact_index
                .entry(features.role)
                .or_default()
                .entry(features.exact_hash)
                .or_default()
                .push(features.block);
            ngram_index.entry(features.role).or_default();
            for ngram in features.ngram_counts.keys() {
                ngram_index
                    .entry(features.role)
                    .or_default()
                    .entry(ngram.clone())
                    .or_default()
                    .push(features.block);
            }
            if is_short(features) {
                short_blocks.push(features.block);
                if let Some(position) = features.page_position {
                    positioned_short_blocks.push((position, features.block));
                }
            }
        }

        for blocks in exact_index.values_mut().flat_map(HashMap::values_mut) {
            blocks.sort_by_key(|block| block.0);
        }
        for blocks in ngram_index.values_mut().flat_map(HashMap::values_mut) {
            blocks.sort_by_key(|block| block.0);
        }
        short_blocks.sort_by_key(|block| block.0);
        positioned_short_blocks.sort_unstable();
        let mut weighted_ngram_totals = HashMap::new();
        weighted_ngram_totals.try_reserve(new.len()).map_err(|_| {
            Error::Unresolved("candidate weighted n-gram totals allocation failed".to_owned())
        })?;
        for features in new {
            let document_count = role_document_counts
                .get(&features.role)
                .copied()
                .ok_or_else(|| {
                    Error::Unresolved("candidate role document count missing".to_owned())
                })?;
            let role_ngram_index = ngram_index.get(&features.role).ok_or_else(|| {
                Error::Unresolved("candidate role n-gram index missing".to_owned())
            })?;
            let mut ngrams = Vec::new();
            ngrams
                .try_reserve_exact(features.ngram_counts.len())
                .map_err(|_| {
                    Error::Unresolved("candidate n-gram ordering allocation failed".to_owned())
                })?;
            ngrams.extend(features.ngram_counts.keys());
            ngrams.sort_unstable();
            let total = weighted_ngram_total(&features.ngram_counts, &ngrams, |ngram| {
                idf(document_count, role_ngram_index, ngram)
            });
            weighted_ngram_totals.insert(features.block, total);
        }

        Ok(Self {
            new_features,
            exact_index,
            ngram_index,
            role_document_counts,
            weighted_ngram_totals,
            new_order,
            short_blocks,
            positioned_short_blocks,
            ngram_size,
        })
    }

    fn idf(&self, role: BlockRole, ngram: &NGram) -> f64 {
        let document_count = self.role_document_counts.get(&role).copied().unwrap_or(0);
        let Some(index) = self.ngram_index.get(&role) else {
            return 1.0;
        };
        idf(document_count, index, ngram)
    }

    fn weighted_ngram_scores(
        &self,
        block: BlockId,
        old_weight: f64,
        shared_weight: f64,
    ) -> (f64, f64) {
        let Some(new_weight) = self.weighted_ngram_totals.get(&block).copied() else {
            return (0.0, 0.0);
        };
        let total = old_weight + new_weight;
        let dice = if total == 0.0 {
            1.0
        } else {
            finite_unit_score(2.0 * shared_weight / total)
        };
        let shorter = old_weight.min(new_weight);
        let containment = if shorter == 0.0 {
            f64::from(old_weight == 0.0 && new_weight == 0.0)
        } else {
            finite_unit_score(shared_weight / shorter)
        };
        (dice, containment)
    }

    fn short_fallback_visit_count(&self, old: &BlockFeatures, limit: usize) -> Result<usize> {
        match old.page_position {
            Some(position) => self.scan_positioned_short_blocks(old, position, limit, |_| {}),
            None => Ok(self.short_blocks.len()),
        }
    }

    fn short_fallback_blocks(&self, old: &BlockFeatures, limit: usize) -> Result<Vec<BlockId>> {
        let Some(position) = old.page_position else {
            let mut selected = Vec::new();
            selected
                .try_reserve_exact(self.short_blocks.len())
                .map_err(|_| {
                    Error::Unresolved("short fallback candidates allocation failed".to_owned())
                })?;
            selected.extend(
                self.short_blocks
                    .iter()
                    .copied()
                    .filter(|block| self.has_compatible_role(old, *block)),
            );
            return Ok(selected);
        };
        let count = self.positioned_short_blocks.len().min(limit);
        let mut selected = Vec::new();
        selected.try_reserve_exact(count).map_err(|_| {
            Error::Unresolved("short structural candidates allocation failed".to_owned())
        })?;
        self.scan_positioned_short_blocks(old, position, limit, |block| selected.push(block))?;
        Ok(selected)
    }

    fn scan_positioned_short_blocks(
        &self,
        old: &BlockFeatures,
        position: u16,
        limit: usize,
        mut select: impl FnMut(BlockId),
    ) -> Result<usize> {
        let target = self.positioned_short_blocks.len().min(limit);
        let mut selected = 0;
        let mut visits = 0;
        let mut right = self
            .positioned_short_blocks
            .partition_point(|(candidate, _)| *candidate < position);
        let mut left = right.checked_sub(1);
        while selected < target {
            let left_candidate = left.and_then(|index| self.positioned_short_blocks.get(index));
            let right_candidate = self.positioned_short_blocks.get(right);
            let take_left = match (left_candidate, right_candidate) {
                (Some(left), Some(right)) => {
                    (left.0.abs_diff(position), left.1) <= (right.0.abs_diff(position), right.1)
                }
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            let block = if take_left {
                let (_, block) = left_candidate
                    .ok_or_else(|| Error::Unresolved("short left candidate missing".to_owned()))?;
                left = left.and_then(|index| index.checked_sub(1));
                *block
            } else {
                let (_, block) = right_candidate
                    .ok_or_else(|| Error::Unresolved("short right candidate missing".to_owned()))?;
                right = right.checked_add(1).ok_or_else(|| {
                    Error::Unresolved("short candidate index overflowed".to_owned())
                })?;
                *block
            };
            visits += 1;
            if self.has_compatible_role(old, block) {
                selected += 1;
                select(block);
            }
        }
        Ok(visits)
    }

    fn has_compatible_role(&self, old: &BlockFeatures, block: BlockId) -> bool {
        self.new_features
            .get(&block)
            .is_some_and(|new| old.role.is_alignment_compatible(new.role))
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

        let exact = self
            .exact_index
            .get(&old.role)
            .and_then(|index| index.get(&old.exact_hash))
            .map_or(0, Vec::len);
        let mut ngram = 0_usize;
        let role_ngram_index = self.ngram_index.get(&old.role);
        for ngram_key in old.ngram_counts.keys() {
            ngram = ngram.saturating_add(
                role_ngram_index
                    .and_then(|index| index.get(ngram_key))
                    .map_or(0, Vec::len),
            );
        }
        let short_fallback = if is_short(old) {
            self.short_fallback_visit_count(old, limit)?
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
        if let Some(blocks) = self
            .exact_index
            .get(&old.role)
            .and_then(|index| index.get(&old.exact_hash))
        {
            for block in blocks {
                let Some(features) = self.new_features.get(block) else {
                    continue;
                };
                if old.role.is_alignment_compatible(features.role)
                    && features.canonical_tokens == old.canonical_tokens
                {
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
        let old_weight = weighted_ngram_total(&old.ngram_counts, &old_ngrams, |ngram| {
            self.idf(old.role, ngram)
        });
        let role_ngram_index = self.ngram_index.get(&old.role);
        for ngram in &old_ngrams {
            let Some(blocks) = role_ngram_index.and_then(|index| index.get(*ngram)) else {
                continue;
            };
            let old_count = old.ngram_counts.get(*ngram).copied().unwrap_or(0);
            let weight = self.idf(old.role, ngram);
            for block in blocks {
                let Some(features) = self.new_features.get(block) else {
                    continue;
                };
                if !old.role.is_alignment_compatible(features.role) {
                    continue;
                }
                let candidate = evidence.entry(*block).or_default();
                candidate
                    .sources
                    .insert(CandidateSource::NGramInvertedIndex);
                let new_count = features.ngram_counts.get(*ngram).copied().unwrap_or(0);
                candidate.shared_ngram_weight += old_count.min(new_count) as f64 * weight;
            }
        }
        if is_short(old) {
            for block in self.short_fallback_blocks(old, limit)? {
                evidence
                    .entry(block)
                    .or_default()
                    .sources
                    .insert(CandidateSource::ShortBlockFallback);
            }
        }

        let mut candidates = Vec::with_capacity(evidence.len());
        for (block, evidence) in evidence {
            let Some(document_index) = self.new_order.get(&block).copied() else {
                return Err(Error::Unresolved(format!(
                    "candidate block {} is missing from the new-document order",
                    block.0
                )));
            };
            let exact = evidence.sources.contains(&CandidateSource::Exact);
            let (coarse_score, containment_score) = if exact {
                (1.0, 1.0)
            } else {
                self.weighted_ngram_scores(block, old_weight, evidence.shared_ngram_weight)
            };
            let mut sources = evidence.sources.into_iter().collect::<Vec<_>>();
            sources.sort_by_key(candidate_source_rank);
            candidates.push(RankedCandidate {
                candidate: Candidate {
                    block,
                    sources,
                    coarse_score,
                },
                containment_score,
                document_index,
            });
        }
        Ok(select_candidate_union(candidates, limit))
    }
}

struct RankedCandidate {
    candidate: Candidate,
    containment_score: f64,
    document_index: usize,
}

fn select_candidate_union(mut candidates: Vec<RankedCandidate>, limit: usize) -> Vec<Candidate> {
    candidates.sort_by(|left, right| candidate_order(&left.candidate, &right.candidate));
    if candidates.len() <= limit {
        return candidates
            .into_iter()
            .map(|ranked| ranked.candidate)
            .collect();
    }
    let local_limit = limit / LOCAL_CANDIDATE_DIVISOR;
    if local_limit == 0 {
        return candidates
            .into_iter()
            .take(limit)
            .map(|ranked| ranked.candidate)
            .collect();
    }

    let exact_count = candidates
        .iter()
        .take_while(|ranked| ranked.candidate.sources.contains(&CandidateSource::Exact))
        .count();
    let primary_limit = (limit - local_limit).max(exact_count.min(limit));
    let mut selected = HashSet::with_capacity(limit);
    let mut indices = Vec::with_capacity(limit);
    for (index, ranked) in candidates.iter().enumerate().take(primary_limit) {
        selected.insert(ranked.candidate.block);
        indices.push(index);
    }

    let seed = candidates[0].document_index;
    let mut local_order = (0..candidates.len()).collect::<Vec<_>>();
    local_order.sort_unstable_by(|left, right| {
        candidates[*left]
            .document_index
            .abs_diff(seed)
            .cmp(&candidates[*right].document_index.abs_diff(seed))
            .then(
                candidates[*right]
                    .containment_score
                    .total_cmp(&candidates[*left].containment_score),
            )
            .then(candidate_order(
                &candidates[*left].candidate,
                &candidates[*right].candidate,
            ))
    });
    for index in local_order {
        if selected.insert(candidates[index].candidate.block) {
            indices.push(index);
            if indices.len() == limit {
                break;
            }
        }
    }
    if indices.len() < limit {
        for (index, ranked) in candidates.iter().enumerate().skip(primary_limit) {
            if selected.insert(ranked.candidate.block) {
                indices.push(index);
                if indices.len() == limit {
                    break;
                }
            }
        }
    }
    indices.sort_unstable();
    let mut selected = indices.into_iter().peekable();
    candidates
        .into_iter()
        .enumerate()
        .filter_map(|(index, ranked)| {
            (selected.next_if_eq(&index).is_some()).then_some(ranked.candidate)
        })
        .collect()
}

fn finite_unit_score(score: f64) -> f64 {
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
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
            .filter(|new| old.role.is_alignment_compatible(new.role))
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
                if old.role.is_alignment_compatible(features.role)
                    && features.canonical_tokens == old.canonical_tokens
                {
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
                    let Some(features) = self.new_features.get(block) else {
                        continue;
                    };
                    if !old.role.is_alignment_compatible(features.role) {
                        continue;
                    }
                    evidence
                        .entry(*block)
                        .or_default()
                        .insert(CandidateSource::MinHashLsh);
                }
            }
        }

        if is_short(old) {
            for block in &self.short_blocks {
                let Some(features) = self.new_features.get(block) else {
                    continue;
                };
                if !old.role.is_alignment_compatible(features.role) {
                    continue;
                }
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
            role: crate::layout::BlockRole::Body,
            exact_hash: ExactHash(block),
            canonical_tokens: Vec::new(),
            matching_tokens: Vec::new(),
            ngram_counts,
            ngram_size: 1,
            page_position: None,
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

    #[test]
    fn candidate_union_reserves_bounded_local_split_merge_slots() {
        let mut candidates = (0..36)
            .map(|index| RankedCandidate {
                candidate: Candidate {
                    block: BlockId(index + 1),
                    sources: vec![CandidateSource::NGramInvertedIndex],
                    coarse_score: 1.0 - index as f64 / 100.0,
                },
                containment_score: 0.5,
                document_index: 200 + index as usize,
            })
            .collect::<Vec<_>>();
        candidates[0].document_index = 100;
        candidates[35].containment_score = 1.0;
        candidates[35].document_index = 103;

        let selected = select_candidate_union(candidates, 32);

        assert_eq!(selected.len(), 32);
        assert!(
            selected
                .iter()
                .any(|candidate| candidate.block == BlockId(36))
        );
        assert!(
            selected
                .iter()
                .any(|candidate| candidate.block == BlockId(28))
        );
        assert!(
            selected
                .iter()
                .all(|candidate| candidate.block != BlockId(32))
        );
    }

    #[test]
    fn small_candidate_limits_keep_the_primary_ranking() {
        let candidates = (0..4)
            .map(|index| RankedCandidate {
                candidate: Candidate {
                    block: BlockId(index + 1),
                    sources: vec![CandidateSource::NGramInvertedIndex],
                    coarse_score: 1.0 - index as f64 / 10.0,
                },
                containment_score: if index == 3 { 1.0 } else { 0.0 },
                document_index: index as usize,
            })
            .collect::<Vec<_>>();

        let selected = select_candidate_union(candidates, 2);

        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.block)
                .collect::<Vec<_>>(),
            [BlockId(1), BlockId(2)]
        );
    }

    #[test]
    fn short_fallback_unions_near_page_candidates_with_text_candidates() {
        let mut old = feature(99, &[('q', 1)]);
        old.page_position = Some(5_000);
        let mut far_text = feature(1, &[('q', 1)]);
        far_text.page_position = Some(0);
        let mut near_left = feature(9, &[('a', 1)]);
        near_left.page_position = Some(4_000);
        let mut near_right = feature(10, &[('b', 1)]);
        near_right.page_position = Some(6_000);
        let mut far_other = feature(2, &[('c', 1)]);
        far_other.page_position = Some(10_000);
        let generator =
            InvertedIndexCandidateGenerator::new(&[far_text, near_left, near_right, far_other])
                .expect("positioned short features build an index");

        let estimate = generator
            .estimate_visits(&old, 2)
            .expect("visit estimate succeeds");
        let candidates = generator
            .candidates(&old, 2)
            .expect("candidate union succeeds");

        assert_eq!(
            estimate.breakdown,
            Some(CandidateVisitBreakdown {
                exact: 0,
                ngram: 1,
                short_fallback: 2,
            })
        );
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.block)
                .collect::<Vec<_>>(),
            [BlockId(1), BlockId(9)]
        );
        assert!(
            candidates[0]
                .sources
                .contains(&CandidateSource::NGramInvertedIndex)
        );
        assert!(
            candidates[1]
                .sources
                .contains(&CandidateSource::ShortBlockFallback)
        );
    }

    #[test]
    fn short_structural_fallback_excludes_candidates_without_page_evidence() {
        let mut positioned_old = feature(99, &[('q', 1)]);
        positioned_old.page_position = Some(5_000);
        let unpositioned = feature(1, &[('a', 1)]);
        let mut positioned = feature(2, &[('b', 1)]);
        positioned.page_position = Some(6_000);
        let generator = InvertedIndexCandidateGenerator::new(&[unpositioned, positioned])
            .expect("short features build an index");

        let positioned_estimate = generator
            .estimate_visits(&positioned_old, 2)
            .expect("positioned visit estimate succeeds");
        let positioned_candidates = generator
            .candidates(&positioned_old, 2)
            .expect("positioned candidate search succeeds");
        let unpositioned_old = feature(100, &[('q', 1)]);
        let unpositioned_estimate = generator
            .estimate_visits(&unpositioned_old, 2)
            .expect("unpositioned visit estimate succeeds");

        assert_eq!(
            positioned_estimate.breakdown,
            Some(CandidateVisitBreakdown {
                exact: 0,
                ngram: 0,
                short_fallback: 1,
            })
        );
        assert_eq!(positioned_candidates[0].block, BlockId(2));
        assert_eq!(
            unpositioned_estimate.breakdown,
            Some(CandidateVisitBreakdown {
                exact: 0,
                ngram: 0,
                short_fallback: 2,
            })
        );
    }
}
