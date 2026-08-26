//! Candidate generation evaluation.
//!
//! Measures top-K recall and per-old-block candidate counts of the
//! inverted-index candidate generator against the exhaustive all-pairs
//! oracle on synthetic fixtures. True counterparts are defined by canonical
//! paragraph id plus global span overlap, so moved, re-wrapped, and
//! re-paginated paragraphs still resolve to their correct new blocks.
//!
//! Latency and memory measurement are deferred to a later slice; this module
//! covers recall, candidate counts, and estimated visit budgets.

use std::{
    collections::{HashMap, HashSet},
    fs::OpenOptions,
    io::Write,
    path::Path,
    sync::Arc,
};

use pdfdelta_core::{
    alignment::{
        AlignmentOptions, BlockFeatures, CandidateGenerator, ExhaustiveCandidateGenerator,
        InvertedIndexCandidateGenerator, MinHashLshCandidateGenerator, NGram, build_block_features,
        estimate_ngram_token_elements,
    },
    layout::{BlockId, reconstruct_blocks, reconstruct_lines},
    model::{Document, Glyph},
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

/// All-old-block candidate visit pressure of the inverted-index generator.
///
/// `estimated_visits_upper_bound_total` sums every old block, while
/// production alignment excludes main exact anchors, so an upper-bound
/// exceedance is a necessary condition and investigation signal, not proof
/// of a production LIMIT failure (false positives possible, false negatives
/// not).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CandidateVisitPressure {
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
    /// `max_candidate_visits`.
    pub estimated_visits_upper_bound_exceeds_limit: bool,
    /// Total n-gram posting visits across all old blocks, excluding exact
    /// matches and the short-block fallback.
    pub ngram_posting_visits_total: usize,
    /// Aggregate visits of the single most-visited n-gram (new-side document
    /// frequency times old-side occurrence count).
    pub dominant_ngram_visits: usize,
    /// New-side document frequency of the dominant n-gram.
    pub dominant_ngram_df: usize,
    /// Unique n-grams present in old blocks whose new-side document
    /// frequency is greater than zero.
    pub shared_ngram_count: usize,
    /// Cumulative visits of the ten largest n-gram contributors.
    pub top_10_ngram_visits: usize,
    /// Shared n-grams needed to reach at least half of the total posting
    /// visits, counting contributions in descending visit order. This is an
    /// observed quantile of the current measurement, not a fixed tuning
    /// threshold.
    pub ngrams_for_50_percent_visits: usize,
    /// Shared n-grams needed to reach 90% of the total posting visits,
    /// counting contributions in descending visit order. This is an
    /// observed quantile of the current measurement, not a fixed tuning
    /// threshold.
    pub ngrams_for_90_percent_visits: usize,
    /// Shared-set new-side document frequency (nearest-rank p50).
    pub shared_ngram_df_p50: usize,
    /// Shared-set new-side document frequency (nearest-rank p95).
    pub shared_ngram_df_p95: usize,
    /// Shared-set new-side document frequency maximum.
    pub shared_ngram_df_max: usize,
}

/// Candidate generation metrics for one synthetic fixture.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct CandidateEvalRecord {
    pub case_name: String,
    pub renderer: RendererKind,
    /// K values whose recall is reported, aligned with `recall_at_k` and
    /// `oracle_recall_at_k` in the given order.
    pub top_k: Vec<usize>,
    pub old_blocks: usize,
    pub new_blocks: usize,
    /// Old blocks that have at least one true counterpart in the new side;
    /// this is the recall denominator.
    pub counterpart_old_blocks: usize,
    /// Old blocks without any true counterpart (e.g. deleted paragraphs).
    pub unmatched_old_blocks: usize,
    /// Inverted-index recall@K, one entry per K in `top_k`.
    pub recall_at_k: Vec<f64>,
    /// MinHash LSH recall@K, one entry per K in `top_k`.
    pub minhash_recall_at_k: Vec<f64>,
    /// Exhaustive-oracle recall@K, one entry per K in `top_k`.
    pub oracle_recall_at_k: Vec<f64>,
    /// Inverted-index candidate counts per old block (nearest-rank p50).
    pub candidate_count_p50: usize,
    /// Inverted-index candidate counts per old block (nearest-rank p95).
    pub candidate_count_p95: usize,
    pub candidate_count_max: usize,
    /// MinHash LSH candidate counts per old block (nearest-rank p50).
    pub minhash_candidate_count_p50: usize,
    /// MinHash LSH candidate counts per old block (nearest-rank p95).
    pub minhash_candidate_count_p95: usize,
    pub minhash_candidate_count_max: usize,
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
    /// MinHash LSH estimated visits per old block (nearest-rank p50).
    pub minhash_estimated_visits_p50: usize,
    /// MinHash LSH estimated visits per old block (nearest-rank p95).
    pub minhash_estimated_visits_p95: usize,
    pub minhash_estimated_visits_max: usize,
    /// Sum of MinHash LSH estimated visits across all old blocks.
    pub minhash_estimated_visits_upper_bound_total: usize,
    /// Whether MinHash LSH `estimated_visits_upper_bound_total` exceeds
    /// `max_candidate_visits`.
    pub minhash_estimated_visits_upper_bound_exceeds_limit: bool,
    /// Total n-gram posting visits across all old blocks, excluding exact
    /// matches and the short-block fallback.
    pub ngram_posting_visits_total: usize,
    /// Aggregate visits of the single most-visited n-gram (new-side document
    /// frequency times old-side occurrence count).
    pub dominant_ngram_visits: usize,
    /// New-side document frequency of the dominant n-gram.
    pub dominant_ngram_df: usize,
    /// Unique n-grams present in old blocks whose new-side document
    /// frequency is greater than zero.
    pub shared_ngram_count: usize,
    /// Cumulative visits of the ten largest n-gram contributors.
    pub top_10_ngram_visits: usize,
    /// Shared n-grams needed to reach at least half of the total posting
    /// visits, counting contributions in descending visit order; an observed
    /// quantile, not a fixed tuning threshold.
    pub ngrams_for_50_percent_visits: usize,
    /// Shared n-grams needed to reach 90% of the total posting visits,
    /// counting contributions in descending visit order; an observed
    /// quantile, not a fixed tuning threshold.
    pub ngrams_for_90_percent_visits: usize,
    /// Shared-set new-side document frequency (nearest-rank p50).
    pub shared_ngram_df_p50: usize,
    /// Shared-set new-side document frequency (nearest-rank p95).
    pub shared_ngram_df_p95: usize,
    /// Shared-set new-side document frequency maximum.
    pub shared_ngram_df_max: usize,
}

