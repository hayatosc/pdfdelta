//! Candidate generation evaluation (SPEC §12.7).
//!
//! Measures top-K recall and per-old-block candidate counts of the
//! inverted-index candidate generator against the exhaustive all-pairs
//! oracle on synthetic fixtures. True counterparts are defined by canonical
//! paragraph id plus global span overlap, so moved, re-wrapped, and
//! re-paginated paragraphs still resolve to their correct new blocks.
//!
//! Latency and memory measurement are deferred to a later slice; this module
//! covers recall, candidate counts, and estimated visit budgets.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pdfdelta_core::{
    alignment::{
        AlignmentOptions, BlockFeatures, CandidateGenerator, ExhaustiveCandidateGenerator,
        InvertedIndexCandidateGenerator, NGram, build_block_features,
        estimate_ngram_token_elements,
    },
    layout::{BlockId, reconstruct_blocks, reconstruct_lines},
    normalize::{BlockText, normalize_blocks},
    pdf::{LopdfParser, ParseLimits},
    pipeline::PipelineOptions,
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};

use crate::{
    BenchError, Result,
    cases::BenchmarkCase,
    mutation::{CanonicalParagraphSpan, RenderPlan},
    renderers::{RenderLimits, RendererKind},
};

/// Matches the pipeline default (§12.6) so candidate features agree with
/// production alignment.
const NGRAM_SIZE: usize = 3;

/// Candidate generation metrics for one synthetic fixture.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateEvalRecord {
    pub case_name: String,
    pub old_blocks: usize,
    pub new_blocks: usize,
    /// Old blocks that have at least one true counterpart in the new side;
    /// this is the recall denominator.
    pub counterpart_old_blocks: usize,
    /// Old blocks without any true counterpart (e.g. deleted paragraphs).
    pub unmatched_old_blocks: usize,
    /// Inverted-index recall@K, one entry per K in `top_k`.
    pub recall_at_k: Vec<f64>,
    /// Exhaustive-oracle recall@K, one entry per K in `top_k`.
    pub oracle_recall_at_k: Vec<f64>,
    /// Inverted-index candidate counts per old block (nearest-rank p50).
    pub candidate_count_p50: usize,
    /// Inverted-index candidate counts per old block (nearest-rank p95).
    pub candidate_count_p95: usize,
    pub candidate_count_max: usize,
    /// Exhaustive-oracle candidate counts per old block (nearest-rank p50).
    pub oracle_candidate_count_p50: usize,
    /// Exhaustive-oracle candidate counts per old block (nearest-rank p95).
    pub oracle_candidate_count_p95: usize,
    pub oracle_candidate_count_max: usize,
    /// Inverted-index estimated visits per old block (nearest-rank p50).
    pub estimated_visits_p50: usize,
    /// Inverted-index estimated visits per old block (nearest-rank p95).
    pub estimated_visits_p95: usize,
    pub estimated_visits_max: usize,
    /// Sum of estimated visits across all old blocks; an upper bound on the
    /// production charge, which excludes main exact anchors.
    pub estimated_visits_upper_bound_total: usize,
    /// Total visit budget (`AlignmentOptions::max_candidate_visits`) that
    /// production alignment charges against.
    pub max_candidate_visits: usize,
    /// Whether `estimated_visits_upper_bound_total` exceeds
    /// `max_candidate_visits`. Because production excludes main exact
    /// anchors, an upper-bound exceedance is a necessary condition and
    /// investigation signal, not a sufficient condition for a production
    /// LIMIT failure (false positives possible, false negatives not).
    pub estimated_visits_upper_bound_exceeds_limit: bool,
    /// Total n-gram posting visits across all old blocks, excluding exact
    /// matches and the short-block fallback.
    pub ngram_posting_visits_total: usize,
    /// Aggregate visits of the single most-visited n-gram (new-side document
    /// frequency times old-side occurrence count).
    pub dominant_ngram_visits: usize,
    /// New-side document frequency of the dominant n-gram.
    pub dominant_ngram_df: usize,
}

