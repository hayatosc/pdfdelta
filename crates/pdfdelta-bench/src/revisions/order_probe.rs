//! Conditional reading-order comparison for externally captured model regions.
//!
//! Model regions are evidence for a bounded experiment only. The probe keeps
//! the ordinary extracted glyphs and normalized blocks as the source of truth,
//! and uses a model capture only to choose a conditional block order.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use pdfdelta_core::{
    alignment::{Alignment, InvertedIndexCandidateGenerator, align_ordered, build_block_features},
    diff::{Comparison, MatchedAtomicDiff, RecoveredAtomicDiff},
    layout::{reconstruct_blocks, reconstruct_lines},
    model::{Document, Glyph, GlyphCropStatus, GlyphPathClipStatus, Rect, TextRenderMode},
    normalize::{BlockText, TextSourceAtom, normalize_blocks},
    pdf::{LopdfParser, ParseLimits},
    pipeline::PipelineOptions,
    source::{
        ContentStreamGlyphExtractor, ExtractionIssueKind, ExtractionLimits, ExtractionOutcome,
        ExtractionScope, ParserBackedGlyphSource,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::revision_diagnostics::{ComparisonDiagnosticInput, evaluate_reviewed_diagnostics};
use super::{
    ActualChange, Annotation, ExpectedChangeDiagnostics, ExpectedChangeFailureReason,
    ExpectedDocument, QualityMetrics, ScopedEventMetrics, ScopedTokenMetrics,
    TokenResolutionCounts, build_block_map, compute_quality, evaluate_complete_scopes,
    flatten_actual_changes, flatten_candidate_changes, load_expected_document, match_changes,
    token_resolution_counts,
};
use crate::{
    BenchError, Result,
    evaluation::{hex_digest, sha256_hex},
};

const MAX_EXPECTED_BYTES: usize = 16 * 1024 * 1024;
const MAX_MODEL_BYTES: usize = 64 * 1024 * 1024;
const MAX_MODEL_PAGES: usize = 100_000;
const MAX_MODEL_REGIONS: usize = 1_000_000;

#[derive(Debug, Deserialize)]
struct ModelCapture {
    schema_version: u32,
    hypothesis_only: bool,
    source_sha256: String,
    pages: Vec<ModelPage>,
}

#[derive(Debug, Deserialize)]
struct ModelPage {
    page: u32,
    width: f64,
    height: f64,
    rotation: i32,
    crop_box: [f64; 4],
    regions: Vec<ModelRegion>,
}

#[derive(Debug, Deserialize)]
struct ModelRegion {
    rank: usize,
    bbox: [f64; 4],
    label: String,
    score: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrderKind {
    Native,
    Model,
}

impl OrderKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Model => "model",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct BlockBounds {
    page: u32,
    rect: Rect,
}

#[derive(Debug)]
struct BlockAssignment {
    order: Vec<usize>,
    regions: usize,
    strict_assignments: usize,
    center_fallbacks: usize,
    unassigned_regions: usize,
    ambiguous_regions: usize,
    unassigned_blocks: usize,
    ambiguous_blocks: usize,
    assigned_blocks: usize,
    fallback_blocks: usize,
    page_reports: Vec<PageOrderReport>,
}

#[derive(Debug, Serialize)]
pub struct OrderProbeReport {
    pub schema_version: u32,
    pub hypothesis_only: bool,
    pub limit_scale: f64,
    pub old_sha256: String,
    pub new_sha256: String,
    pub expected: Option<ExpectedSummary>,
    pub scope_fingerprint: Option<String>,
    pub extraction: ExtractionReport,
    pub old_model: Option<ModelSummary>,
    pub new_model: Option<ModelSummary>,
    pub old_assignment: Option<AssignmentReport>,
    pub new_assignment: Option<AssignmentReport>,
    pub controls: Vec<ControlReport>,
}

#[derive(Debug, Serialize)]
pub struct ExpectedSummary {
    pub pair: String,
    pub annotation: Annotation,
    pub changes: usize,
    pub scope_fingerprint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExtractionReport {
    pub old_complete: bool,
    pub new_complete: bool,
    pub issues: Vec<ExtractionIssueReport>,
}

#[derive(Debug, Serialize)]
pub struct ExtractionIssueReport {
    pub side: &'static str,
    pub kind: &'static str,
    pub scope: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct ModelSummary {
    pub source_sha256: String,
    pub schema_version: u32,
    pub hypothesis_only: bool,
    pub pages: usize,
    pub regions: usize,
}

#[derive(Debug, Serialize)]
pub struct AssignmentReport {
    pub blocks: usize,
    pub model_block_count: usize,
    pub model_unique_block_ids: bool,
    pub model_is_permutation: bool,
    pub regions: usize,
    pub assigned_blocks: usize,
    pub fallback_blocks: usize,
    pub strict_assignments: usize,
    pub center_fallbacks: usize,
    pub unassigned_regions: usize,
    pub ambiguous_regions: usize,
    pub unassigned_blocks: usize,
    pub ambiguous_blocks: usize,
    pub order_changed: bool,
    pub pages: Vec<PageOrderReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PageOrderReport {
    pub page: u32,
    pub native_block_ids: Vec<u64>,
    pub model_block_ids: Vec<u64>,
    pub changed: bool,
}

#[derive(Debug, Serialize)]
pub struct ControlReport {
    pub name: String,
    pub old_order: &'static str,
    pub new_order: &'static str,
    pub status: &'static str,
    pub runtime_ms: u64,
    pub alignment_spans: Option<usize>,
    pub changes: Option<usize>,
    pub candidate_changes: Option<usize>,
    pub unresolved_regions: Option<usize>,
    pub old_coverage: Option<TokenCoverageReport>,
    pub new_coverage: Option<TokenCoverageReport>,
    pub source_old_changed_tokens: Option<usize>,
    pub source_new_changed_tokens: Option<usize>,
    pub source_old_unchanged_tokens: Option<usize>,
    pub source_new_unchanged_tokens: Option<usize>,
    pub source_old_unresolved_tokens: Option<usize>,
    pub source_new_unresolved_tokens: Option<usize>,
    pub source_old_token_resolution: Option<TokenResolutionCounts>,
    pub source_new_token_resolution: Option<TokenResolutionCounts>,
    pub candidate_recall: Option<f64>,
    pub candidate_expected_outcomes: Option<Vec<ExpectedOutcomeReport>>,
    pub reviewed_scope_events: Option<ScopedEventMetrics>,
    pub reviewed_scope_tokens: Option<ScopedTokenMetrics>,
    pub reviewed_scope_error: Option<String>,
    pub quality: Option<QualityMetrics>,
    pub quality_error: Option<String>,
    pub expected_outcomes: Option<Vec<ExpectedOutcomeReport>>,
    pub failure_reason_counts: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<ExpectedChangeDiagnostics>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TokenCoverageReport {
    pub resolved_tokens: usize,
    pub total_tokens: usize,
    pub ratio: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct ExpectedOutcomeReport {
    pub id: String,
    pub matched: bool,
    pub actual_change_index: Option<usize>,
    pub failure_reason: Option<String>,
}

struct ProbeInput {
    old_path: PathBuf,
    new_path: PathBuf,
    expected_path: Option<PathBuf>,
    old_model_path: Option<PathBuf>,
    new_model_path: Option<PathBuf>,
    limit_scale: f64,
}

struct ModelOrder {
    blocks: Vec<BlockText>,
    assignment: AssignmentReport,
}

/// Parses the probe command line and runs the comparison.
///
/// The accepted form is `OLD.pdf NEW.pdf EXPECTED.json [OLD.model.json
/// NEW.model.json] [--limit-scale SCALE]`. Use `-` for no expected document.
pub fn run_from_args<I>(args: I) -> Result<OrderProbeReport>
where
    I: IntoIterator<Item = String>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let input = parse_args(&args)?;
    run(input)
}

fn parse_args(args: &[String]) -> Result<ProbeInput> {
    if args.len() < 3 {
        return Err(BenchError::InvalidInput(usage().to_owned()));
    }
    let old_path = PathBuf::from(&args[0]);
    let new_path = PathBuf::from(&args[1]);
    let expected_path = (args[2] != "-").then(|| PathBuf::from(&args[2]));
    let mut positional = Vec::new();
    let mut limit_scale = 1.0;
    let mut index = 3;
    while index < args.len() {
        let argument = &args[index];
        if let Some(value) = argument.strip_prefix("--limit-scale=") {
            limit_scale = parse_scale(value)?;
        } else if argument == "--limit-scale" {
            index += 1;
            let value = args.get(index).ok_or_else(|| {
                BenchError::InvalidInput("--limit-scale requires a value".to_owned())
            })?;
            limit_scale = parse_scale(value)?;
        } else if argument.starts_with('-') {
            return Err(BenchError::InvalidInput(format!(
                "unknown option {argument:?}; {}",
                usage()
            )));
        } else {
            positional.push(PathBuf::from(argument));
        }
        index += 1;
    }
    let (old_model_path, new_model_path) = match positional.as_slice() {
        [] => (None, None),
        [old, new] => (Some(old.clone()), Some(new.clone())),
        _ => {
            return Err(BenchError::InvalidInput(
                "model captures must be supplied as an old/new pair".to_owned(),
            ));
        }
    };
    Ok(ProbeInput {
        old_path,
        new_path,
        expected_path,
        old_model_path,
        new_model_path,
        limit_scale,
    })
}

const fn usage() -> &'static str {
    "usage: compare_order_hypotheses OLD.pdf NEW.pdf EXPECTED.json [OLD.model.json NEW.model.json] [--limit-scale SCALE]"
}

fn parse_scale(value: &str) -> Result<f64> {
    let scale = value.parse::<f64>().map_err(|_| {
        BenchError::InvalidInput(format!(
            "invalid limit scale {value:?}; expected a finite value >= 1"
        ))
    })?;
    if !scale.is_finite() || scale < 1.0 {
        return Err(BenchError::InvalidInput(format!(
            "invalid limit scale {scale}; expected a finite value >= 1"
        )));
    }
    Ok(scale)
}

fn run(input: ProbeInput) -> Result<OrderProbeReport> {
    let old_bytes = read_bounded(
        &input.old_path,
        ParseLimits::default().max_input_bytes,
        "old PDF",
    )?;
    let new_bytes = read_bounded(
        &input.new_path,
        ParseLimits::default().max_input_bytes,
        "new PDF",
    )?;
    let old_sha256 = sha256_hex(&old_bytes);
    let new_sha256 = sha256_hex(&new_bytes);
    let expected = input
        .expected_path
        .as_deref()
        .map(read_expected)
        .transpose()?;
    let old_model = input
        .old_model_path
        .as_deref()
        .map(|path| read_model(path, &old_sha256))
        .transpose()?;
    let new_model = input
        .new_model_path
        .as_deref()
        .map(|path| read_model(path, &new_sha256))
        .transpose()?;

    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let old_extraction = extract(&source, old_bytes);
    let new_extraction = extract(&source, new_bytes);
    let extraction = extraction_report(&old_extraction, &new_extraction);
    let options = PipelineOptions::default()
        .scaled_limits(input.limit_scale)
        .map_err(|source| BenchError::Core {
            stage: "limit scaling",
            source,
        })?;

    let complete = old_extraction.is_complete() && new_extraction.is_complete();
    let prepared = complete
        .then(|| {
            (
                prepare_blocks(&old_extraction, options),
                prepare_blocks(&new_extraction, options),
            )
        })
        .map(|(old, new)| Ok((old?, new?)))
        .transpose()?;
    let expected_summary = expected.as_ref().map(expected_summary);
    let scope_fingerprint_from_summary = expected_summary
        .as_ref()
        .and_then(|summary| summary.scope_fingerprint.clone());

    let (old_assignment, new_assignment, controls) =
        if let Some((prepared_old, prepared_new)) = prepared.as_ref().filter(|_| complete) {
            let base_old = prepared_old.as_slice();
            let base_new = prepared_new.as_slice();
            let assumed_native = compare_control(
                OrderKind::Native,
                OrderKind::Native,
                ControlBlocks {
                    ordered_old: base_old,
                    ordered_new: base_new,
                    source_old: base_old,
                    source_new: base_new,
                },
                options,
                expected.as_ref(),
            );
            if let (Some(old_model), Some(new_model)) = (old_model.as_ref(), new_model.as_ref()) {
                let old_order = build_model_order(base_old, old_extraction.document(), old_model)?;
                let new_order = build_model_order(base_new, new_extraction.document(), new_model)?;
                let assumed_model = compare_control(
                    OrderKind::Model,
                    OrderKind::Model,
                    ControlBlocks {
                        ordered_old: &old_order.blocks,
                        ordered_new: &new_order.blocks,
                        source_old: base_old,
                        source_new: base_new,
                    },
                    options,
                    expected.as_ref(),
                );
                (
                    Some(old_order.assignment),
                    Some(new_order.assignment),
                    vec![assumed_native, assumed_model],
                )
            } else {
                (None, None, vec![assumed_native])
            }
        } else {
            (
                None,
                None,
                suppressed_controls(old_model.is_some() && new_model.is_some()),
            )
        };
    Ok(OrderProbeReport {
        schema_version: 1,
        hypothesis_only: old_model.is_some() && new_model.is_some(),
        limit_scale: input.limit_scale,
        old_sha256,
        new_sha256,
        expected: expected_summary,
        scope_fingerprint: scope_fingerprint_from_summary,
        extraction,
        old_model: old_model.map(|capture| model_summary(&capture)),
        new_model: new_model.map(|capture| model_summary(&capture)),
        old_assignment,
        new_assignment,
        controls,
    })
}

fn read_bounded(path: &Path, max_bytes: usize, label: &str) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path).map_err(|error| {
        BenchError::InvalidInput(format!("cannot read {label} {}: {error}", path.display()))
    })?;
    if metadata.len() > max_bytes as u64 {
        return Err(BenchError::InvalidInput(format!(
            "{label} {} exceeds the {} byte limit",
            path.display(),
            max_bytes
        )));
    }
    // Enforce the ceiling on the read itself: the metadata check above can
    // race a growing file, and an oversized input must never be buffered in
    // full before the limit is noticed.
    let file = fs::File::open(path).map_err(|error| {
        BenchError::InvalidInput(format!("cannot read {label} {}: {error}", path.display()))
    })?;
    let mut bytes = Vec::new();
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            BenchError::InvalidInput(format!("cannot read {label} {}: {error}", path.display()))
        })?;
    if bytes.len() > max_bytes {
        return Err(BenchError::InvalidInput(format!(
            "{label} {} exceeds the {} byte limit",
            path.display(),
            max_bytes
        )));
    }
    Ok(bytes)
}

fn read_expected(path: &Path) -> Result<ExpectedDocument> {
    let bytes = read_bounded(path, MAX_EXPECTED_BYTES, "expected JSON")?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        BenchError::InvalidInput(format!(
            "expected JSON {} is not UTF-8: {error}",
            path.display()
        ))
    })?;
    load_expected_document(text)
}