impl CandidateEvalRecord {
    /// Whether the inverted-index recall meets the exhaustive oracle at
    /// every K, and all candidate generator recall vectors are well-formed.
    /// Recall values share the same integer denominator, so exact
    /// comparison is used; an empty evaluation or any vector length
    /// mismatch is never healthy.
    pub fn healthy(&self) -> bool {
        !self.top_k.is_empty()
            && self.top_k.len() == self.recall_at_k.len()
            && self.recall_at_k.len() == self.oracle_recall_at_k.len()
            && self.recall_at_k.len() == self.minhash_recall_at_k.len()
            && self
                .recall_at_k
                .iter()
                .zip(&self.oracle_recall_at_k)
                .all(|(inverted, oracle)| inverted >= oracle)
    }
}

/// Rejects empty or zero top-K values before any rendering happens.
fn validate_top_k(top_k: &[usize]) -> Result<()> {
    if top_k.is_empty() {
        return Err(BenchError::InvalidInput(
            "candidate evaluation requires at least one top-K value".to_owned(),
        ));
    }
    if let Some(k) = top_k.iter().find(|k| **k == 0) {
        return Err(BenchError::InvalidInput(format!(
            "candidate evaluation top-K values must be greater than zero, got {k}"
        )));
    }
    Ok(())
}