/// Runs candidate generation evaluation for one fixture.
///
/// `top_k` selects the K values whose recall is reported; candidate counts
/// are always measured over the full (untruncated) candidate set.
pub fn evaluate_candidate_generation(
    case: &BenchmarkCase,
    renderer: RendererKind,
    top_k: &[usize],
) -> Result<CandidateEvalRecord> {
    let plan = case.plan();
    let old_blocks = normalized_blocks(plan.old(), renderer)?;
    let new_blocks = normalized_blocks(plan.new_plan(), renderer)?;
    enforce_ngram_budget(&old_blocks, &new_blocks)?;
    let counterparts = true_counterparts(
        &old_blocks,
        &new_blocks,
        plan.old_paragraphs(),
        plan.new_paragraphs(),
        &canonical_source(plan.old()),
        &canonical_source(plan.new_plan()),
    )?;

    let old_features = build_block_features(&old_blocks, NGRAM_SIZE)
        .map_err(|error| core_error("candidate feature build", error))?;
    let new_features = build_block_features(&new_blocks, NGRAM_SIZE)
        .map_err(|error| core_error("candidate feature build", error))?;
    let inverted = InvertedIndexCandidateGenerator::new(&new_features)
        .map_err(|error| core_error("candidate index build", error))?;
    let exhaustive = ExhaustiveCandidateGenerator::new(&new_features)
        .map_err(|error| core_error("candidate index build", error))?;

    let mut recall = Vec::with_capacity(top_k.len());
    let mut oracle_recall = Vec::with_capacity(top_k.len());
    for k in top_k {
        recall.push(recall_at_k(&old_features, &counterparts, &inverted, *k)?);
        oracle_recall.push(recall_at_k(&old_features, &counterparts, &exhaustive, *k)?);
    }

    let counts = candidate_counts(&old_features, &inverted)?;
    let oracle_counts = candidate_counts(&old_features, &exhaustive)?;
    let visit_metrics = measure_visit_metrics(&old_features, &new_features, &inverted)?;
    let counterpart_old_blocks = counterparts
        .values()
        .filter(|counterparts| !counterparts.is_empty())
        .count();

    Ok(CandidateEvalRecord {
        case_name: case.name().to_owned(),
        old_blocks: old_blocks.len(),
        new_blocks: new_blocks.len(),
        counterpart_old_blocks,
        unmatched_old_blocks: old_blocks.len() - counterpart_old_blocks,
        recall_at_k: recall,
        oracle_recall_at_k: oracle_recall,
        candidate_count_p50: percentile(&counts, 0.50),
        candidate_count_p95: percentile(&counts, 0.95),
        candidate_count_max: counts.iter().copied().max().unwrap_or(0),
        oracle_candidate_count_p50: percentile(&oracle_counts, 0.50),
        oracle_candidate_count_p95: percentile(&oracle_counts, 0.95),
        oracle_candidate_count_max: oracle_counts.iter().copied().max().unwrap_or(0),
        estimated_visits_p50: visit_metrics.estimated_visits_p50,
        estimated_visits_p95: visit_metrics.estimated_visits_p95,
        estimated_visits_max: visit_metrics.estimated_visits_max,
        estimated_visits_upper_bound_total: visit_metrics.estimated_visits_upper_bound_total,
        max_candidate_visits: visit_metrics.max_candidate_visits,
        estimated_visits_upper_bound_exceeds_limit: visit_metrics
            .estimated_visits_upper_bound_exceeds_limit,
        ngram_posting_visits_total: visit_metrics.ngram_posting_visits_total,
        dominant_ngram_visits: visit_metrics.dominant_ngram_visits,
        dominant_ngram_df: visit_metrics.dominant_ngram_df,
    })
}

/// Renders a plan and returns its normalized blocks in document order.
fn normalized_blocks(plan: &RenderPlan, renderer: RendererKind) -> Result<Vec<BlockText>> {
    let pdf = renderer.render(plan, RenderLimits::default())?;
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let document = source
        .extract_outcome(
            Arc::from(pdf),
            ParseLimits::default(),
            ExtractionLimits::default(),
        )
        .map_err(|error| core_error("candidate-eval extraction", error))?
        .into_complete()
        .map_err(|error| core_error("candidate-eval extraction", error))?;
    let options = PipelineOptions::default();
    let lines = reconstruct_lines(&document, options.line)
        .map_err(|error| core_error("candidate-eval line reconstruction", error))?;
    let blocks = reconstruct_blocks(&document, &lines, options.block)
        .map_err(|error| core_error("candidate-eval block reconstruction", error))?;
    normalize_blocks(&document, &lines, &blocks)
        .map_err(|error| core_error("candidate-eval normalization", error))
}