fn read_model(path: &Path, source_sha256: &str) -> Result<ModelCapture> {
    let bytes = read_bounded(path, MAX_MODEL_BYTES, "model JSON")?;
    let capture: ModelCapture = serde_json::from_slice(&bytes).map_err(|error| {
        BenchError::InvalidInput(format!("invalid model JSON {}: {error}", path.display()))
    })?;
    validate_model(&capture, source_sha256, path)?;
    Ok(capture)
}

fn validate_model(capture: &ModelCapture, source_sha256: &str, path: &Path) -> Result<()> {
    if capture.schema_version != 1 || !capture.hypothesis_only {
        return Err(BenchError::InvalidInput(format!(
            "model JSON {} must use schema_version 1 and hypothesis_only true",
            path.display()
        )));
    }
    if capture.source_sha256 != source_sha256 {
        return Err(BenchError::InvalidInput(format!(
            "model JSON {} fingerprints a different PDF",
            path.display()
        )));
    }
    if capture.pages.len() > MAX_MODEL_PAGES {
        return Err(BenchError::InvalidInput(format!(
            "model JSON {} contains too many pages",
            path.display()
        )));
    }
    let mut pages = HashSet::with_capacity(capture.pages.len());
    let mut regions = 0usize;
    for page in &capture.pages {
        if !pages.insert(page.page)
            || !page.width.is_finite()
            || !page.height.is_finite()
            || page.width <= 0.0
            || page.height <= 0.0
            || !matches!(page.rotation, 0 | 90 | 180 | 270)
            || page.crop_box.iter().any(|value| !value.is_finite())
            || page.crop_box[0] >= page.crop_box[2]
            || page.crop_box[1] >= page.crop_box[3]
        {
            return Err(BenchError::InvalidInput(format!(
                "model JSON {} contains invalid or duplicate page metadata",
                path.display()
            )));
        }
        let mut ranks = HashSet::with_capacity(page.regions.len());
        regions = regions
            .checked_add(page.regions.len())
            .ok_or_else(|| BenchError::InvalidInput("model region count overflow".to_owned()))?;
        if regions > MAX_MODEL_REGIONS {
            return Err(BenchError::InvalidInput(format!(
                "model JSON {} contains too many regions",
                path.display()
            )));
        }
        for region in &page.regions {
            let [left, bottom, right, top] = region.bbox;
            if !ranks.insert(region.rank)
                || region.label.trim().is_empty()
                || !region.score.is_finite()
                || !(0.0..=1.0).contains(&region.score)
                || !left.is_finite()
                || !bottom.is_finite()
                || !right.is_finite()
                || !top.is_finite()
                || left >= right
                || bottom >= top
            {
                return Err(BenchError::InvalidInput(format!(
                    "model JSON {} contains invalid region metadata",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn extract(
    source: &ParserBackedGlyphSource<LopdfParser, ContentStreamGlyphExtractor>,
    bytes: Vec<u8>,
) -> ExtractionOutcome {
    match source.extract_outcome(
        Arc::from(bytes),
        ParseLimits::default(),
        ExtractionLimits::default(),
    ) {
        Ok(outcome) => outcome,
        Err(error) => ExtractionOutcome::new(
            Document::new(Vec::new()),
            vec![
                pdfdelta_core::source::ExtractionIssue::new(
                    ExtractionIssueKind::Unresolved,
                    ExtractionScope::Document,
                    format!("backend failure: {error}"),
                )
                .expect("backend failure description is nonblank"),
            ],
        )
        .expect("one document issue is a valid extraction outcome"),
    }
}

fn prepare_blocks(outcome: &ExtractionOutcome, options: PipelineOptions) -> Result<Vec<BlockText>> {
    let document = Document::with_vector_lines(
        outcome
            .document()
            .items()
            .iter()
            .filter(|glyph| is_comparison_visible(glyph))
            .cloned()
            .collect(),
        outcome.document().vector_lines().to_vec(),
    );
    let lines = reconstruct_lines(&document, options.line).map_err(|source| BenchError::Core {
        stage: "order probe line reconstruction",
        source,
    })?;
    let blocks = reconstruct_blocks(&document, &lines, options.block).map_err(|source| {
        BenchError::Core {
            stage: "order probe block reconstruction",
            source,
        }
    })?;
    normalize_blocks(&document, &lines, &blocks).map_err(|source| BenchError::Core {
        stage: "order probe normalization",
        source,
    })
}

fn is_comparison_visible(glyph: &Glyph) -> bool {
    matches!(
        glyph.render_mode,
        TextRenderMode::Fill
            | TextRenderMode::Stroke
            | TextRenderMode::FillAndStroke
            | TextRenderMode::FillAndClip
            | TextRenderMode::StrokeAndClip
            | TextRenderMode::FillStrokeAndClip
    ) && glyph.crop_status != GlyphCropStatus::Outside
        && glyph.path_clip_status != GlyphPathClipStatus::Outside
}

fn expected_summary(document: &ExpectedDocument) -> ExpectedSummary {
    ExpectedSummary {
        pair: document.pair.clone(),
        annotation: document.annotation,
        changes: document.changes.len(),
        scope_fingerprint: scope_fingerprint(document),
    }
}

fn scope_fingerprint(document: &ExpectedDocument) -> Option<String> {
    if document.scopes.is_empty() {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"pdfdelta-order-probe-scopes\0v1\0");
    for scope in &document.scopes {
        for value in [
            scope.id.as_str(),
            scope.completeness.map_or("unspecified", |_| "complete"),
            scope.old.start_quote.as_str(),
            scope.old.end_quote.as_str(),
            scope.new.start_quote.as_str(),
            scope.new.end_quote.as_str(),
        ] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        }
    }
    Some(hex_digest(digest.finalize().as_slice()))
}

fn model_summary(capture: &ModelCapture) -> ModelSummary {
    ModelSummary {
        source_sha256: capture.source_sha256.clone(),
        schema_version: capture.schema_version,
        hypothesis_only: capture.hypothesis_only,
        pages: capture.pages.len(),
        regions: capture.pages.iter().map(|page| page.regions.len()).sum(),
    }
}

fn extraction_report(old: &ExtractionOutcome, new: &ExtractionOutcome) -> ExtractionReport {
    let mut issues = Vec::with_capacity(old.issues().len() + new.issues().len());
    issues.extend(old.issues().iter().map(|issue| issue_report("old", issue)));
    issues.extend(new.issues().iter().map(|issue| issue_report("new", issue)));
    ExtractionReport {
        old_complete: old.is_complete(),
        new_complete: new.is_complete(),
        issues,
    }
}

fn issue_report(
    side: &'static str,
    issue: &pdfdelta_core::source::ExtractionIssue,
) -> ExtractionIssueReport {
    let kind = match issue.kind() {
        ExtractionIssueKind::Unsupported => "unsupported",
        ExtractionIssueKind::Unresolved => "unresolved",
    };
    let scope = match issue.scope() {
        ExtractionScope::Document => "document".to_owned(),
        ExtractionScope::Page(page) => format!("page {}", page.0),
        ExtractionScope::PageGap { retained_before } => {
            format!("page gap after {retained_before} retained pages")
        }
        ExtractionScope::GlyphGap { retained_before } => {
            format!("glyph gap after {retained_before} retained glyphs")
        }
        _ => "unknown".to_owned(),
    };
    ExtractionIssueReport {
        side,
        kind,
        scope,
        description: issue.description().to_owned(),
    }
}

fn build_model_order(
    blocks: &[BlockText],
    document: &Document<pdfdelta_core::model::Glyph>,
    capture: &ModelCapture,
) -> Result<ModelOrder> {
    let bounds = block_bounds(blocks, document)?;
    let assignment = assign_blocks(blocks, &bounds, capture);
    let reordered = assignment
        .order
        .iter()
        .map(|&index| blocks[index].clone())
        .collect();
    Ok(ModelOrder {
        blocks: reordered,
        assignment: assignment_report(blocks, &assignment),
    })
}

fn block_bounds(
    blocks: &[BlockText],
    document: &Document<pdfdelta_core::model::Glyph>,
) -> Result<Vec<Option<BlockBounds>>> {
    let glyphs = document
        .items()
        .iter()
        .map(|glyph| (glyph.id, glyph))
        .collect::<HashMap<_, _>>();
    let mut result = Vec::with_capacity(blocks.len());
    for block in blocks {
        let mut pages = HashMap::<u32, Rect>::new();
        for entry in &block.canonical.source_map {
            for atom in &entry.source.atoms {
                let TextSourceAtom::Glyph(glyph_id) = atom else {
                    continue;
                };
                let Some(glyph) = glyphs.get(glyph_id) else {
                    continue;
                };
                pages
                    .entry(glyph.page.0)
                    .and_modify(|rect| *rect = union_rect(*rect, glyph.bbox))
                    .or_insert(glyph.bbox);
            }
        }
        result.push(
            pages
                .into_iter()
                .min_by_key(|(page, _)| *page)
                .map(|(page, rect)| BlockBounds { page, rect }),
        );
    }
    Ok(result)
}

fn union_rect(left: Rect, right: Rect) -> Rect {
    Rect {
        min: pdfdelta_core::model::Vec2 {
            x: left.min.x.min(right.min.x),
            y: left.min.y.min(right.min.y),
        },
        max: pdfdelta_core::model::Vec2 {
            x: left.max.x.max(right.max.x),
            y: left.max.y.max(right.max.y),
        },
    }
}

fn assign_blocks(
    blocks: &[BlockText],
    bounds: &[Option<BlockBounds>],
    capture: &ModelCapture,
) -> BlockAssignment {
    let mut pages = capture.pages.iter().collect::<Vec<_>>();
    pages.sort_unstable_by_key(|page| page.page);
    let mut assignments = HashMap::<usize, (u32, usize)>::new();
    let mut assigned_regions = HashSet::<(u32, usize)>::new();
    let mut ambiguous_region_keys = HashSet::<(u32, usize)>::new();
    let mut strict_assignments = 0;
    let mut center_fallbacks = 0;
    let mut unassigned_blocks = 0;
    let mut ambiguous_blocks = 0;
    for (index, bound) in bounds.iter().enumerate() {
        let Some(bound) = bound else {
            unassigned_blocks += 1;
            continue;
        };
        let strict = candidate_regions(bound.page, bound.rect, capture, true);
        let (candidate, fallback) = if strict.len() == 1 {
            (strict[0], false)
        } else if strict.is_empty() {
            let centers = candidate_regions(bound.page, bound.rect, capture, false);
            if centers.len() == 1 {
                (centers[0], true)
            } else {
                if centers.is_empty() {
                    unassigned_blocks += 1;
                } else {
                    ambiguous_blocks += 1;
                    ambiguous_region_keys
                        .extend(centers.iter().copied().map(|rank| (bound.page, rank)));
                }
                continue;
            }
        } else {
            ambiguous_blocks += 1;
            ambiguous_region_keys.extend(strict.iter().copied().map(|rank| (bound.page, rank)));
            continue;
        };
        assignments.insert(index, (bound.page, candidate));
        assigned_regions.insert((bound.page, candidate));
        if fallback {
            center_fallbacks += 1;
        } else {
            strict_assignments += 1;
        }
    }
    let mut mapped = assignments
        .iter()
        .map(|(&index, &(page, rank))| (page, rank, index))
        .collect::<Vec<_>>();
    mapped.sort_unstable_by_key(|&(page, rank, index)| (page, rank, index));
    let unassigned_regions = pages
        .iter()
        .flat_map(|page| {
            page.regions
                .iter()
                .map(move |region| (page.page, region.rank))
        })
        .filter(|key| !assigned_regions.contains(key) && !ambiguous_region_keys.contains(key))
        .count();
    let ambiguous_regions = ambiguous_region_keys.len();
    let mut page_reports = Vec::new();
    let mut all_pages = bounds
        .iter()
        .filter_map(|bound| bound.map(|bound| bound.page))
        .collect::<Vec<_>>();
    all_pages.extend(pages.iter().map(|page| page.page));
    all_pages.sort_unstable();
    all_pages.dedup();
    let mut ordered = Vec::with_capacity(blocks.len());
    for &page in &all_pages {
        ordered.extend(
            mapped
                .iter()
                .filter(|&&(mapped_page, _, _)| mapped_page == page)
                .map(|&(_, _, index)| index),
        );
        ordered.extend((0..blocks.len()).filter(|&index| {
            bounds[index].is_some_and(|bound| bound.page == page)
                && !assignments.contains_key(&index)
        }));
    }
    ordered.extend(
        (0..blocks.len())
            .filter(|&index| bounds[index].is_none() && !assignments.contains_key(&index)),
    );
    let fallback_blocks = blocks.len().saturating_sub(assignments.len());
    for page in all_pages {
        let native = (0..blocks.len())
            .filter(|&index| bounds[index].is_some_and(|bound| bound.page == page))
            .collect::<Vec<_>>();
        let model = mapped
            .iter()
            .filter(|&&(mapped_page, _, _)| mapped_page == page)
            .map(|&(_, _, index)| index)
            .chain(
                native
                    .iter()
                    .copied()
                    .filter(|index| !assignments.contains_key(index)),
            )
            .collect::<Vec<_>>();
        page_reports.push(PageOrderReport {
            page,
            native_block_ids: native.iter().map(|&index| blocks[index].block.0).collect(),
            model_block_ids: model.iter().map(|&index| blocks[index].block.0).collect(),
            changed: native != model,
        });
    }
    BlockAssignment {
        order: ordered,
        regions: capture.pages.iter().map(|page| page.regions.len()).sum(),
        strict_assignments,
        center_fallbacks,
        unassigned_regions,
        ambiguous_regions,
        unassigned_blocks,
        ambiguous_blocks,
        assigned_blocks: assignments.len(),
        fallback_blocks,
        page_reports,
    }
}

fn candidate_regions(page: u32, block: Rect, capture: &ModelCapture, strict: bool) -> Vec<usize> {
    capture
        .pages
        .iter()
        .find(|model_page| model_page.page == page)
        .into_iter()
        .flat_map(|model_page| model_page.regions.iter())
        .filter_map(|region| {
            let region_rect = Rect {
                min: pdfdelta_core::model::Vec2 {
                    x: region.bbox[0],
                    y: region.bbox[1],
                },
                max: pdfdelta_core::model::Vec2 {
                    x: region.bbox[2],
                    y: region.bbox[3],
                },
            };
            let matches = if strict {
                contains_rect(region_rect, block)
            } else {
                contains_point(region_rect, center(block))
            };
            matches.then_some(region.rank)
        })
        .collect()
}

fn contains_rect(container: Rect, contained: Rect) -> bool {
    container.min.x <= contained.min.x
        && container.min.y <= contained.min.y
        && container.max.x >= contained.max.x
        && container.max.y >= contained.max.y
}

fn contains_point(container: Rect, point: pdfdelta_core::model::Vec2) -> bool {
    container.min.x <= point.x
        && point.x <= container.max.x
        && container.min.y <= point.y
        && point.y <= container.max.y
}

fn center(rect: Rect) -> pdfdelta_core::model::Vec2 {
    pdfdelta_core::model::Vec2 {
        x: f64::midpoint(rect.min.x, rect.max.x),
        y: f64::midpoint(rect.min.y, rect.max.y),
    }
}

fn assignment_report(blocks: &[BlockText], assignment: &BlockAssignment) -> AssignmentReport {
    let native = (0..blocks.len()).collect::<Vec<_>>();
    let native_ids = blocks.iter().map(|block| block.block.0).collect::<Vec<_>>();
    let model_ids = assignment
        .order
        .iter()
        .map(|&index| blocks[index].block.0)
        .collect::<Vec<_>>();
    let model_unique_block_ids = model_ids.iter().collect::<HashSet<_>>().len() == model_ids.len();
    let model_is_permutation = model_ids.len() == native_ids.len()
        && model_unique_block_ids
        && model_ids.iter().copied().collect::<HashSet<_>>()
            == native_ids.iter().copied().collect::<HashSet<_>>();
    AssignmentReport {
        blocks: blocks.len(),
        model_block_count: model_ids.len(),
        model_unique_block_ids,
        model_is_permutation,
        regions: assignment.regions,
        assigned_blocks: assignment.assigned_blocks,
        fallback_blocks: assignment.fallback_blocks,
        strict_assignments: assignment.strict_assignments,
        center_fallbacks: assignment.center_fallbacks,
        unassigned_regions: assignment.unassigned_regions,
        ambiguous_regions: assignment.ambiguous_regions,
        unassigned_blocks: assignment.unassigned_blocks,
        ambiguous_blocks: assignment.ambiguous_blocks,
        order_changed: native != assignment.order,
        pages: assignment.page_reports.clone(),
    }
}

fn suppressed_controls(model_available: bool) -> Vec<ControlReport> {
    let controls = if model_available {
        vec![
            ("native_native", OrderKind::Native, OrderKind::Native),
            ("model_model", OrderKind::Model, OrderKind::Model),
        ]
    } else {
        vec![("native_native", OrderKind::Native, OrderKind::Native)]
    };
    controls
        .into_iter()
        .map(|(name, old_order, new_order)| ControlReport {
            name: name.to_owned(),
            old_order: old_order.label(),
            new_order: new_order.label(),
            status: "suppressed_incomplete_extraction",
            runtime_ms: 0,
            alignment_spans: None,
            changes: None,
            candidate_changes: None,
            unresolved_regions: None,
            old_coverage: None,
            new_coverage: None,
            source_old_changed_tokens: None,
            source_new_changed_tokens: None,
            source_old_unchanged_tokens: None,
            source_new_unchanged_tokens: None,
            source_old_unresolved_tokens: None,
            source_new_unresolved_tokens: None,
            source_old_token_resolution: None,
            source_new_token_resolution: None,
            candidate_recall: None,
            candidate_expected_outcomes: None,
            reviewed_scope_events: None,
            reviewed_scope_tokens: None,
            reviewed_scope_error: None,
            quality: None,
            quality_error: None,
            expected_outcomes: None,
            failure_reason_counts: BTreeMap::new(),
            diagnostics: None,
            error: None,
        })
        .collect()
}

struct ControlBlocks<'a> {
    ordered_old: &'a [BlockText],
    ordered_new: &'a [BlockText],
    source_old: &'a [BlockText],
    source_new: &'a [BlockText],
}

fn compare_control(
    old_order: OrderKind,
    new_order: OrderKind,
    blocks: ControlBlocks<'_>,
    options: PipelineOptions,
    expected: Option<&ExpectedDocument>,
) -> ControlReport {
    let ControlBlocks {
        ordered_old,
        ordered_new,
        source_old,
        source_new,
    } = blocks;
    let started = Instant::now();
    let name = format!("{}_{}", old_order.label(), new_order.label());
    let old_features = match build_block_features(ordered_old, options.ngram_size) {
        Ok(features) => features,
        Err(error) => {
            return failed_control(
                name,
                old_order,
                new_order,
                elapsed_ms(started),
                error.to_string(),
            );
        }
    };
    let new_features = match build_block_features(ordered_new, options.ngram_size) {
        Ok(features) => features,
        Err(error) => {
            return failed_control(
                name,
                old_order,
                new_order,
                elapsed_ms(started),
                error.to_string(),
            );
        }
    };
    let generator = match InvertedIndexCandidateGenerator::new(&new_features) {
        Ok(generator) => generator,
        Err(error) => {
            return failed_control(
                name,
                old_order,
                new_order,
                elapsed_ms(started),
                error.to_string(),
            );
        }
    };
    let alignment = match align_ordered(&old_features, &new_features, &generator, options.alignment)
    {
        Ok(alignment) => alignment,
        Err(error) => {
            return failed_control(
                name,
                old_order,
                new_order,
                elapsed_ms(started),
                error.to_string(),
            );
        }
    };
    let diff = match pdfdelta_core::diff::compare_aligned_with_atomic_edits(
        ordered_old,
        ordered_new,
        &alignment,
        options.diff,
    ) {
        Ok(diff) => diff,
        Err(error) => {
            return failed_control(
                name,
                old_order,
                new_order,
                elapsed_ms(started),
                error.to_string(),
            );
        }
    };
    report_control(ControlEvidence {
        name: &name,
        old_order,
        new_order,
        comparison: &diff.comparison,
        alignment_spans: alignment.spans.len(),
        matched_atomic_diffs: &diff.matched_atomic_diffs,
        recovered_atomic_diffs: &[],
        source_old_blocks: source_old,
        source_new_blocks: source_new,
        expected,
        runtime_ms: elapsed_ms(started),
        alignment: Some(&alignment),
    })
}

fn failed_control(
    name: String,
    old_order: OrderKind,
    new_order: OrderKind,
    runtime_ms: u64,
    error: String,
) -> ControlReport {
    ControlReport {
        name,
        old_order: old_order.label(),
        new_order: new_order.label(),
        status: "failed",
        runtime_ms,
        alignment_spans: None,
        changes: None,
        candidate_changes: None,
        unresolved_regions: None,
        old_coverage: None,
        new_coverage: None,
        source_old_changed_tokens: None,
        source_new_changed_tokens: None,
        source_old_unchanged_tokens: None,
        source_new_unchanged_tokens: None,
        source_old_unresolved_tokens: None,
        source_new_unresolved_tokens: None,
        source_old_token_resolution: None,
        source_new_token_resolution: None,
        candidate_recall: None,
        candidate_expected_outcomes: None,
        reviewed_scope_events: None,
        reviewed_scope_tokens: None,
        reviewed_scope_error: None,
        quality: None,
        quality_error: None,
        expected_outcomes: None,
        failure_reason_counts: BTreeMap::new(),
        diagnostics: None,
        error: Some(error),
    }
}

struct ControlEvidence<'a> {
    name: &'a str,
    old_order: OrderKind,
    new_order: OrderKind,
    comparison: &'a Comparison,
    alignment_spans: usize,
    matched_atomic_diffs: &'a [MatchedAtomicDiff],
    recovered_atomic_diffs: &'a [RecoveredAtomicDiff],
    source_old_blocks: &'a [BlockText],
    source_new_blocks: &'a [BlockText],
    expected: Option<&'a ExpectedDocument>,
    runtime_ms: u64,
    alignment: Option<&'a Alignment>,
}

fn report_control(evidence: ControlEvidence<'_>) -> ControlReport {
    let ControlEvidence {
        name,
        old_order,
        new_order,
        comparison,
        alignment_spans,
        matched_atomic_diffs,
        recovered_atomic_diffs,
        source_old_blocks,
        source_new_blocks,
        expected,
        runtime_ms,
        alignment,
    } = evidence;
    let maps = [
        build_block_map(source_old_blocks),
        build_block_map(source_new_blocks),
    ];
    let actuals = flatten_actual_changes(
        comparison,
        [&maps[0], &maps[1]],
        matched_atomic_diffs,
        recovered_atomic_diffs,
    );
    let candidate_actuals =
        flatten_candidate_changes(&comparison.change_candidates, [&maps[0], &maps[1]]);
    let scope_evaluation = expected
        .filter(|document| {
            document.annotation == Annotation::ScopedComplete || !document.scopes.is_empty()
        })
        .map(|document| {
            evaluate_complete_scopes(
                document,
                comparison,
                source_old_blocks,
                source_new_blocks,
                &actuals,
                recovered_atomic_diffs,
            )
        });
    let (quality, quality_error) = match (expected, scope_evaluation.as_ref()) {
        (Some(document), Some(Ok(scope))) if document.annotation == Annotation::ScopedComplete => {
            (Some(scope.quality), None)
        }
        (Some(document), Some(Err(error))) if document.annotation == Annotation::ScopedComplete => {
            (None, Some(error.clone()))
        }
        (Some(document), _) => quality_for(
            document,
            comparison,
            source_old_blocks,
            source_new_blocks,
            &actuals,
            recovered_atomic_diffs,
        ),
        (None, _) => (None, None),
    };
    let (reviewed_scope_events, reviewed_scope_tokens, reviewed_scope_error) =
        match (expected, scope_evaluation.as_ref()) {
            (Some(document), Some(Ok(scope))) if !document.scopes.is_empty() => {
                (Some(scope.event_metrics), Some(scope.token_metrics), None)
            }
            (Some(document), Some(Err(error))) if !document.scopes.is_empty() => {
                (None, None, Some(error.clone()))
            }
            _ => (None, None, None),
        };
    let (expected_outcomes, failure_reason_counts, diagnostics) = expected
        .and_then(|document| {
            expected_diagnostics(
                document,
                comparison,
                source_old_blocks,
                source_new_blocks,
                &actuals,
                alignment,
            )
        })
        .map_or((None, BTreeMap::new(), None), |value| {
            (
                Some(value.outcomes),
                value.failure_reason_counts,
                Some(value.diagnostics),
            )
        });
    let (candidate_recall, candidate_expected_outcomes) = expected
        .map_or((None, None), |document| {
            candidate_match_summary(document, &candidate_actuals)
        });
    let (old_token_resolution, new_token_resolution) = source_token_resolution(comparison);
    ControlReport {
        name: name.to_owned(),
        old_order: old_order.label(),
        new_order: new_order.label(),
        status: "ok",
        runtime_ms,
        alignment_spans: Some(alignment_spans),
        changes: Some(comparison.changes.len()),
        candidate_changes: Some(comparison.change_candidates.len()),
        unresolved_regions: Some(comparison.unresolved_regions.len()),
        old_coverage: Some(TokenCoverageReport {
            resolved_tokens: comparison.old_coverage.resolved_tokens,
            total_tokens: comparison.old_coverage.total_tokens,
            ratio: comparison.old_coverage.ratio,
        }),
        new_coverage: Some(TokenCoverageReport {
            resolved_tokens: comparison.new_coverage.resolved_tokens,
            total_tokens: comparison.new_coverage.total_tokens,
            ratio: comparison.new_coverage.ratio,
        }),
        source_old_changed_tokens: old_token_resolution.map(|counts| counts.changed),
        source_new_changed_tokens: new_token_resolution.map(|counts| counts.changed),
        source_old_unchanged_tokens: old_token_resolution.map(|counts| counts.same),
        source_new_unchanged_tokens: new_token_resolution.map(|counts| counts.same),
        source_old_unresolved_tokens: old_token_resolution.map(|counts| counts.unresolved),
        source_new_unresolved_tokens: new_token_resolution.map(|counts| counts.unresolved),
        source_old_token_resolution: old_token_resolution,
        source_new_token_resolution: new_token_resolution,
        candidate_recall,
        candidate_expected_outcomes,
        reviewed_scope_events,
        reviewed_scope_tokens,
        reviewed_scope_error,
        quality,
        quality_error,
        expected_outcomes,
        failure_reason_counts,
        diagnostics,
        error: None,
    }
}

struct ExpectedDiagnosticsSummary {
    outcomes: Vec<ExpectedOutcomeReport>,
    failure_reason_counts: BTreeMap<String, usize>,
    diagnostics: ExpectedChangeDiagnostics,
}

fn expected_diagnostics(
    document: &ExpectedDocument,
    comparison: &Comparison,
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    actuals: &[ActualChange],
    alignment: Option<&Alignment>,
) -> Option<ExpectedDiagnosticsSummary> {
    let outcome = match_changes(&document.changes, actuals);
    let diagnostics = evaluate_reviewed_diagnostics(
        &document.changes,
        old_blocks,
        new_blocks,
        ComparisonDiagnosticInput {
            alignment,
            comparison,
            actual_scopes: None,
        },
        actuals,
        &outcome,
    )
    .ok()?;
    let mut failures = document
        .changes
        .iter()
        .filter_map(|change| {
            diagnostics
                .expected_change_diagnostics
                .failures
                .iter()
                .find(|failure| failure.expected_id == change.id)
        })
        .map(|failure| {
            (
                failure.expected_id.as_str(),
                failure_reason_name(&failure.reason),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut failure_reason_counts = BTreeMap::new();
    for reason in failures.values_mut() {
        *failure_reason_counts
            .entry((*reason).to_owned())
            .or_insert(0) += 1;
    }
    let outcomes = document
        .changes
        .iter()
        .enumerate()
        .map(|(index, change)| {
            let actual_change_index = outcome.claimed_actual_by_expected[index];
            ExpectedOutcomeReport {
                id: change.id.clone(),
                matched: actual_change_index.is_some(),
                actual_change_index,
                failure_reason: failures.remove(change.id.as_str()).map(str::to_owned),
            }
        })
        .collect();
    Some(ExpectedDiagnosticsSummary {
        outcomes,
        failure_reason_counts,
        diagnostics: diagnostics.expected_change_diagnostics,
    })
}

fn candidate_match_summary(
    document: &ExpectedDocument,
    candidate_actuals: &[ActualChange],
) -> (Option<f64>, Option<Vec<ExpectedOutcomeReport>>) {
    let expected = document
        .changes
        .iter()
        .filter(|change| {
            document.annotation != Annotation::ScopedComplete || change.scope.is_some()
        })
        .collect::<Vec<_>>();
    let expected_changes = expected.len();
    let outcome = match_changes(
        &expected
            .iter()
            .map(|change| (*change).clone())
            .collect::<Vec<_>>(),
        candidate_actuals,
    );
    let recall = (expected_changes > 0).then(|| outcome.matched as f64 / expected_changes as f64);
    let outcomes = expected
        .iter()
        .enumerate()
        .map(|(index, change)| ExpectedOutcomeReport {
            id: change.id.clone(),
            matched: outcome.claimed_actual_by_expected[index].is_some(),
            actual_change_index: outcome.claimed_actual_by_expected[index],
            failure_reason: None,
        })
        .collect();
    (recall, Some(outcomes))
}

fn source_token_resolution(
    comparison: &Comparison,
) -> (Option<TokenResolutionCounts>, Option<TokenResolutionCounts>) {
    comparison
        .assessment
        .as_ref()
        .map_or((None, None), |assessment| {
            (
                Some(token_resolution_counts(&assessment.old_resolution)),
                Some(token_resolution_counts(&assessment.new_resolution)),
            )
        })
}

fn failure_reason_name(reason: &ExpectedChangeFailureReason) -> &'static str {
    match reason {
        ExpectedChangeFailureReason::QuoteNotExtracted { .. } => "quote_not_extracted",
        ExpectedChangeFailureReason::UnitSegmentationFailure { .. } => "unit_segmentation_failure",
        ExpectedChangeFailureReason::CandidateNotGenerated => "candidate_not_generated",
        ExpectedChangeFailureReason::CandidateScoringRejected => "candidate_scoring_rejected",
        ExpectedChangeFailureReason::AlignmentAmbiguous => "alignment_ambiguous",
        ExpectedChangeFailureReason::AlignmentSpanMismatch => "alignment_span_mismatch",
        ExpectedChangeFailureReason::ReadingOrderUnresolved { .. } => "reading_order_unresolved",
        ExpectedChangeFailureReason::DiffEditDistanceExceeded => "diff_edit_distance_exceeded",
        ExpectedChangeFailureReason::DiffRejectedAsImplausible => "diff_rejected_as_implausible",
        ExpectedChangeFailureReason::WrongChangeKind { .. } => "wrong_change_kind",
        ExpectedChangeFailureReason::OccurrenceCountMismatch { .. } => "occurrence_count_mismatch",
        ExpectedChangeFailureReason::FragmentedAcrossHunks { .. } => "fragmented_across_hunks",
        ExpectedChangeFailureReason::ExpectationOutsideAlignmentObjective { .. } => {
            "expectation_outside_alignment_objective"
        }
        ExpectedChangeFailureReason::AlignmentOrCandidate { .. } => "alignment_or_candidate",
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn quality_for(
    document: &ExpectedDocument,
    comparison: &Comparison,
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    actuals: &[ActualChange],
    recovered_atomic_diffs: &[RecoveredAtomicDiff],
) -> (Option<QualityMetrics>, Option<String>) {
    if document.annotation == Annotation::ScopedComplete {
        match evaluate_complete_scopes(
            document,
            comparison,
            old_blocks,
            new_blocks,
            actuals,
            recovered_atomic_diffs,
        ) {
            Ok(scoped) => (Some(scoped.quality), None),
            Err(error) => (None, Some(error)),
        }
    } else {
        (
            Some(compute_quality(
                document.annotation,
                &document.changes,
                actuals,
            )),
            None,
        )
    }
}