/// Runs candidate generation evaluation for one fixture.
///
/// `top_k` selects the K values whose recall is reported; candidate counts
/// are always measured over the full (untruncated) candidate set. The slice
/// must be non-empty with every K greater than zero; duplicates are a CLI
/// presentation concern and are not rejected here.
pub fn evaluate_candidate_generation(
    case: &BenchmarkCase,
    renderer: RendererKind,
    top_k: &[usize],
) -> Result<CandidateEvalRecord> {
    validate_top_k(top_k)?;
    let options = PipelineOptions::default();
    let plan = case.plan();
    let old_blocks = normalized_blocks(plan.old(), renderer)?;
    let new_blocks = normalized_blocks(plan.new_plan(), renderer)?;
    enforce_ngram_budget(&old_blocks, &new_blocks, options)?;
    let counterparts = true_counterparts(
        &old_blocks,
        &new_blocks,
        plan.old_paragraphs(),
        plan.new_paragraphs(),
        &canonical_source(plan.old()),
        &canonical_source(plan.new_plan()),
    )?;

    let old_features = build_block_features(&old_blocks, options.ngram_size)
        .map_err(|error| core_error("candidate feature build", error))?;
    let new_features = build_block_features(&new_blocks, options.ngram_size)
        .map_err(|error| core_error("candidate feature build", error))?;
    let inverted = InvertedIndexCandidateGenerator::new(&new_features)
        .map_err(|error| core_error("candidate index build", error))?;
    let minhash = MinHashLshCandidateGenerator::new(&new_features)
        .map_err(|error| core_error("minhash candidate index build", error))?;
    let exhaustive = ExhaustiveCandidateGenerator::new(&new_features)
        .map_err(|error| core_error("candidate index build", error))?;

    let mut recall = Vec::with_capacity(top_k.len());
    let mut minhash_recall = Vec::with_capacity(top_k.len());
    let mut oracle_recall = Vec::with_capacity(top_k.len());
    for k in top_k {
        recall.push(recall_at_k(&old_features, &counterparts, &inverted, *k)?);
        minhash_recall.push(recall_at_k(&old_features, &counterparts, &minhash, *k)?);
        oracle_recall.push(recall_at_k(&old_features, &counterparts, &exhaustive, *k)?);
    }

    let counts = candidate_counts(&old_features, &inverted)?;
    let minhash_counts = candidate_counts(&old_features, &minhash)?;
    let oracle_counts = candidate_counts(&old_features, &exhaustive)?;
    let pressure =
        measure_visit_metrics(&old_features, &new_features, &inverted, options.alignment)?;
    let minhash_pressure =
        measure_visit_metrics(&old_features, &new_features, &minhash, options.alignment)?;
    let counterpart_old_blocks = counterparts
        .values()
        .filter(|counterparts| !counterparts.is_empty())
        .count();

    Ok(CandidateEvalRecord {
        case_name: case.name().to_owned(),
        renderer,
        top_k: top_k.to_vec(),
        old_blocks: old_blocks.len(),
        new_blocks: new_blocks.len(),
        counterpart_old_blocks,
        unmatched_old_blocks: old_blocks.len() - counterpart_old_blocks,
        recall_at_k: recall,
        minhash_recall_at_k: minhash_recall,
        oracle_recall_at_k: oracle_recall,
        candidate_count_p50: percentile(&counts, 0.50),
        candidate_count_p95: percentile(&counts, 0.95),
        candidate_count_max: counts.iter().copied().max().unwrap_or(0),
        minhash_candidate_count_p50: percentile(&minhash_counts, 0.50),
        minhash_candidate_count_p95: percentile(&minhash_counts, 0.95),
        minhash_candidate_count_max: minhash_counts.iter().copied().max().unwrap_or(0),
        oracle_candidate_count_p50: percentile(&oracle_counts, 0.50),
        oracle_candidate_count_p95: percentile(&oracle_counts, 0.95),
        oracle_candidate_count_max: oracle_counts.iter().copied().max().unwrap_or(0),
        estimated_visits_p50: pressure.estimated_visits_p50,
        estimated_visits_p95: pressure.estimated_visits_p95,
        estimated_visits_max: pressure.estimated_visits_max,
        estimated_visits_upper_bound_total: pressure.estimated_visits_upper_bound_total,
        max_candidate_visits: pressure.max_candidate_visits,
        estimated_visits_upper_bound_exceeds_limit: pressure
            .estimated_visits_upper_bound_exceeds_limit,
        minhash_estimated_visits_p50: minhash_pressure.estimated_visits_p50,
        minhash_estimated_visits_p95: minhash_pressure.estimated_visits_p95,
        minhash_estimated_visits_max: minhash_pressure.estimated_visits_max,
        minhash_estimated_visits_upper_bound_total: minhash_pressure
            .estimated_visits_upper_bound_total,
        minhash_estimated_visits_upper_bound_exceeds_limit: minhash_pressure
            .estimated_visits_upper_bound_exceeds_limit,
        ngram_posting_visits_total: pressure.ngram_posting_visits_total,
        dominant_ngram_visits: pressure.dominant_ngram_visits,
        dominant_ngram_df: pressure.dominant_ngram_df,
        shared_ngram_count: pressure.shared_ngram_count,
        top_10_ngram_visits: pressure.top_10_ngram_visits,
        ngrams_for_50_percent_visits: pressure.ngrams_for_50_percent_visits,
        ngrams_for_90_percent_visits: pressure.ngrams_for_90_percent_visits,
        shared_ngram_df_p50: pressure.shared_ngram_df_p50,
        shared_ngram_df_p95: pressure.shared_ngram_df_p95,
        shared_ngram_df_max: pressure.shared_ngram_df_max,
    })
}

/// Measures the all-old-block candidate visit pressure of the inverted-index
/// generator for two extracted glyph documents, using the same layout,
/// n-gram, and alignment options as production. The documents are borrowed,
/// so no re-extraction happens.
pub fn evaluate_candidate_visit_pressure(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: PipelineOptions,
) -> Result<CandidateVisitPressure> {
    let old_blocks = normalized_blocks_from_document(old, options)?;
    let new_blocks = normalized_blocks_from_document(new, options)?;
    enforce_ngram_budget(&old_blocks, &new_blocks, options)?;
    let old_features = build_block_features(&old_blocks, options.ngram_size)
        .map_err(|error| core_error("candidate feature build", error))?;
    let new_features = build_block_features(&new_blocks, options.ngram_size)
        .map_err(|error| core_error("candidate feature build", error))?;
    let inverted = InvertedIndexCandidateGenerator::new(&new_features)
        .map_err(|error| core_error("candidate index build", error))?;
    measure_visit_metrics(&old_features, &new_features, &inverted, options.alignment)
}

/// Measures the all-old-block candidate visit pressure of the MinHash LSH
/// generator for two extracted glyph documents.
pub fn evaluate_minhash_candidate_visit_pressure(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: PipelineOptions,
) -> Result<CandidateVisitPressure> {
    let old_blocks = normalized_blocks_from_document(old, options)?;
    let new_blocks = normalized_blocks_from_document(new, options)?;
    enforce_ngram_budget(&old_blocks, &new_blocks, options)?;
    let old_features = build_block_features(&old_blocks, options.ngram_size)
        .map_err(|error| core_error("candidate feature build", error))?;
    let new_features = build_block_features(&new_blocks, options.ngram_size)
        .map_err(|error| core_error("candidate feature build", error))?;
    let minhash = MinHashLshCandidateGenerator::new(&new_features)
        .map_err(|error| core_error("candidate minhash index build", error))?;
    measure_visit_metrics(&old_features, &new_features, &minhash, options.alignment)
}