/// The render plan's flattened lines joined by one space, matching the
/// canonical source the paragraph spans are defined against.
fn canonical_source(plan: &RenderPlan) -> String {
    plan.pages()
        .iter()
        .flatten()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Applies the SPEC §12.6 n-gram token element budget with the same
/// per-side helper and pair-global limit as production alignment, so
/// candidate evaluation fails on the same billing boundary as the pipeline.
fn enforce_ngram_budget(old: &[BlockText], new: &[BlockText]) -> Result<()> {
    let limit = PipelineOptions::default().max_ngram_token_elements;
    let old_elements = estimate_ngram_token_elements(old, NGRAM_SIZE, limit)
        .map_err(|error| core_error("candidate n-gram budget", error))?;
    let new_elements = estimate_ngram_token_elements(new, NGRAM_SIZE, limit)
        .map_err(|error| core_error("candidate n-gram budget", error))?;
    let aggregate = old_elements
        .checked_add(new_elements)
        .ok_or_else(|| ngram_budget_error(limit))?;
    if aggregate > limit {
        return Err(ngram_budget_error(limit));
    }
    Ok(())
}

fn ngram_budget_error(limit: usize) -> BenchError {
    BenchError::Core {
        stage: "candidate n-gram budget",
        source: pdfdelta_core::Error::LimitExceeded {
            resource: "alignment n-gram token elements",
            limit,
        },
    }
}

/// Global scalar span of every normalized block inside the canonical source.
///
/// UTF-8 slicing (`String::get`) needs a byte cursor, while the returned
/// spans use scalar offsets to match `CanonicalParagraphSpan` coordinates.
fn block_global_spans(canonical_source: &str, blocks: &[BlockText]) -> Result<Vec<(usize, usize)>> {
    let mut spans = Vec::with_capacity(blocks.len());
    let mut byte_cursor = 0_usize;
    let mut scalar_cursor = 0_usize;
    for (position, block) in blocks.iter().enumerate() {
        let text = &block.canonical.text;
        if text.is_empty() {
            return Err(BenchError::InvalidInput(
                "normalized block has empty canonical text".to_owned(),
            ));
        }
        let remaining = canonical_source.get(byte_cursor..).ok_or_else(|| {
            BenchError::InvalidInput(format!(
                "normalized block {} starts beyond the canonical source",
                block.block.0
            ))
        })?;
        // The separator between blocks is a single ASCII space, so the gap
        // advances both the byte and scalar cursors by one.
        let gap = if remaining.starts_with(text) {
            0
        } else if position > 0
            && remaining
                .strip_prefix(' ')
                .is_some_and(|remaining| remaining.starts_with(text))
        {
            1
        } else {
            return Err(BenchError::InvalidInput(format!(
                "normalized block {} does not map at canonical byte offset {byte_cursor}",
                block.block.0
            )));
        };
        let start = scalar_cursor.checked_add(gap).ok_or_else(|| {
            BenchError::InvalidInput("canonical block span start overflowed".into())
        })?;
        let end = start.checked_add(text.chars().count()).ok_or_else(|| {
            BenchError::InvalidInput("canonical block span end overflowed".into())
        })?;
        spans.push((start, end));
        let byte_advance = gap.checked_add(text.len()).ok_or_else(|| {
            BenchError::InvalidInput("canonical block byte advance overflowed".into())
        })?;
        byte_cursor = byte_cursor.checked_add(byte_advance).ok_or_else(|| {
            BenchError::InvalidInput("canonical block byte cursor overflowed".into())
        })?;
        let scalar_advance = gap.checked_add(text.chars().count()).ok_or_else(|| {
            BenchError::InvalidInput("canonical block scalar advance overflowed".into())
        })?;
        scalar_cursor = scalar_cursor.checked_add(scalar_advance).ok_or_else(|| {
            BenchError::InvalidInput("canonical block scalar cursor overflowed".into())
        })?;
    }
    if byte_cursor != canonical_source.len() {
        return Err(BenchError::InvalidInput(format!(
            "normalized blocks consumed {byte_cursor} of {} canonical bytes",
            canonical_source.len()
        )));
    }
    if scalar_cursor != canonical_source.chars().count() {
        return Err(BenchError::InvalidInput(format!(
            "normalized blocks consumed {scalar_cursor} of {} canonical scalars",
            canonical_source.chars().count()
        )));
    }
    Ok(spans)
}

struct BlockMapping {
    paragraph_ids: Vec<String>,
}

/// Maps every normalized block to the canonical paragraph ids whose spans
/// overlap its global span inside the same document.
fn map_blocks(
    canonical_source: &str,
    blocks: &[BlockText],
    paragraph_spans: &[CanonicalParagraphSpan],
) -> Result<Vec<BlockMapping>> {
    let global_spans = block_global_spans(canonical_source, blocks)?;
    Ok(global_spans
        .into_iter()
        .map(|span| BlockMapping {
            paragraph_ids: paragraph_spans
                .iter()
                .filter(|paragraph| paragraph.start() < span.1 && span.0 < paragraph.end())
                .map(|paragraph| paragraph.paragraph_id().to_owned())
                .collect(),
        })
        .collect())
}

/// True counterparts of every old block: new blocks that share at least one
/// canonical paragraph id. Paragraph spans are only used to map each block
/// to its own document's paragraphs; no cross-document span comparison is
/// made, so moved paragraphs keep their counterparts.
fn true_counterparts(
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    old_paragraphs: &[CanonicalParagraphSpan],
    new_paragraphs: &[CanonicalParagraphSpan],
    old_source: &str,
    new_source: &str,
) -> Result<HashMap<BlockId, Vec<BlockId>>> {
    let old_map = map_blocks(old_source, old_blocks, old_paragraphs)?;
    let new_map = map_blocks(new_source, new_blocks, new_paragraphs)?;
    let mut counterparts = HashMap::with_capacity(old_blocks.len());
    for (old_block, old_mapping) in old_blocks.iter().zip(&old_map) {
        let matches = new_blocks
            .iter()
            .zip(&new_map)
            .filter(|(_, new_mapping)| {
                old_mapping
                    .paragraph_ids
                    .iter()
                    .any(|id| new_mapping.paragraph_ids.contains(id))
            })
            .map(|(new_block, _)| new_block.block)
            .collect();
        counterparts.insert(old_block.block, matches);
    }
    Ok(counterparts)
}

/// Fraction of old blocks with a true counterpart whose counterpart appears
/// in the generator's top-K candidates.
fn recall_at_k(
    old_features: &[BlockFeatures],
    counterparts: &HashMap<BlockId, Vec<BlockId>>,
    generator: &dyn CandidateGenerator,
    k: usize,
) -> Result<f64> {
    let mut hit = 0_usize;
    let mut total = 0_usize;
    for features in old_features {
        let Some(true_blocks) = counterparts.get(&features.block) else {
            continue;
        };
        if true_blocks.is_empty() {
            continue;
        }
        total += 1;
        let top_k = generator
            .candidates(features, k)
            .map_err(|error| core_error("candidate query", error))?
            .into_iter()
            .map(|candidate| candidate.block)
            .collect::<HashSet<_>>();
        if true_blocks.iter().any(|block| top_k.contains(block)) {
            hit += 1;
        }
    }
    Ok(if total == 0 {
        0.0
    } else {
        hit as f64 / total as f64
    })
}

/// Full (untruncated) candidate count per old block.
fn candidate_counts(
    old_features: &[BlockFeatures],
    generator: &dyn CandidateGenerator,
) -> Result<Vec<usize>> {
    old_features
        .iter()
        .map(|features| {
            generator
                .candidates(features, usize::MAX)
                .map(|candidates| candidates.len())
                .map_err(|error| core_error("candidate query", error))
        })
        .collect()
}

/// Visit metrics of the inverted-index generator on one fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VisitMetrics {
    estimated_visits_p50: usize,
    estimated_visits_p95: usize,
    estimated_visits_max: usize,
    estimated_visits_upper_bound_total: usize,
    max_candidate_visits: usize,
    estimated_visits_upper_bound_exceeds_limit: bool,
    ngram_posting_visits_total: usize,
    dominant_ngram_visits: usize,
    dominant_ngram_df: usize,
}

