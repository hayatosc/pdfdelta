//! Candidate generation evaluation (SPEC §12.7).
//!
//! Measures top-K recall and per-old-block candidate counts of the
//! inverted-index candidate generator against the exhaustive all-pairs
//! oracle on synthetic fixtures. True counterparts are defined by canonical
//! paragraph id plus global span overlap, so moved, re-wrapped, and
//! re-paginated paragraphs still resolve to their correct new blocks.
//!
//! Latency and memory measurement are deferred to a later slice; this module
//! only covers recall and candidate counts.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pdfdelta_core::{
    alignment::{
        BlockFeatures, CandidateGenerator, ExhaustiveCandidateGenerator,
        InvertedIndexCandidateGenerator, build_block_features, estimate_ngram_token_elements,
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
        normalize::{BlockText, MappedText},
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
            matching: String::new(),
            matching_tokens: Vec::new(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: Vec::new(),
        }
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
}