/// Writes every record as a pretty JSON array to a new file, refusing to
/// overwrite an existing path via `create_new`. Suppressing partial-run
/// artifacts is the caller's responsibility.
pub fn write_candidates_json(path: &Path, records: &[CandidateEvalRecord]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            BenchError::InvalidInput(format!(
                "cannot create candidate evaluation JSON output {}: {error}",
                path.display()
            ))
        })?;
    let bytes = serde_json::to_vec_pretty(records).map_err(|error| {
        BenchError::InvalidInput(format!(
            "cannot serialize candidate evaluation JSON: {error}"
        ))
    })?;
    file.write_all(&bytes).map_err(|error| {
        BenchError::InvalidInput(format!("cannot write candidate evaluation JSON: {error}"))
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
    normalized_blocks_from_document(&document, PipelineOptions::default())
}

/// Reconstructs lines and blocks from an extracted glyph document and
/// normalizes them under the given pipeline options.
fn normalized_blocks_from_document(
    document: &Document<Glyph>,
    options: PipelineOptions,
) -> Result<Vec<BlockText>> {
    let lines = reconstruct_lines(document, options.line)
        .map_err(|error| core_error("candidate-eval line reconstruction", error))?;
    let blocks = reconstruct_blocks(document, &lines, options.block)
        .map_err(|error| core_error("candidate-eval block reconstruction", error))?;
    normalize_blocks(document, &lines, &blocks)
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

/// Applies the n-gram token element budget with the same
/// per-side helper and pair-global limit as production alignment, so
/// candidate evaluation fails on the same billing boundary as the pipeline.
fn enforce_ngram_budget(
    old: &[BlockText],
    new: &[BlockText],
    options: PipelineOptions,
) -> Result<()> {
    let limit = options.max_ngram_token_elements;
    let old_elements = estimate_ngram_token_elements(old, options.ngram_size, limit)
        .map_err(|error| core_error("candidate n-gram budget", error))?;
    let new_elements = estimate_ngram_token_elements(new, options.ngram_size, limit)
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

/// Measures the inverted-index visit budget under the given alignment
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
    alignment_options: AlignmentOptions,
) -> Result<CandidateVisitPressure> {
    let candidate_limit = alignment_options.candidate_limit;
    let visit_limit = alignment_options.max_candidate_visits;
    let visits = estimated_visits_per_block(old_features, generator, candidate_limit)?;
    let visits_total = visits.iter().try_fold(0_usize, |total, visits| {
        total
            .checked_add(*visits)
            .ok_or_else(|| visit_budget_error(visit_limit))
    })?;
    let ngram_stats = ngram_visit_stats(old_features, new_features, visit_limit)?;
    Ok(CandidateVisitPressure {
        estimated_visits_p50: percentile(&visits, 0.50),
        estimated_visits_p95: percentile(&visits, 0.95),
        estimated_visits_max: visits.iter().copied().max().unwrap_or(0),
        estimated_visits_upper_bound_total: visits_total,
        max_candidate_visits: visit_limit,
        estimated_visits_upper_bound_exceeds_limit: visits_total > visit_limit,
        ngram_posting_visits_total: ngram_stats.total_posting_visits,
        dominant_ngram_visits: ngram_stats.dominant_ngram_visits,
        dominant_ngram_df: ngram_stats.dominant_ngram_df,
        shared_ngram_count: ngram_stats.shared_ngram_count,
        top_10_ngram_visits: ngram_stats.top_10_ngram_visits,
        ngrams_for_50_percent_visits: ngram_stats.ngrams_for_50_percent_visits,
        ngrams_for_90_percent_visits: ngram_stats.ngrams_for_90_percent_visits,
        shared_ngram_df_p50: ngram_stats.shared_ngram_df_p50,
        shared_ngram_df_p95: ngram_stats.shared_ngram_df_p95,
        shared_ngram_df_max: ngram_stats.shared_ngram_df_max,
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
    shared_ngram_count: usize,
    top_10_ngram_visits: usize,
    ngrams_for_50_percent_visits: usize,
    ngrams_for_90_percent_visits: usize,
    shared_ngram_df_p50: usize,
    shared_ngram_df_p95: usize,
    shared_ngram_df_max: usize,
}

/// Distribution summary over per-n-gram `(visits, df)` contributions of the
/// shared n-gram set (new-side document frequency greater than zero).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NGramDistribution {
    shared_ngram_count: usize,
    top_10_ngram_visits: usize,
    ngrams_for_50_percent_visits: usize,
    ngrams_for_90_percent_visits: usize,
    shared_ngram_df_p50: usize,
    shared_ngram_df_p95: usize,
    shared_ngram_df_max: usize,
}

/// Summarizes the visit distribution over per-n-gram `(visits, df)`
/// contributions of the shared n-gram set. The 50%/90% counts are observed
/// quantiles of this measurement, not fixed tuning thresholds.
fn summarize_ngram_distribution(
    contributions: &[(usize, usize)],
    total_posting_visits: usize,
) -> NGramDistribution {
    let mut by_descending_visits = contributions.to_vec();
    by_descending_visits.sort_unstable_by_key(|contribution| std::cmp::Reverse(contribution.0));
    let (n50, n90) = cumulative_visit_counts(&by_descending_visits, total_posting_visits);
    let dfs = by_descending_visits
        .iter()
        .map(|(_, df)| *df)
        .collect::<Vec<_>>();
    NGramDistribution {
        shared_ngram_count: by_descending_visits.len(),
        top_10_ngram_visits: by_descending_visits
            .iter()
            .take(10)
            .map(|(visits, _)| *visits)
            .sum(),
        ngrams_for_50_percent_visits: n50,
        ngrams_for_90_percent_visits: n90,
        shared_ngram_df_p50: percentile(&dfs, 0.50),
        shared_ngram_df_p95: percentile(&dfs, 0.95),
        shared_ngram_df_max: dfs.iter().copied().max().unwrap_or(0),
    }
}

/// Counts the shared n-grams needed to reach the observed 50% and 90%
/// visit shares, accumulating in descending visit order. The 50% target is
/// `ceil(total / 2)`, so the count is the first prefix that accumulates at
/// least half of the visits. Equal-visit ties contribute equally at every
/// prefix, so both counts depend only on the multiset of contributions.
/// `total_posting_visits` is the checked sum of all contributions; with
/// zero total both counts are zero.
fn cumulative_visit_counts(
    by_descending_visits: &[(usize, usize)],
    total_posting_visits: usize,
) -> (usize, usize) {
    if total_posting_visits == 0 {
        return (0, 0);
    }
    let target_50 = total_posting_visits.div_ceil(2);
    let target_90 = total_posting_visits.saturating_sub(total_posting_visits / 10);
    let mut accumulated = 0_usize;
    let mut n50 = 0_usize;
    let mut n90 = 0_usize;
    for (index, (visits, _)) in by_descending_visits.iter().enumerate() {
        // The full sum was verified through checked arithmetic upstream, so
        // every prefix sum fits.
        accumulated += visits;
        if n50 == 0 && accumulated >= target_50 {
            n50 = index + 1;
        }
        if n90 == 0 && accumulated >= target_90 {
            n90 = index + 1;
        }
        if n50 > 0 && n90 > 0 {
            break;
        }
    }
    (n50, n90)
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

    let mut contributions = Vec::new();
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
        if visits > dominant_ngram_visits
            || (visits == dominant_ngram_visits && df > dominant_ngram_df)
        {
            dominant_ngram_visits = visits;
            dominant_ngram_df = df;
        }
        if df > 0 {
            contributions.push((visits, df));
        }
    }
    let distribution = summarize_ngram_distribution(&contributions, total_posting_visits);
    Ok(NGramVisitStats {
        total_posting_visits,
        dominant_ngram_visits,
        dominant_ngram_df,
        shared_ngram_count: distribution.shared_ngram_count,
        top_10_ngram_visits: distribution.top_10_ngram_visits,
        ngrams_for_50_percent_visits: distribution.ngrams_for_50_percent_visits,
        ngrams_for_90_percent_visits: distribution.ngrams_for_90_percent_visits,
        shared_ngram_df_p50: distribution.shared_ngram_df_p50,
        shared_ngram_df_p95: distribution.shared_ngram_df_p95,
        shared_ngram_df_max: distribution.shared_ngram_df_max,
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
        alignment::{BlockFeatures, ExactHash, NGram},
        layout::BlockId,
        model::{
            DecodedText, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect, TextRenderMode,
            Vec2,
        },
        normalize::{BlockText, ComparableToken, MappedText},
        pdf::ObjectRef,
    };

    use crate::{
        canonical::{CanonicalDocument, Paragraph},
        mutation::Mutation,
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
        let old_features = build_block_features(&old_blocks, PipelineOptions::default().ngram_size)
            .expect("old features build");
        let new_features = build_block_features(&new_blocks, PipelineOptions::default().ngram_size)
            .expect("new features build");
        let inverted = InvertedIndexCandidateGenerator::new(&new_features).expect("index builds");

        let metrics = measure_visit_metrics(
            &old_features,
            &new_features,
            &inverted,
            AlignmentOptions::default(),
        )
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

    fn record_with_recall(
        recall_at_k: Vec<f64>,
        oracle_recall_at_k: Vec<f64>,
    ) -> CandidateEvalRecord {
        CandidateEvalRecord {
            case_name: "case".to_owned(),
            renderer: RendererKind::LopdfTj,
            top_k: vec![5, 10],
            old_blocks: 3,
            new_blocks: 3,
            counterpart_old_blocks: 3,
            unmatched_old_blocks: 0,
            recall_at_k: recall_at_k.clone(),
            minhash_recall_at_k: recall_at_k,
            oracle_recall_at_k,
            candidate_count_p50: 1,
            candidate_count_p95: 1,
            candidate_count_max: 1,
            minhash_candidate_count_p50: 1,
            minhash_candidate_count_p95: 1,
            minhash_candidate_count_max: 1,
            oracle_candidate_count_p50: 3,
            oracle_candidate_count_p95: 3,
            oracle_candidate_count_max: 3,
            estimated_visits_p50: 1,
            estimated_visits_p95: 1,
            estimated_visits_max: 1,
            estimated_visits_upper_bound_total: 3,
            max_candidate_visits: 1_000_000,
            estimated_visits_upper_bound_exceeds_limit: false,
            minhash_estimated_visits_p50: 1,
            minhash_estimated_visits_p95: 1,
            minhash_estimated_visits_max: 1,
            minhash_estimated_visits_upper_bound_total: 3,
            minhash_estimated_visits_upper_bound_exceeds_limit: false,
            ngram_posting_visits_total: 3,
            dominant_ngram_visits: 1,
            dominant_ngram_df: 1,
            shared_ngram_count: 3,
            top_10_ngram_visits: 3,
            ngrams_for_50_percent_visits: 2,
            ngrams_for_90_percent_visits: 3,
            shared_ngram_df_p50: 1,
            shared_ngram_df_p95: 1,
            shared_ngram_df_max: 1,
        }
    }

    #[test]
    fn healthy_requires_recall_at_or_above_oracle_at_every_k() {
        assert!(record_with_recall(vec![1.0, 1.0], vec![1.0, 1.0]).healthy());
        assert!(record_with_recall(vec![1.0, 0.9], vec![1.0, 0.8]).healthy());
        assert!(!record_with_recall(vec![1.0, 0.7], vec![1.0, 0.8]).healthy());
    }

    #[test]
    fn healthy_rejects_vector_length_mismatch() {
        assert!(!record_with_recall(vec![1.0], vec![1.0, 1.0]).healthy());
        assert!(!record_with_recall(vec![1.0, 1.0], vec![1.0]).healthy());
    }

    #[test]
    fn healthy_rejects_top_k_length_mismatch() {
        let mut record = record_with_recall(vec![1.0, 1.0], vec![1.0, 1.0]);
        record.top_k = vec![5];
        assert!(!record.healthy());
    }

    #[test]
    fn healthy_rejects_empty_top_k() {
        let mut record = record_with_recall(Vec::new(), Vec::new());
        record.top_k = Vec::new();
        assert!(!record.healthy());
    }

    #[test]
    fn evaluate_candidate_generation_rejects_empty_and_zero_top_k_before_rendering() {
        // 16 lines at line_gap 48 exceed the vertical page area, so
        // rendering would fail; the top-K validation must run first and win.
        let paragraphs = (0..16)
            .map(|index| {
                Paragraph::new(
                    format!("p{index:02}"),
                    format!("Paragraph {index} keeps a steady cadence"),
                )
                .expect("valid paragraph")
            })
            .collect();
        let document = CanonicalDocument::new(paragraphs).expect("valid document");
        let case = BenchmarkCase::new(
            "tall-render",
            document,
            Mutation::LineHeightChange { new_line_gap: 48 },
            30,
        )
        .expect("valid benchmark case");

        for top_k in [&[][..], &[0][..], &[5, 0][..]] {
            let error = evaluate_candidate_generation(&case, RendererKind::LopdfTj, top_k)
                .expect_err("invalid top-K must fail");
            assert!(
                matches!(error, BenchError::InvalidInput(message) if message.contains("top-K")),
                "top_k={top_k:?}"
            );
        }
    }

    #[test]
    fn write_candidates_json_creates_new_artifact_and_rejects_existing() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "pdfbench-candidate-eval-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        let records = vec![record_with_recall(vec![1.0], vec![1.0])];

        write_candidates_json(&path, &records).expect("artifact writes");
        let json = std::fs::read_to_string(&path).expect("artifact reads");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("artifact parses");
        assert_eq!(parsed[0]["renderer"], "lopdf-tj");
        assert_eq!(parsed[0]["top_k"], serde_json::json!([5, 10]));
        assert!(write_candidates_json(&path, &records).is_err());
        std::fs::remove_file(&path).expect("artifact removed");
    }

    /// One glyph per non-space scalar, one line per entry at a distinct y.
    fn glyph_document(lines: &[&str]) -> Document<Glyph> {
        let mut glyphs = Vec::new();
        let mut next_id = 1_u64;
        for (line_index, text) in lines.iter().enumerate() {
            let mut inline_offset = 0.0;
            for character in text.chars() {
                if character == ' ' {
                    inline_offset += 5.0;
                    continue;
                }
                let x = inline_offset;
                let y = 100.0 - line_index as f64 * 30.0;
                glyphs.push(Glyph {
                    id: GlyphId(next_id),
                    text: DecodedText::Mapped(character.to_string()),
                    raw_code: character.to_string().into_bytes(),
                    page: PageId(0),
                    bbox: Rect {
                        min: Vec2 { x, y },
                        max: Vec2 {
                            x: x + 5.0,
                            y: y + 10.0,
                        },
                    },
                    baseline: Vec2 { x, y },
                    direction: Vec2 { x: 1.0, y: 0.0 },
                    font_id: FontId(1),
                    font_size: 10.0,
                    render_order: u32::try_from(next_id).expect("fixture glyph id fits in u32"),
                    render_mode: TextRenderMode::Fill,
                    provenance: GlyphProvenance {
                        content_stream: ObjectRef {
                            object_number: 1,
                            generation: 0,
                        },
                        operator_index: u32::try_from(next_id)
                            .expect("fixture glyph id fits in u32"),
                    },
                });
                next_id += 1;
                inline_offset += 6.0;
            }
        }
        Document::new(glyphs)
    }

    #[test]
    fn evaluate_candidate_visit_pressure_reports_upper_bound_and_dominance() {
        // Three blocks per side sharing the "aaa" n-gram: the upper bound is
        // 3 x 3 = 9 visits, exceeding a deliberately small budget.
        let old = glyph_document(&["aaaaaaefgh", "aaaaaaijkl", "aaaaaamnop"]);
        let new = glyph_document(&["aaaaaaqrst", "aaaaaauvwx", "aaaaaayzab"]);
        let options = PipelineOptions {
            alignment: AlignmentOptions {
                max_candidate_visits: 8,
                ..AlignmentOptions::default()
            },
            ..PipelineOptions::default()
        };

        let pressure =
            evaluate_candidate_visit_pressure(&old, &new, options).expect("pressure measures");

        assert_eq!(pressure.estimated_visits_upper_bound_total, 9);
        assert_eq!(pressure.max_candidate_visits, 8);
        assert!(pressure.estimated_visits_upper_bound_exceeds_limit);
        assert_eq!(pressure.dominant_ngram_visits, 9);
        assert_eq!(pressure.dominant_ngram_df, 3);
        // One shared n-gram carrying the whole charge.
        assert_eq!(pressure.shared_ngram_count, 1);
        assert_eq!(pressure.top_10_ngram_visits, 9);
        assert_eq!(pressure.ngrams_for_50_percent_visits, 1);
        assert_eq!(pressure.ngrams_for_90_percent_visits, 1);
        assert_eq!(pressure.shared_ngram_df_p50, 3);
        assert_eq!(pressure.shared_ngram_df_p95, 3);
        assert_eq!(pressure.shared_ngram_df_max, 3);
    }

    #[test]
    fn candidate_visit_pressure_charges_only_short_blocks_for_the_fallback() {
        // One short and one long old block against two short and one long
        // new block: the short query pays two fallback visits (the short
        // new blocks only) and the long query pays two shared n-gram
        // postings ("ta " and "eta"), so the upper bound is 4. The old
        // full-scan fallback would have charged three for the short query.
        let old = glyph_document(&["id", "alpha beta gamma"]);
        let new = glyph_document(&["ux", "vy", "delta epsilon zeta"]);

        let pressure = evaluate_candidate_visit_pressure(&old, &new, PipelineOptions::default())
            .expect("pressure measures");

        assert_eq!(pressure.estimated_visits_upper_bound_total, 4);
        assert_eq!(pressure.estimated_visits_max, 2);
    }

    #[test]
    fn evaluate_minhash_candidate_visit_pressure_measures_deterministic_budget() {
        let old = glyph_document(&["aaaaaaefgh", "aaaaaaijkl", "aaaaaamnop"]);
        let new = glyph_document(&["aaaaaaefgh", "aaaaaaijkl", "aaaaaamnop"]);
        let options = PipelineOptions {
            alignment: AlignmentOptions {
                max_candidate_visits: 1000,
                ..AlignmentOptions::default()
            },
            ..PipelineOptions::default()
        };

        let pressure = evaluate_minhash_candidate_visit_pressure(&old, &new, options)
            .expect("minhash pressure measures");

        assert!(pressure.estimated_visits_upper_bound_total > 0);
        assert_eq!(pressure.max_candidate_visits, 1000);
        assert!(!pressure.estimated_visits_upper_bound_exceeds_limit);
    }

    #[test]
    fn summarize_ngram_distribution_ranks_contributions_by_visits() {
        let contributions = [(6, 10), (3, 5), (3, 5), (2, 1), (1, 1)];
        let distribution = summarize_ngram_distribution(&contributions, 15);

        assert_eq!(distribution.shared_ngram_count, 5);
        // Fewer than ten contributors: the top-10 sum is the whole total.
        assert_eq!(distribution.top_10_ngram_visits, 15);
        // Descending visits [6, 3, 3, 2, 1]: target ceil(15/2)=8, prefixes
        // 6 (<8), then 9 (>=8).
        assert_eq!(distribution.ngrams_for_50_percent_visits, 2);
        // Target 15 - 1 = 14: prefixes 6, 9, 12, then 14 (>=14).
        assert_eq!(distribution.ngrams_for_90_percent_visits, 4);
        // Shared-set DFs sorted [1, 1, 5, 5, 10].
        assert_eq!(distribution.shared_ngram_df_p50, 5);
        assert_eq!(distribution.shared_ngram_df_p95, 10);
        assert_eq!(distribution.shared_ngram_df_max, 10);
    }

    #[test]
    fn summarize_ngram_distribution_is_tie_order_independent() {
        let mut reversed = [(6, 10), (3, 5), (3, 5), (2, 1), (1, 1)];
        reversed.reverse();

        assert_eq!(
            summarize_ngram_distribution(&[(6, 10), (3, 5), (3, 5), (2, 1), (1, 1)], 15),
            summarize_ngram_distribution(&reversed, 15)
        );
    }

    #[test]
    fn summarize_ngram_distribution_reaches_half_with_ceil_on_odd_total() {
        // Total 15 with a leading contribution of 7: the 50% target is
        // ceil(15/2)=8, so the first prefix (7) is not enough and n50 is 2.
        let contributions = [(7, 1), (5, 1), (3, 1)];
        let distribution = summarize_ngram_distribution(&contributions, 15);

        assert_eq!(distribution.ngrams_for_50_percent_visits, 2);
        // Target 15 - 1 = 14: prefixes 7, 12, then 15 (>=14).
        assert_eq!(distribution.ngrams_for_90_percent_visits, 3);
    }

    #[test]
    fn summarize_ngram_distribution_top_10_excludes_the_smallest_of_11_contributors() {
        // Eleven distinct contributors: the top-10 sum drops the smallest.
        let contributions = [
            (11, 1),
            (10, 1),
            (9, 1),
            (8, 1),
            (7, 1),
            (6, 1),
            (5, 1),
            (4, 1),
            (3, 1),
            (2, 1),
            (1, 1),
        ];
        let distribution = summarize_ngram_distribution(&contributions, 66);

        assert_eq!(distribution.shared_ngram_count, 11);
        assert_eq!(distribution.top_10_ngram_visits, 65);
    }

    /// A single-gram feature block; the gram is the only n-gram.
    fn ngram_feature(block: BlockId, gram: char) -> BlockFeatures {
        let token = ComparableToken::Scalar(gram);
        BlockFeatures {
            block,
            exact_hash: ExactHash(0),
            canonical_tokens: vec![token.clone()],
            matching_tokens: vec![token.clone()],
            ngrams: HashSet::from([NGram(vec![token])]),
            ngram_size: 1,
            numeric_mask_applied: false,
            has_normalization_issues: false,
        }
    }

    #[test]
    fn dominant_ngram_df_tie_prefers_larger_document_frequency() {
        // "a" has new-side df 3 with 2 old occurrences (6 visits); "b" has
        // new-side df 2 with 3 old occurrences (6 visits). The tied visits
        // must resolve to the larger df regardless of input order.
        let new = vec![
            ngram_feature(BlockId(10), 'a'),
            ngram_feature(BlockId(11), 'a'),
            ngram_feature(BlockId(12), 'a'),
            ngram_feature(BlockId(13), 'b'),
            ngram_feature(BlockId(14), 'b'),
        ];
        let old_forward = vec![
            ngram_feature(BlockId(0), 'a'),
            ngram_feature(BlockId(1), 'a'),
            ngram_feature(BlockId(2), 'b'),
            ngram_feature(BlockId(3), 'b'),
            ngram_feature(BlockId(4), 'b'),
        ];
        let old_reversed = vec![
            ngram_feature(BlockId(0), 'b'),
            ngram_feature(BlockId(1), 'b'),
            ngram_feature(BlockId(2), 'b'),
            ngram_feature(BlockId(3), 'a'),
            ngram_feature(BlockId(4), 'a'),
        ];
        let limit = AlignmentOptions::default().max_candidate_visits;

        let forward = ngram_visit_stats(&old_forward, &new, limit).expect("stats measure");
        let reversed = ngram_visit_stats(&old_reversed, &new, limit).expect("stats measure");

        assert_eq!(forward.total_posting_visits, 12);
        assert_eq!(forward.shared_ngram_count, 2);
        assert_eq!(forward.dominant_ngram_visits, 6);
        assert_eq!(forward.dominant_ngram_df, 3);
        assert_eq!(reversed.dominant_ngram_df, 3);
        assert_eq!(reversed.dominant_ngram_df, forward.dominant_ngram_df);
    }

    #[test]
    fn summarize_ngram_distribution_reports_zero_without_shared_ngrams() {
        let distribution = summarize_ngram_distribution(&[], 0);

        assert_eq!(distribution.shared_ngram_count, 0);
        assert_eq!(distribution.top_10_ngram_visits, 0);
        assert_eq!(distribution.ngrams_for_50_percent_visits, 0);
        assert_eq!(distribution.ngrams_for_90_percent_visits, 0);
        assert_eq!(distribution.shared_ngram_df_p50, 0);
        assert_eq!(distribution.shared_ngram_df_p95, 0);
        assert_eq!(distribution.shared_ngram_df_max, 0);
    }
}