/// Measures the inverted-index visit budget under the production alignment
/// limits: `estimated_visits` is charged per old block with the per-block
/// candidate limit, and the total is compared against `max_candidate_visits`.
///
/// The total sums every old block, while production excludes main exact
/// anchors, so an exceedance here is a necessary condition and investigation
/// signal rather than proof of a production LIMIT failure (false positives
/// possible, false negatives not).
fn measure_visit_metrics(
    old_features: &[BlockFeatures],
    new_features: &[BlockFeatures],
    generator: &dyn CandidateGenerator,
) -> Result<VisitMetrics> {
    let alignment_options = AlignmentOptions::default();
    let candidate_limit = alignment_options.candidate_limit;
    let visit_limit = alignment_options.max_candidate_visits;
    let visits = estimated_visits_per_block(old_features, generator, candidate_limit)?;
    let visits_total = visits.iter().try_fold(0_usize, |total, visits| {
        total
            .checked_add(*visits)
            .ok_or_else(|| visit_budget_error(visit_limit))
    })?;
    let ngram_stats = ngram_visit_stats(old_features, new_features, visit_limit)?;
    Ok(VisitMetrics {
        estimated_visits_p50: percentile(&visits, 0.50),
        estimated_visits_p95: percentile(&visits, 0.95),
        estimated_visits_max: visits.iter().copied().max().unwrap_or(0),
        estimated_visits_upper_bound_total: visits_total,
        max_candidate_visits: visit_limit,
        estimated_visits_upper_bound_exceeds_limit: visits_total > visit_limit,
        ngram_posting_visits_total: ngram_stats.total_posting_visits,
        dominant_ngram_visits: ngram_stats.dominant_ngram_visits,
        dominant_ngram_df: ngram_stats.dominant_ngram_df,
    })
}

/// Per-old-block estimated visits under the production per-block candidate
/// limit.
fn estimated_visits_per_block(
    old_features: &[BlockFeatures],
    generator: &dyn CandidateGenerator,
    limit: usize,
) -> Result<Vec<usize>> {
    old_features
        .iter()
        .map(|features| {
            generator
                .estimated_visits(features, limit)
                .map_err(|error| core_error("candidate visit estimate", error))
        })
        .collect()
}

/// N-gram posting visit statistics derived from new-side document
/// frequencies and old-side query occurrences.
struct NGramVisitStats {
    total_posting_visits: usize,
    dominant_ngram_visits: usize,
    dominant_ngram_df: usize,
}

/// Aggregates the n-gram component of `estimated_visits`: every old query
/// n-gram is charged its new-side posting length (document frequency) once
/// per old block containing it. Exact matches and the short-block fallback
/// are excluded.
fn ngram_visit_stats(
    old_features: &[BlockFeatures],
    new_features: &[BlockFeatures],
    visit_limit: usize,
) -> Result<NGramVisitStats> {
    let mut new_df = HashMap::<&NGram, usize>::new();
    for features in new_features {
        for ngram in &features.ngrams {
            *new_df.entry(ngram).or_default() += 1;
        }
    }
    let mut old_occurrences = HashMap::<&NGram, usize>::new();
    for features in old_features {
        for ngram in &features.ngrams {
            *old_occurrences.entry(ngram).or_default() += 1;
        }
    }

    let mut total_posting_visits = 0_usize;
    let mut dominant_ngram_visits = 0_usize;
    let mut dominant_ngram_df = 0_usize;
    for (ngram, occurrences) in &old_occurrences {
        let df = new_df.get(ngram).copied().unwrap_or(0);
        let visits = df
            .checked_mul(*occurrences)
            .ok_or_else(|| visit_budget_error(visit_limit))?;
        total_posting_visits = total_posting_visits
            .checked_add(visits)
            .ok_or_else(|| visit_budget_error(visit_limit))?;
        if visits > dominant_ngram_visits {
            dominant_ngram_visits = visits;
            dominant_ngram_df = df;
        }
    }
    Ok(NGramVisitStats {
        total_posting_visits,
        dominant_ngram_visits,
        dominant_ngram_df,
    })
}

fn visit_budget_error(limit: usize) -> BenchError {
    BenchError::Core {
        stage: "candidate visit budget",
        source: pdfdelta_core::Error::LimitExceeded {
            resource: "alignment candidate visits",
            limit,
        },
    }
}

/// Nearest-rank percentile: the value at index `round((n - 1) * quantile)`
/// of the sorted sample, without interpolation.
fn percentile(values: &[usize], quantile: f64) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) as f64 * quantile).round() as usize;
    sorted[index]
}

fn core_error(stage: &'static str, error: pdfdelta_core::Error) -> BenchError {
    BenchError::Core {
        stage,
        source: error,
    }
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::{
        layout::BlockId,
        normalize::{BlockText, ComparableToken, MappedText},
    };

    use super::*;

    fn block_text(id: u64, text: &str) -> BlockText {
        BlockText {
            block: BlockId(id),
            raw: MappedText {
                text: text.to_owned(),
                source_map: Vec::new(),
                unmapped: Vec::new(),
            },
            canonical: MappedText {
                text: text.to_owned(),
                source_map: Vec::new(),
                unmapped: Vec::new(),
            },
            matching: text.to_owned(),
            matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: Vec::new(),
        }
    }

    /// Four-letter base-26 suffix, unique for values below 26^4.
    fn base26_suffix(mut value: usize) -> String {
        let mut chars = Vec::with_capacity(4);
        while value > 0 {
            chars.push((b'a' + (value % 26) as u8) as char);
            value /= 26;
        }
        while chars.len() < 4 {
            chars.push('a');
        }
        chars.into_iter().rev().collect()
    }

    #[test]
    fn block_global_spans_use_scalar_coordinates_for_non_ascii_text() {
        // Each kana word is 5 scalars but 15 bytes; paragraph spans from
        // mutation.rs count scalars, so block spans must too.
        let source = "あいうえお かきくけこ さしすせそ";
        let blocks = [
            block_text(1, "あいうえお"),
            block_text(2, "かきくけこ"),
            block_text(3, "さしすせそ"),
        ];

        let spans = block_global_spans(source, &blocks).expect("blocks map to source");

        assert_eq!(spans, vec![(0, 5), (6, 11), (12, 17)]);
    }

    #[test]
    fn block_global_spans_accept_mixed_ascii_and_non_ascii_blocks() {
        let source = "alpha あいうえお omega";
        let blocks = [
            block_text(1, "alpha"),
            block_text(2, "あいうえお"),
            block_text(3, "omega"),
        ];

        let spans = block_global_spans(source, &blocks).expect("blocks map to source");

        assert_eq!(spans, vec![(0, 5), (6, 11), (12, 17)]);
    }

    #[test]
    fn block_global_spans_report_byte_offsets_for_unmapped_blocks() {
        let source = "あいうえお かきくけこ";
        let blocks = [block_text(1, "あいうえお"), block_text(2, "さしすせそ")];

        let error = block_global_spans(source, &blocks).expect_err("second block does not map");

        assert!(matches!(
            error,
            BenchError::InvalidInput(message) if message.contains("byte offset")
        ));
    }

    #[test]
    fn high_df_ngram_upper_bound_exceeds_the_budget() {
        // 1001 identical-prefix blocks per side: every block shares the
        // "aaa" n-gram, so each old query pays 1001 postings and the total
        // exceeds the default 1,000,000 visit budget without rendering.
        // Every block is 10 tokens, below the 16-token exact-anchor
        // threshold, so no block is excluded as a main anchor and the
        // upper-bound total equals the production charge in this fixture.
        let count = 1001;
        let old_blocks = (0..count)
            .map(|index| block_text(index as u64, &format!("aaaaaa{}", base26_suffix(index))))
            .collect::<Vec<_>>();
        let new_blocks = (0..count)
            .map(|index| {
                block_text(
                    10_000 + index as u64,
                    &format!("aaaaaa{}", base26_suffix(index)),
                )
            })
            .collect::<Vec<_>>();
        let old_features =
            build_block_features(&old_blocks, NGRAM_SIZE).expect("old features build");
        let new_features =
            build_block_features(&new_blocks, NGRAM_SIZE).expect("new features build");
        let inverted = InvertedIndexCandidateGenerator::new(&new_features).expect("index builds");

        let metrics = measure_visit_metrics(&old_features, &new_features, &inverted)
            .expect("visit metrics measure");

        assert!(metrics.estimated_visits_upper_bound_total > metrics.max_candidate_visits);
        assert!(metrics.estimated_visits_upper_bound_exceeds_limit);
        assert_eq!(metrics.dominant_ngram_visits, count * count);
        assert_eq!(metrics.dominant_ngram_df, count);

        // Recall still resolves every old block to its identical new block.
        let counterparts = old_features
            .iter()
            .zip(&new_features)
            .map(|(old, new)| (old.block, vec![new.block]))
            .collect::<HashMap<_, _>>();
        let recall =
            recall_at_k(&old_features, &counterparts, &inverted, 5).expect("recall measures");
        assert_eq!(recall, 1.0);
    }
}
