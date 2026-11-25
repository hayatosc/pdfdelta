//! Exact ground truth on real PDFs by page-tree surgery.
//!
//! Generated fixtures prove that the flow engine can handle known small edits.
//! Real documents have no exact truth, but structural page edits do: deleting
//! page *i* must change exactly that page's tokens, duplicating it must insert
//! exactly those tokens, swapping two pages must change no content, and a
//! document compared with itself must produce no change at all.
//!
//! The harness edits a real PDF with `lopdf` page-tree surgery only: page
//! objects and `/Kids` arrays move, `/Contents`, `/Resources`, and every
//! content stream stay byte-identical. Each edited document is then run through
//! the same extraction, reading-order choice, and diff pipeline as
//! `pdfdelta flow`, and the reported changed-token multiset is scored against
//! the page-token multiset of the untouched document.
//!
//! Reading-order hypotheses use cross-document evidence and furniture
//! detection uses page-count fractions, so editing one page can legitimately
//! expose instability on untouched pages. The record therefore also compares
//! the untouched side's per-page order choices and furniture with the identity
//! run, and reports them in the detail column for non-exact cases.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use lopdf::{Document as LopdfDocument, Object};
use pdfdelta_core::{
    model::{DecodedText, Document, Glyph, PageId},
    pdf::{LopdfParser, ParseLimits},
    source::{
        ContentStreamGlyphExtractor, ExtractionIssue, ExtractionLimits, ExtractionScope,
        ParserBackedGlyphSource,
    },
};
use pdfdelta_flow::{
    FlowChange, FlowDiff, FlowStream, FlowToken, FurnitureChange, FurnitureKind, FurnitureLine,
    OrderKind, PageOrderEvidence, flow_candidates_owned, flow_diff, flow_order_from_candidates,
    flow_tokens, furniture_changes,
};
use sha2::{Digest, Sha256};

use crate::{
    BenchError, Result,
    flow_edit::{self, TextEditPlan},
};

/// Largest page count that keeps the full per-document plan.
///
/// The full plan edits up to five deterministic pages (first, last, middle,
/// and two LCG picks) with delete, insert, swap, and duplicate. Larger
/// documents use a fixed five-case plan (identity plus one delete, insert,
/// swap, and duplicate at the middle page), so a document's cost is bounded by
/// its own flow time rather than by a shared wall-clock budget. This threshold
/// keeps the expected full corpus run under an hour; no case is skipped.
pub const FULL_PLAN_MAX_PAGES: u32 = 64;

/// A case slower than this is recorded as a failure, never skipped.
pub const DEFAULT_CASE_TIMEOUT_SECONDS: u64 = 600;

/// Maximum text-edit candidates validated on one selected page.
///
/// Validation re-extracts the edited document, so an unbounded search over
/// every digit on a large page would dominate the run. The first candidates
/// that pass the cheap glyph/operator/token checks are tried in document order;
/// the rest are counted as skipped.
const TEXT_EDIT_VALIDATION_ATTEMPTS: usize = 8;

/// Optional evidence output for the metamorphic run.
#[derive(Clone, Debug)]
pub struct MetamorphicOptions {
    /// When set, every constructed old/new PDF pair is written here so a
    /// finding can be reproduced with `pdfdelta flow`.
    pub dump_dir: Option<PathBuf>,
    /// A case whose wall time exceeds this is marked as a timeout failure.
    pub case_timeout: Duration,
    /// Up to this many single-token text-edit cases are planned per document.
    pub text_edits: usize,
    /// Up to this many page-merge cases are planned per document.
    pub merges: usize,
    /// Uniform scale for extraction limits. Large documents may need > 1.0.
    pub limit_scale: f64,
}

impl Default for MetamorphicOptions {
    fn default() -> Self {
        Self {
            dump_dir: None,
            case_timeout: Duration::from_secs(DEFAULT_CASE_TIMEOUT_SECONDS),
            text_edits: 0,
            merges: 0,
            limit_scale: 1.0,
        }
    }
}

/// One document's deterministic coverage plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedDocument {
    /// Input PDF file name.
    pub file: String,
    /// Page count from the page tree.
    pub pages: u32,
    /// Whether the full plan applies.
    pub full_plan: bool,
    /// Number of planned cases, including identity.
    pub cases: usize,
}

/// One constructed structural edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseKind {
    /// A document compared with itself.
    Identity,
    /// A page removed from the new side.
    Delete,
    /// A page removed from the old side.
    Insert,
    /// Two adjacent pages exchanged.
    Swap,
    /// A page repeated after itself.
    Duplicate,
    /// One digit inside one text-show string replaced by another digit.
    TextEdit,
    /// Two adjacent pages stacked into one taller page.
    Merge,
    /// One embedded font program rewritten without changing names or outlines.
    Resubset,
}

impl CaseKind {
    /// Stable lowercase name for the TSV.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Delete => "delete",
            Self::Insert => "insert",
            Self::Swap => "swap",
            Self::Duplicate => "duplicate",
            Self::TextEdit => "text-edit",
            Self::Merge => "merge",
            Self::Resubset => "resubset",
        }
    }
}

/// One evaluated case with multiset precision and recall against the truth.
#[derive(Clone, Debug)]
pub struct MetamorphicRecord {
    /// Input PDF file name.
    pub file: String,
    /// Constructed edit.
    pub case: String,
    /// 1-based page the edit applies to; 0 for identity.
    pub page: u32,
    /// Tokens that should be reported as changed.
    pub true_tokens: usize,
    /// Tokens the diff reports as changed (both sides of a replacement).
    pub reported_tokens: usize,
    /// Reported changed tokens also present in the truth multiset.
    pub correct_tokens: usize,
    /// `correct_tokens / reported_tokens`, or 1.0 when nothing was reported.
    pub precision: f64,
    /// `correct_tokens / true_tokens`, or 1.0 when nothing should change.
    pub recall: f64,
    /// All changed tokens matched the truth and only the expected change kind
    /// appeared.
    pub exact: bool,
    /// Number of move changes.
    pub moves: usize,
    /// Case wall time.
    pub seconds: f64,
    /// Classification evidence for non-exact cases.
    pub detail: String,
}

impl MetamorphicRecord {
    fn failure(file: &str, case: &str, page: u32, detail: String, seconds: f64) -> Self {
        Self {
            file: file.to_owned(),
            case: case.to_owned(),
            page,
            true_tokens: 0,
            reported_tokens: 0,
            correct_tokens: 0,
            precision: 0.0,
            recall: 0.0,
            exact: false,
            moves: 0,
            seconds,
            detail,
        }
    }
}

/// All evaluated cases plus micro totals.
#[derive(Clone, Debug, Default)]
pub struct MetamorphicSummary {
    /// One record per constructed edit that was evaluated.
    pub records: Vec<MetamorphicRecord>,
    /// Cases skipped because a document or the global budget ran out.
    pub skipped: usize,
    /// Harness skips by reason.
    pub skips: BTreeMap<String, usize>,
    /// Whether the global budget stopped the run early.
    pub stopped_early: bool,
    /// Total wall time.
    pub elapsed: Duration,
}

impl MetamorphicSummary {
    /// Number of evaluated cases.
    #[must_use]
    pub fn total(&self) -> usize {
        self.records.len()
    }

    /// Number of exactly matching cases.
    #[must_use]
    pub fn exact(&self) -> usize {
        self.records.iter().filter(|record| record.exact).count()
    }

    /// Micro precision over all reported changed tokens.
    #[must_use]
    pub fn micro_precision(&self) -> f64 {
        ratio(
            self.records
                .iter()
                .map(|record| record.correct_tokens)
                .sum(),
            self.records
                .iter()
                .map(|record| record.reported_tokens)
                .sum(),
        )
    }

    /// Micro recall over all truly changed tokens.
    #[must_use]
    pub fn micro_recall(&self) -> f64 {
        ratio(
            self.records
                .iter()
                .map(|record| record.correct_tokens)
                .sum(),
            self.records.iter().map(|record| record.true_tokens).sum(),
        )
    }
}

/// Writes the header, one TSV row per evaluated case, and a totals row.
///
/// # Errors
///
/// Returns the writer error when the TSV cannot be written.
pub fn write_tsv(summary: &MetamorphicSummary, writer: &mut impl Write) -> std::io::Result<()> {
    writeln!(
        writer,
        "file\tcase\tpage\ttrue_tokens\treported_tokens\tprecision\trecall\texact\tmoves\tseconds\tdetail"
    )?;
    for record in &summary.records {
        writeln!(
            writer,
            "{}\t{}\t{}\t{}\t{}\t{:.4}\t{:.4}\t{}\t{}\t{:.2}\t{}",
            tsv_field(&record.file),
            record.case,
            record.page,
            record.true_tokens,
            record.reported_tokens,
            record.precision,
            record.recall,
            record.exact,
            record.moves,
            record.seconds,
            tsv_field(&record.detail),
        )?;
    }
    writeln!(
        writer,
        "TOTAL\tcases={}\texact={}\tprecision={:.4}\trecall={:.4}\ttrue_tokens={}\treported_tokens={}\tcorrect_tokens={}\tskipped={}\tskip_reasons={}\telapsed_s={:.1}",
        summary.total(),
        summary.exact(),
        summary.micro_precision(),
        summary.micro_recall(),
        summary
            .records
            .iter()
            .map(|record| record.true_tokens)
            .sum::<usize>(),
        summary
            .records
            .iter()
            .map(|record| record.reported_tokens)
            .sum::<usize>(),
        summary
            .records
            .iter()
            .map(|record| record.correct_tokens)
            .sum::<usize>(),
        summary.skipped,
        summary
            .skips
            .iter()
            .map(|(reason, count)| format!("{reason}:{count}"))
            .collect::<Vec<_>>()
            .join(","),
        summary.elapsed.as_secs_f64(),
    )
}

/// Runs the metamorphic harness over every cached or explicitly named PDF.
///
/// When `files` is empty, every `*-old.pdf` and `*-new.pdf` directly under
/// `cache` is used, sorted by file name. The case plan is deterministic from
/// the page count and file name; every planned case runs. A case that exceeds
/// the case timeout or fails extraction is recorded as a non-exact failure,
/// never skipped.
///
/// # Errors
///
/// Returns an error when the input list is empty, a file cannot be read within
/// the parser input limit, or a directory cannot be listed. Per-document parse
/// and extraction failures become non-exact records instead of aborting the
/// run.
pub fn run(
    cache: &Path,
    files: &[PathBuf],
    options: &MetamorphicOptions,
) -> Result<MetamorphicSummary> {
    if let Some(directory) = &options.dump_dir {
        fs::create_dir_all(directory).map_err(|error| {
            BenchError::InvalidInput(format!(
                "cannot create dump directory {}: {error}",
                directory.display()
            ))
        })?;
    }
    let inputs = resolve_inputs(cache, files)?;
    let started = Instant::now();
    let mut summary = MetamorphicSummary::default();
    for path in inputs {
        let file = file_name(&path);
        let document_started = Instant::now();
        let records_before = summary.records.len();
        let result = run_document(&path, &file, options, &mut summary);
        if let Err(error) = result {
            summary.records.push(MetamorphicRecord::failure(
                &file,
                "identity",
                0,
                format!("document failed before any case: {error}"),
                document_started.elapsed().as_secs_f64(),
            ));
        }
        eprintln!(
            "flow-metamorphic: {file} {} cases in {:.1}s (total {:.1}s)",
            summary.records.len().saturating_sub(records_before),
            document_started.elapsed().as_secs_f64(),
            started.elapsed().as_secs_f64(),
        );
        summary.elapsed = started.elapsed();
    }
    summary.elapsed = started.elapsed();
    Ok(summary)
}

/// Prints the deterministic plan for every input without running a case.
///
/// # Errors
///
/// Returns an error when an input cannot be read, parsed, or listed.
pub fn plan_documents(
    cache: &Path,
    files: &[PathBuf],
    options: &MetamorphicOptions,
) -> Result<Vec<PlannedDocument>> {
    let inputs = resolve_inputs(cache, files)?;
    let mut plan = Vec::with_capacity(inputs.len());
    for path in inputs {
        let (bytes, page_count, parents) = load_input(&path)?;
        let mut skips = BTreeMap::new();
        let (cases, full_plan) = prepare_cases(
            &bytes,
            page_count,
            &parents,
            seed_from_name(&file_name(&path)),
            options,
            &mut skips,
        );
        plan.push(PlannedDocument {
            file: file_name(&path),
            pages: page_count,
            full_plan,
            cases: cases.len(),
        });
    }
    Ok(plan)
}

fn resolve_inputs(cache: &Path, files: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let inputs = if files.is_empty() {
        discover(cache)?
    } else {
        files.to_vec()
    };
    if inputs.is_empty() {
        return Err(BenchError::InvalidInput(format!(
            "no *-old.pdf or *-new.pdf found under {}",
            cache.display()
        )));
    }
    Ok(inputs)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Lists cache inputs in deterministic order.
fn discover(cache: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(cache).map_err(|error| {
        BenchError::InvalidInput(format!("cannot list {}: {error}", cache.display()))
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            BenchError::InvalidInput(format!("cannot list {}: {error}", cache.display()))
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.ends_with("-old.pdf") || name.ends_with("-new.pdf") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Loads one PDF's bytes, page count, and page-tree parents.
fn load_input(path: &Path) -> Result<(Vec<u8>, u32, Vec<Option<lopdf::ObjectId>>)> {
    let bytes = read_bounded_pdf(path)?;
    let document = LopdfDocument::load_mem(&bytes)
        .map_err(|error| invalid(format!("cannot parse {}: {error}", path.display())))?;
    let page_ids = document.get_pages();
    let page_count = u32::try_from(page_ids.len()).unwrap_or(u32::MAX);
    // Adjacent pages normally share a page-tree node. Swapping references
    // across nodes would change the inherited attributes of both pages, so
    // the swap pair is planned inside one node.
    let parents = (1..=page_count)
        .map(|page| {
            page_ids
                .get(&page)
                .and_then(|id| document.get_dictionary(*id).ok())
                .and_then(|dictionary| dictionary.get(b"Parent").ok())
                .and_then(|parent| parent.as_reference().ok())
        })
        .collect::<Vec<_>>();
    Ok((bytes, page_count, parents))
}

/// Runs every planned case for one PDF, appending records to `summary`.
fn run_document(
    path: &Path,
    file: &str,
    options: &MetamorphicOptions,
    summary: &mut MetamorphicSummary,
) -> Result<()> {
    let (bytes, page_count, parents) = load_input(path)?;
    let mut skips = BTreeMap::new();
    let (mut cases, full_plan) = prepare_cases(
        &bytes,
        page_count,
        &parents,
        seed_from_name(file),
        options,
        &mut skips,
    );
    let skipped = skips.values().copied().sum::<usize>();
    summary.skipped = summary.skipped.saturating_add(skipped);
    for (reason, count) in skips {
        *summary.skips.entry(reason).or_default() += count;
    }
    let identity_started = Instant::now();
    let identity = compare_pdf_bytes(bytes.clone(), bytes.clone(), options.limit_scale)?;
    let identity_seconds = identity_started.elapsed().as_secs_f64();
    let identity_record = timeout_record(
        identity_record(file, &identity, identity_seconds),
        options.case_timeout,
    );
    summary.records.push(identity_record);

    match resubset_plan(&identity) {
        ResubsetPlan::NoPlaceholders => {}
        ResubsetPlan::Skip(reason) => {
            summary.skipped = summary.skipped.saturating_add(1);
            *summary.skips.entry(reason).or_default() += 1;
        }
        ResubsetPlan::Eligible(page) => {
            cases.push(Case::structural(CaseKind::Resubset, page, 0));
        }
    }

    let pages = cases
        .iter()
        .filter(|case| case.kind != CaseKind::Identity)
        .map(|case| case.page)
        .collect::<Vec<_>>();
    eprintln!(
        "flow-metamorphic: {file} pages={page_count} plan={} cases={}",
        if full_plan { "full" } else { "reduced" },
        cases.len()
    );

    let truth = page_multisets(&identity.old.stream, &pages);
    for case in cases {
        if case.kind == CaseKind::Identity {
            continue;
        }
        let case_started = Instant::now();
        let page = case.page;
        let kind = case.kind.name();
        let outcome = match evaluate_case(
            file,
            case,
            &bytes,
            &identity,
            &truth,
            options.dump_dir.as_deref(),
            options.limit_scale,
        ) {
            Ok(outcome) => outcome,
            Err(error) => CaseOutcome::Record(MetamorphicRecord::failure(
                file,
                kind,
                page,
                error.to_string(),
                case_started.elapsed().as_secs_f64(),
            )),
        };
        match outcome {
            CaseOutcome::Record(record) => {
                summary
                    .records
                    .push(timeout_record(record, options.case_timeout));
            }
            CaseOutcome::Skip(reason) => {
                summary.skipped = summary.skipped.saturating_add(1);
                *summary.skips.entry(reason).or_default() += 1;
            }
        }
    }
    Ok(())
}

/// Marks a case that exceeded the generous timeout as a timeout failure.
fn timeout_record(mut record: MetamorphicRecord, case_timeout: Duration) -> MetamorphicRecord {
    if record.seconds > case_timeout.as_secs_f64() {
        record.exact = false;
        record.detail = format!(
            "timeout: case took {:.1}s (limit {:.0}s); {}",
            record.seconds,
            case_timeout.as_secs_f64(),
            record.detail
        );
    }
    record
}

/// One side of a completed flow comparison.
struct SideFlow {
    stream: FlowStream,
    pages: Vec<PageOrderEvidence>,
    complete: bool,
    unresolved: Vec<String>,
    /// Multiset of visible glyph identities (font, raw code, decoded text).
    /// Page ids are excluded so a merged page compares like-for-like.
    glyphs: Multiset,
}

/// Both sides, their verified diff, and furniture changes.
struct Comparison {
    old: SideFlow,
    new: SideFlow,
    diff: FlowDiff,
    furniture: Vec<FurnitureChange>,
}

/// Scales the count limits of a default extraction for a large document.
fn scaled_extraction_limits(scale: f64) -> ExtractionLimits {
    ExtractionLimits::default()
        .scaled(scale.max(1.0))
        .expect("a scale of at least 1.0 is valid")
}

/// Encoded image pages matched across the two documents.
#[derive(Default)]
struct ImageResolution {
    old_tokens: BTreeMap<PageId, String>,
    new_tokens: BTreeMap<PageId, String>,
    old_resolved: BTreeSet<PageId>,
    new_resolved: BTreeSet<PageId>,
}

/// Extracts, reconstructs reading order, resolves image-only pages, and diffs.
fn compare_pdf_bytes(
    old_bytes: Vec<u8>,
    new_bytes: Vec<u8>,
    limit_scale: f64,
) -> Result<Comparison> {
    let (old_document, old_issues) = extract_document(old_bytes.clone(), limit_scale)?;
    let (new_document, new_issues) = extract_document(new_bytes.clone(), limit_scale)?;
    let old_glyphs = glyph_multiset(&old_document);
    let new_glyphs = glyph_multiset(&new_document);
    let old_candidates = flow_candidates_owned(old_document)
        .map_err(|error| BenchError::core("flow candidates", error))?;
    let new_candidates = flow_candidates_owned(new_document)
        .map_err(|error| BenchError::core("flow candidates", error))?;
    let pair = flow_order_from_candidates(old_candidates, new_candidates)
        .map_err(|error| BenchError::core("flow order selection", error))?;
    let images = resolve_image_pages(&old_bytes, &new_bytes, &old_issues, &new_issues);
    let pdfdelta_flow::FlowOrderPair {
        mut old,
        mut new,
        old_pages,
        new_pages,
    } = pair;
    insert_image_tokens(&mut old, &images.old_tokens);
    insert_image_tokens(&mut new, &images.new_tokens);
    let old_unresolved = retained_issues(&old_issues, &images.old_resolved);
    let new_unresolved = retained_issues(&new_issues, &images.new_resolved);
    let v_diff = flow_diff(&old.tokens, &new.tokens);
    let diff = v_diff.map_err(|error| {
        BenchError::InvalidInput(format!("flow diff failed verification: {error}"))
    })?;
    let furniture = furniture_changes(&old, &new);
    Ok(Comparison {
        old: SideFlow {
            complete: old_unresolved.is_empty() && old.conservation.is_complete(),
            unresolved: old_unresolved,
            stream: old,
            pages: old_pages,
            glyphs: old_glyphs,
        },
        new: SideFlow {
            complete: new_unresolved.is_empty() && new.conservation.is_complete(),
            unresolved: new_unresolved,
            stream: new,
            pages: new_pages,
            glyphs: new_glyphs,
        },
        diff,
        furniture,
    })
}

/// Extracts one PDF and returns its non-fatal extraction issues.
fn extract_document(
    bytes: Vec<u8>,
    limit_scale: f64,
) -> Result<(Document<Glyph>, Vec<ExtractionIssue>)> {
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let outcome = source
        .extract_outcome(
            Arc::from(bytes),
            ParseLimits::default(),
            scaled_extraction_limits(limit_scale),
        )
        .map_err(|error| BenchError::core("metamorphic extraction", error))?;
    let (document, issues) = outcome.into_parts();
    Ok((document, issues))
}

/// Resolves image-only pages by an encoded page identity.
///
/// The harness never rewrites page content or resources, so hashing each page's
/// content streams, page box, and encoded image `XObject` streams is a
/// sufficient equality certificate for its constructed edits. This is a
/// harness-level identity, not the CLI's painted-image inventory: pages whose
/// image is only painted through a Form `XObject` stay unresolved instead of
/// being guessed. One-sided pages use their own identity, exactly like the CLI's
/// one-sided rule.
fn resolve_image_pages(
    old_bytes: &[u8],
    new_bytes: &[u8],
    old_issues: &[ExtractionIssue],
    new_issues: &[ExtractionIssue],
) -> ImageResolution {
    let old_images = image_page_identities(old_bytes, old_issues);
    let new_images = image_page_identities(new_bytes, new_issues);
    let mut used_new = vec![false; new_images.len()];
    let mut resolution = ImageResolution::default();
    for (old_page, old_hash) in &old_images {
        let matched = new_images
            .iter()
            .enumerate()
            .position(|(index, (_, new_hash))| !used_new[index] && new_hash == old_hash);
        let token = image_page_token(old_hash);
        if let Some(index) = matched {
            used_new[index] = true;
            let (new_page, _) = new_images[index];
            resolution.old_tokens.insert(*old_page, token.clone());
            resolution.new_tokens.insert(new_page, token);
            resolution.old_resolved.insert(*old_page);
            resolution.new_resolved.insert(new_page);
        } else {
            resolution.old_tokens.insert(*old_page, token);
            resolution.old_resolved.insert(*old_page);
        }
    }
    for (index, (new_page, new_hash)) in new_images.iter().enumerate() {
        if !used_new[index] {
            resolution
                .new_tokens
                .insert(*new_page, image_page_token(new_hash));
            resolution.new_resolved.insert(*new_page);
        }
    }
    resolution
}

/// Encoded identity for every image-only page that has one.
fn image_page_identities(bytes: &[u8], issues: &[ExtractionIssue]) -> Vec<(PageId, String)> {
    let mut pages = BTreeSet::new();
    for issue in issues {
        if !is_image_only_issue(issue) {
            continue;
        }
        if let Some(page) = issue_page(issue) {
            pages.insert(page);
        }
    }
    let mut identities = Vec::new();
    for page in pages {
        if let Some(hash) = page_image_identity(bytes, page.0 + 1) {
            identities.push((page, hash));
        }
    }
    identities
}

fn image_page_token(hash: &str) -> String {
    format!("\u{27e6}image-page:{hash}\u{27e7}")
}

/// Hashes the encoded content of one page without decoding any image.
fn page_image_identity(bytes: &[u8], page: u32) -> Option<String> {
    let document = LopdfDocument::load_mem(bytes).ok()?;
    let page_id = *document.get_pages().get(&page)?;
    let dictionary = document.get_dictionary(page_id).ok()?;
    let mut hash = Sha256::new();
    hash.update(b"flow-metamorphic-image-page-v1\0");
    for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate"] {
        if let Ok(value) = dictionary.get(key) {
            hash.update(format!("{key:?}={value:?}").as_bytes());
        }
    }
    let contents = dictionary.get(b"Contents").ok()?;
    hash_encoded_object(&document, contents, &mut hash)?;
    let resources = page_resources(&document, page_id)?;
    let xobjects = match resources.get(b"XObject").ok()? {
        Object::Reference(id) => document.get_dictionary(*id).ok()?,
        Object::Dictionary(dictionary) => dictionary,
        _ => return None,
    };
    let mut entries = xobjects
        .iter()
        .map(|(name, object)| (name.clone(), object.clone()))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut image_count = 0usize;
    for (name, object) in entries {
        let Object::Reference(id) = object else {
            continue;
        };
        let Ok(stream) = document.get_object(id).and_then(Object::as_stream) else {
            continue;
        };
        let is_image = stream
            .dict
            .get(b"Subtype")
            .ok()
            .and_then(|subtype| subtype.as_name().ok())
            == Some(b"Image");
        if !is_image {
            continue;
        }
        image_count += 1;
        hash.update(&name);
        hash.update(&stream.content);
        for key in [
            b"Width".as_slice(),
            b"Height",
            b"ColorSpace",
            b"BitsPerComponent",
            b"Filter",
            b"DecodeParms",
        ] {
            if let Ok(value) = stream.dict.get(key) {
                hash.update(format!("{key:?}={value:?}").as_bytes());
            }
        }
    }
    if image_count == 0 {
        return None;
    }
    Some(
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )
}

fn page_resources(
    document: &LopdfDocument,
    page_id: lopdf::ObjectId,
) -> Option<&lopdf::Dictionary> {
    let mut node = Some(page_id);
    while let Some(id) = node {
        let dictionary = document.get_dictionary(id).ok()?;
        if let Ok(resources) = dictionary.get(b"Resources") {
            return match resources {
                Object::Reference(id) => document.get_dictionary(*id).ok(),
                Object::Dictionary(dictionary) => Some(dictionary),
                _ => None,
            };
        }
        node = dictionary
            .get(b"Parent")
            .ok()
            .and_then(|parent| parent.as_reference().ok());
    }
    None
}

fn hash_encoded_object(document: &LopdfDocument, object: &Object, hash: &mut Sha256) -> Option<()> {
    match object {
        Object::Reference(id) => {
            let object = document.get_object(*id).ok()?;
            match object {
                Object::Stream(stream) => {
                    hash.update(&stream.content);
                    Some(())
                }
                Object::Array(_) => hash_encoded_object(document, object, hash),
                _ => None,
            }
        }
        Object::Array(array) => {
            for entry in array {
                hash_encoded_object(document, entry, hash)?;
            }
            Some(())
        }
        _ => None,
    }
}

fn is_image_only_issue(issue: &ExtractionIssue) -> bool {
    issue
        .description()
        .starts_with("page has a potentially visible image and no comparison-visible native glyphs")
}

fn issue_page(issue: &ExtractionIssue) -> Option<PageId> {
    match issue.scope() {
        ExtractionScope::Page(page) | ExtractionScope::PageGlyphGap { page, .. } => Some(page),
        _ => None,
    }
}

/// Issue descriptions that remain after resolving image-only pages.
fn retained_issues(issues: &[ExtractionIssue], resolved: &BTreeSet<PageId>) -> Vec<String> {
    issues
        .iter()
        .filter(|issue| {
            !is_image_only_issue(issue)
                || issue_page(issue).is_none_or(|page| !resolved.contains(&page))
        })
        .map(|issue| issue.description().to_owned())
        .collect()
}

/// Inserts one synthetic token for a page that had no text tokens.
fn insert_image_tokens(stream: &mut FlowStream, tokens: &BTreeMap<PageId, String>) {
    for (page, text) in tokens {
        if stream.tokens.iter().any(|token| token.page == *page) {
            continue;
        }
        let position = stream.tokens.partition_point(|token| token.page.0 < page.0);
        stream.tokens.insert(
            position,
            FlowToken {
                text: text.clone(),
                page: *page,
                bounds: None,
                block: 0,
                uncertain: false,
                line_break_after: false,
                font_size: None,
                raw: None,
                position: None,
                page_break_after: false,
                normalization: 0,
            },
        );
    }
}

/// Evaluates the identity case, where no change of any kind is expected.
fn identity_record(file: &str, comparison: &Comparison, seconds: f64) -> MetamorphicRecord {
    let stats = comparison.diff.stats();
    let reported = reported_tokens(
        &comparison.diff,
        &comparison.old.stream,
        &comparison.new.stream,
    );
    let exact =
        comparison.diff.changes.is_empty() && comparison.old.complete && comparison.new.complete;
    let mut detail = format!(
        "delete={}/{}t insert={}/{}t replace={}/{}t move={}/{}t layout={} furniture={} extraction_complete={}/{}",
        stats.delete.changes,
        stats.delete.old_tokens,
        stats.insert.changes,
        stats.insert.new_tokens,
        stats.replace.changes,
        stats.replace.old_tokens,
        stats.moves.changes,
        stats.moves.old_tokens,
        stats.layout_only.changes,
        comparison.furniture.len(),
        comparison.old.complete,
        comparison.new.complete,
    );
    if !comparison.old.unresolved.is_empty() {
        detail.push_str(" unresolved_old=");
        detail.push_str(&format_texts(
            comparison
                .old
                .unresolved
                .iter()
                .map(String::as_str)
                .collect(),
            3,
        ));
    }
    if !comparison.new.unresolved.is_empty() {
        detail.push_str(" unresolved_new=");
        detail.push_str(&format_texts(
            comparison
                .new
                .unresolved
                .iter()
                .map(String::as_str)
                .collect(),
            3,
        ));
    }
    if !exact {
        detail.push_str(" sample=");
        detail.push_str(&format_texts(reported.texts(), 8));
    }
    MetamorphicRecord {
        file: file.to_owned(),
        case: CaseKind::Identity.name().to_owned(),
        page: 0,
        true_tokens: 0,
        reported_tokens: reported.total(),
        correct_tokens: 0,
        precision: ratio(0, reported.total()),
        recall: 1.0,
        exact,
        moves: stats.moves.changes,
        seconds,
        detail,
    }
}

/// One constructed page edit.
#[derive(Clone, Debug)]
struct Case {
    kind: CaseKind,
    page: u32,
    right: u32,
    text_edit: Option<TextEditPlan>,
}

impl Case {
    fn structural(kind: CaseKind, page: u32, right: u32) -> Self {
        Self {
            kind,
            page,
            right,
            text_edit: None,
        }
    }
}

/// Builds the structural plan plus any selected text-edit and merge cases.
fn prepare_cases(
    bytes: &[u8],
    page_count: u32,
    parents: &[Option<lopdf::ObjectId>],
    seed: u64,
    options: &MetamorphicOptions,
    skips: &mut BTreeMap<String, usize>,
) -> (Vec<Case>, bool) {
    let (mut cases, full_plan) = plan_cases(page_count, parents, seed);
    cases.extend(text_edit_cases(
        bytes,
        options.text_edits,
        options.limit_scale,
        skips,
    ));
    cases.extend(merge_cases(bytes, page_count, options.merges, skips));
    (cases, full_plan)
}

/// Returns the deterministic plan and whether the full tier applies.
fn plan_cases(
    page_count: u32,
    parents: &[Option<lopdf::ObjectId>],
    seed: u64,
) -> (Vec<Case>, bool) {
    if page_count <= FULL_PLAN_MAX_PAGES {
        let pages = chosen_pages(page_count, seed);
        (full_cases_for(&pages, page_count, parents), true)
    } else {
        (reduced_cases_for(page_count, parents), false)
    }
}

/// Full plan: identity plus every chosen page's structural edits.
fn full_cases_for(
    pages: &[u32],
    page_count: u32,
    parents: &[Option<lopdf::ObjectId>],
) -> Vec<Case> {
    let mut cases = vec![Case::structural(CaseKind::Identity, 0, 0)];
    for &page in pages {
        cases.push(Case::structural(CaseKind::Delete, page, 0));
        cases.push(Case::structural(CaseKind::Insert, page, 0));
        if let Some((left, right)) = swap_pair(page, page_count, parents) {
            cases.push(Case::structural(CaseKind::Swap, left, right));
        }
        cases.push(Case::structural(CaseKind::Duplicate, page, 0));
    }
    cases
}

/// Reduced plan for large documents: one deterministic page, four edits.
fn reduced_cases_for(page_count: u32, parents: &[Option<lopdf::ObjectId>]) -> Vec<Case> {
    let page = page_count.div_ceil(2).max(1);
    let mut cases = vec![
        Case::structural(CaseKind::Identity, 0, 0),
        Case::structural(CaseKind::Delete, page, 0),
        Case::structural(CaseKind::Insert, page, 0),
    ];
    if let Some((left, right)) =
        swap_pair(page, page_count, parents).or_else(|| find_swap_pair(page_count, parents))
    {
        cases.push(Case::structural(CaseKind::Swap, left, right));
    }
    cases.push(Case::structural(CaseKind::Duplicate, page, 0));
    cases
}

/// First adjacent same-parent pair, used when the middle page has none.
fn find_swap_pair(page_count: u32, parents: &[Option<lopdf::ObjectId>]) -> Option<(u32, u32)> {
    (1..page_count).find_map(|page| swap_pair(page, page_count, parents))
}

/// Picks an adjacent page pair around `page` that shares one page-tree node.
fn swap_pair(
    page: u32,
    page_count: u32,
    parents: &[Option<lopdf::ObjectId>],
) -> Option<(u32, u32)> {
    let parent = |page: u32| parents.get(page as usize - 1).copied().flatten();
    let same_parent =
        |left: u32, right: u32| parent(left).is_some() && parent(left) == parent(right);
    if page < page_count && same_parent(page, page + 1) {
        return Some((page, page + 1));
    }
    if page > 1 && same_parent(page - 1, page) {
        return Some((page - 1, page));
    }
    None
}

/// Deterministic pages edited in every document with at least four pages.
///
/// The first, last, and middle pages are always included; two more come from a
/// fixed LCG seeded by the file name. The result is sorted and deduplicated.
fn chosen_pages(page_count: u32, seed: u64) -> Vec<u32> {
    if page_count < 4 {
        return Vec::new();
    }
    let mut pages = vec![1, page_count, page_count.div_ceil(2)];
    let mut state = seed;
    for _ in 0..2 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        pages.push(u32::try_from(state % u64::from(page_count)).unwrap_or(0) + 1);
    }
    pages.sort_unstable();
    pages.dedup();
    pages
}

/// FNV-1a over the file name, used only to seed the page LCG.
fn seed_from_name(name: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Adds up to `limit` single-token text-edit cases for one document.
///
/// A candidate must be a mapped single-byte ASCII digit glyph, live in a
/// direct page content stream that the scanner can rewrite, have another digit
/// code used by the same font, and fall inside exactly one flow token. The
/// edited document is extracted independently and the glyph sequences must
/// differ in exactly that glyph; otherwise the candidate is a harness skip.
fn text_edit_cases(
    bytes: &[u8],
    limit: usize,
    limit_scale: f64,
    skips: &mut BTreeMap<String, usize>,
) -> Vec<Case> {
    if limit == 0 {
        return Vec::new();
    }
    let (document, _issues) = match extract_document(bytes.to_vec(), limit_scale) {
        Ok(value) => value,
        Err(error) => {
            add_skip(skips, &format!("text-edit: extraction failed: {error}"));
            return Vec::new();
        }
    };
    let pdf = match LopdfDocument::load_mem(bytes) {
        Ok(pdf) => pdf,
        Err(error) => {
            add_skip(skips, &format!("text-edit: PDF parse failed: {error}"));
            return Vec::new();
        }
    };
    let flow = match flow_tokens(&document) {
        Ok(flow) => flow,
        Err(error) => {
            add_skip(skips, &format!("text-edit: flow tokens failed: {error}"));
            return Vec::new();
        }
    };
    let mut tokens_by_page: BTreeMap<u32, Vec<&FlowToken>> = BTreeMap::new();
    for token in &flow.tokens {
        tokens_by_page.entry(token.page.0).or_default().push(token);
    }
    let items = document.items();
    let mut groups: HashMap<(pdfdelta_core::pdf::ObjectRef, u32), Vec<usize>> = HashMap::new();
    let mut digit_codes: HashMap<u32, BTreeSet<(char, u8)>> = HashMap::new();
    for (index, glyph) in items.iter().enumerate() {
        groups
            .entry((
                glyph.provenance.content_stream,
                glyph.provenance.operator_index,
            ))
            .or_default()
            .push(index);
        let DecodedText::Mapped(text) = &glyph.text else {
            continue;
        };
        let mut chars = text.chars();
        let (Some(digit), None) = (chars.next(), chars.next()) else {
            continue;
        };
        if digit.is_ascii_digit() && glyph.raw_code.len() == 1 {
            digit_codes
                .entry(glyph.font_id.0)
                .or_default()
                .insert((digit, glyph.raw_code[0]));
        }
    }
    let mut shows_cache: HashMap<
        pdfdelta_core::pdf::ObjectRef,
        std::result::Result<Vec<flow_edit::ShowStrings>, String>,
    > = HashMap::new();
    let mut page_plans: BTreeMap<u32, Vec<TextEditPlan>> = BTreeMap::new();
    for (index, glyph) in items.iter().enumerate() {
        let DecodedText::Mapped(text) = &glyph.text else {
            continue;
        };
        let mut chars = text.chars();
        let (Some(old_digit), None) = (chars.next(), chars.next()) else {
            continue;
        };
        if !old_digit.is_ascii_digit() || glyph.raw_code.len() != 1 {
            continue;
        }
        let key = (
            glyph.provenance.content_stream,
            glyph.provenance.operator_index,
        );
        let Some(group) = groups.get(&key) else {
            continue;
        };
        if group.iter().any(|index| items[*index].raw_code.len() != 1) {
            continue;
        }
        let Some(position) = group.iter().position(|candidate| *candidate == index) else {
            continue;
        };
        let shows = shows_cache.entry(key.0).or_insert_with(|| {
            let object = pdf
                .get_object((key.0.object_number, key.0.generation))
                .map_err(|error| format!("cannot read content stream: {error}"))?;
            let stream = object
                .as_stream()
                .map_err(|error| format!("content object is not a stream: {error}"))?;
            let content = stream
                .get_plain_content_with_limit(64 * 1024 * 1024)
                .map_err(|error| format!("cannot decode content stream: {error}"))?;
            flow_edit::parse_show_strings(&content)
        });
        let Ok(shows) = shows else {
            continue;
        };
        let Some(show) = shows.iter().find(|show| show.operator_index == key.1) else {
            continue;
        };
        let mut flat = Vec::new();
        for (string_index, (_, bytes)) in show.strings.iter().enumerate() {
            for (offset, code) in bytes.iter().enumerate() {
                flat.push((string_index, offset, *code));
            }
        }
        if flat.len() != group.len() || flat[position].2 != glyph.raw_code[0] {
            continue;
        }
        let Some((new_digit, new_code)) = digit_codes
            .get(&glyph.font_id.0)
            .and_then(|codes| codes.iter().find(|(digit, _)| *digit != old_digit).copied())
        else {
            continue;
        };
        let (string_index, offset, _) = flat[position];
        let (span, decoded) = &show.strings[string_index];
        let mut edited = decoded.clone();
        edited[offset] = new_code;
        let replacement = flow_edit::hex_literal(&edited);
        let Ok(token) = digit_token(
            tokens_by_page.get(&glyph.page.0).map_or(&[], Vec::as_slice),
            glyph,
            old_digit,
        ) else {
            continue;
        };
        let old_token = token.text.clone();
        let new_token = old_token.replacen(old_digit, &new_digit.to_string(), 1);
        page_plans
            .entry(glyph.page.0)
            .or_default()
            .push(TextEditPlan {
                page: glyph.page.0,
                stream: (key.0.object_number, key.0.generation),
                span: span.clone(),
                replacement,
                old_digit,
                old_code: glyph.raw_code[0],
                new_digit,
                new_code,
                old_token,
                new_token,
            });
    }
    if page_plans.is_empty() {
        add_skip(skips, "text-edit: no eligible candidate page");
        return Vec::new();
    }
    let pages = page_plans.keys().copied().collect::<Vec<_>>();
    let selected = evenly_spaced(&pages, limit);
    let mut cases = Vec::new();
    for page in selected {
        let Some(plans) = page_plans.get(&page) else {
            continue;
        };
        let mut valid = false;
        for plan in plans.iter().take(TEXT_EDIT_VALIDATION_ATTEMPTS) {
            match validate_text_edit(bytes, &document, plan, limit_scale) {
                Ok(()) => {
                    cases.push(Case {
                        kind: CaseKind::TextEdit,
                        page: page + 1,
                        right: 0,
                        text_edit: Some(plan.clone()),
                    });
                    valid = true;
                    break;
                }
                Err(reason) => add_skip(skips, &format!("text-edit: {reason}")),
            }
        }
        if !valid && plans.len() > TEXT_EDIT_VALIDATION_ATTEMPTS {
            add_skip(skips, "text-edit: validation attempt budget exhausted");
        }
    }
    cases
}

/// Chooses up to `limit` entries spread over `values` (first, middle, last).
fn evenly_spaced(values: &[u32], limit: usize) -> Vec<u32> {
    if limit == 0 || values.is_empty() {
        return Vec::new();
    }
    if limit == 1 {
        return vec![values[0]];
    }
    let mut chosen = Vec::new();
    for index in 0..limit {
        let position = index * (values.len() - 1) / (limit - 1);
        let value = values[position];
        if chosen.last() != Some(&value) {
            chosen.push(value);
        }
    }
    chosen
}

fn digit_token<'a>(
    tokens: &[&'a FlowToken],
    glyph: &Glyph,
    digit: char,
) -> std::result::Result<&'a FlowToken, &'static str> {
    // The glyph must lie wholly inside the token's source bounds. Choosing the
    // smallest containing token prevents a neighbouring or overlapping line
    // that also contains the digit from standing in for the edited glyph.
    let mut candidates = tokens
        .iter()
        .filter_map(|token| {
            let bounds = token.bounds?;
            if glyph.bbox.min.x < bounds.min.x
                || glyph.bbox.max.x > bounds.max.x
                || glyph.bbox.min.y < bounds.min.y
                || glyph.bbox.max.y > bounds.max.y
                || token.text.matches(digit).count() != 1
            {
                return None;
            }
            let area = (bounds.max.x - bounds.min.x) * (bounds.max.y - bounds.min.y);
            Some((area, token.text.as_str(), *token))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.total_cmp(&right.0).then_with(|| left.1.cmp(right.1)));
    let Some(&(area, _, token)) = candidates.first() else {
        return Err("no unique token");
    };
    if candidates
        .get(1)
        .is_some_and(|(next_area, _, _)| *next_area <= area)
    {
        return Err("ambiguous token");
    }
    Ok(token)
}

/// Re-extracts the edited PDF and requires exactly the expected glyph change.
fn validate_text_edit(
    original: &[u8],
    document: &Document<Glyph>,
    plan: &TextEditPlan,
    limit_scale: f64,
) -> std::result::Result<(), String> {
    let mut pdf = LopdfDocument::load_mem(original)
        .map_err(|error| format!("cannot parse PDF for text edit: {error}"))?;
    flow_edit::apply_text_edit(&mut pdf, plan)?;
    let mut edited = Vec::new();
    pdf.save_to(&mut edited)
        .map_err(|error| format!("cannot serialize text edit: {error}"))?;
    let (edited_document, _) = extract_document(edited, limit_scale)
        .map_err(|error| format!("cannot extract text edit: {error}"))?;
    let original_items = document.items();
    let edited_items = edited_document.items();
    if original_items.len() != edited_items.len() {
        return Err("edited glyph count changed".to_owned());
    }
    let mut change = None;
    for (index, (before, after)) in original_items.iter().zip(edited_items).enumerate() {
        if before.page != after.page
            || before.raw_code != after.raw_code
            || before.text != after.text
        {
            if change.is_some() {
                return Err("more than one glyph changed".to_owned());
            }
            change = Some(index);
        }
    }
    let index = change.ok_or("no glyph changed")?;
    let before = &original_items[index];
    let after = &edited_items[index];
    let expected_before = DecodedText::Mapped(plan.old_digit.to_string());
    let expected_after = DecodedText::Mapped(plan.new_digit.to_string());
    if before.raw_code != [plan.old_code]
        || after.raw_code != [plan.new_code]
        || before.text != expected_before
        || after.text != expected_after
    {
        return Err("edited glyph does not match the planned digit".to_owned());
    }
    Ok(())
}

/// Builds up to `limit` page-merge cases for one document.
fn merge_cases(
    bytes: &[u8],
    page_count: u32,
    limit: usize,
    skips: &mut BTreeMap<String, usize>,
) -> Vec<Case> {
    if limit == 0 {
        return Vec::new();
    }
    let pdf = match LopdfDocument::load_mem(bytes) {
        Ok(pdf) => pdf,
        Err(error) => {
            add_skip(skips, &format!("merge: PDF parse failed: {error}"));
            return Vec::new();
        }
    };
    let mut eligible = Vec::new();
    for left in 1..page_count {
        if flow_edit::merge_eligible(&pdf, left, left + 1).is_ok() {
            eligible.push(left);
        }
    }
    if eligible.is_empty() {
        add_skip(skips, "merge: no eligible adjacent pair");
        return Vec::new();
    }
    let mut selected = vec![eligible[0]];
    if limit > 1 {
        let middle = page_count / 2;
        let pair = eligible
            .iter()
            .min_by_key(|left| (**left as i64 - i64::from(middle)).abs())
            .copied()
            .unwrap_or(eligible[0]);
        if selected.last() != Some(&pair) {
            selected.push(pair);
        }
    }
    selected
        .into_iter()
        .map(|left| Case::structural(CaseKind::Merge, left, left + 1))
        .collect()
}

fn add_skip(skips: &mut BTreeMap<String, usize>, reason: &str) {
    *skips.entry(reason.to_owned()).or_default() += 1;
}

/// Result of one constructed case.
enum CaseOutcome {
    Record(MetamorphicRecord),
    /// The construction was valid but the comparison cannot observe a content
    /// change, for example a digit run that is masked as a page number.
    Skip(String),
}

/// Builds the edited PDF and scores the flow diff against the truth.
fn evaluate_case(
    file: &str,
    case: Case,
    original: &[u8],
    identity: &Comparison,
    truth: &HashMap<u32, Multiset>,
    dump_dir: Option<&Path>,
    case_limit_scale: f64,
) -> Result<CaseOutcome> {
    let started = Instant::now();
    let a_is_old = !matches!(case.kind, CaseKind::Insert);
    let (old_bytes, new_bytes) = match case.kind {
        CaseKind::Identity => (original.to_vec(), original.to_vec()),
        CaseKind::Delete => (original.to_vec(), edit_bytes(original, &case)?),
        CaseKind::Insert => (edit_bytes(original, &case)?, original.to_vec()),
        CaseKind::Swap => (original.to_vec(), edit_bytes(original, &case)?),
        CaseKind::Duplicate => (original.to_vec(), edit_bytes(original, &case)?),
        CaseKind::TextEdit => {
            let plan = case
                .text_edit
                .as_ref()
                .ok_or_else(|| invalid("text-edit case has no plan".to_owned()))?;
            (original.to_vec(), edit_text_bytes(original, plan)?)
        }
        CaseKind::Merge => (
            original.to_vec(),
            merge_page_bytes(original, case.page, case.right)?,
        ),
        CaseKind::Resubset => (original.to_vec(), resubset_bytes(original)?),
    };
    if let Some(directory) = dump_dir {
        let stem = format!("{file}.{}.{}-{}", case.kind.name(), case.page, case.right);
        fs::write(directory.join(format!("{stem}.old.pdf")), &old_bytes)
            .map_err(|error| BenchError::InvalidInput(format!("cannot dump old PDF: {error}")))?;
        fs::write(directory.join(format!("{stem}.new.pdf")), &new_bytes)
            .map_err(|error| BenchError::InvalidInput(format!("cannot dump new PDF: {error}")))?;
    }
    let comparison = compare_pdf_bytes(old_bytes, new_bytes, case_limit_scale)?;
    let seconds = started.elapsed().as_secs_f64();
    match case.kind {
        CaseKind::TextEdit => return evaluate_text_edit(file, &case, &comparison, seconds),
        CaseKind::Merge => return evaluate_merge(file, &case, &comparison, seconds),
        _ => {}
    }
    let a_side = if a_is_old {
        &comparison.old
    } else {
        &comparison.new
    };
    let stats = comparison.diff.stats();
    let reported = reported_tokens(
        &comparison.diff,
        &comparison.old.stream,
        &comparison.new.stream,
    );
    let empty = Multiset::default();
    // Furniture is a comparison-level decision: the joint rule removes a line
    // from both sides. For the edited page, the untouched side's page tokens
    // are therefore the right content truth (the identity run may still have
    // counted the line as content). When the untouched page's chosen order
    // differs from identity, keep the identity truth so the order flip stays
    // visible instead of being excused.
    let edited_page_truth = (case.kind != CaseKind::Identity
        && case.kind != CaseKind::Swap
        && case.kind != CaseKind::Resubset
        && page_order_matches(&identity.old.pages, &a_side.pages, case.page))
    .then(|| page_multiset(&a_side.stream, case.page));
    let expected = match case.kind {
        CaseKind::Identity | CaseKind::Swap | CaseKind::Resubset => &empty,
        CaseKind::Delete | CaseKind::Insert | CaseKind::Duplicate => edited_page_truth
            .as_ref()
            .unwrap_or_else(|| truth.get(&case.page).unwrap_or(&empty)),
        CaseKind::TextEdit | CaseKind::Merge => &empty,
    };
    // A page edit can make repeated lines qualify as page furniture. Those
    // tokens leave the content stream and are reported in the furniture
    // section, which the truth contract allows, so they are removed from both
    // the truth and the reported content before scoring. Digits are masked
    // exactly like furniture lines.
    let mut furniture_tokens = furniture_token_multiset(&comparison.furniture);
    add_joint_furniture(
        &mut furniture_tokens,
        &a_side.stream,
        if a_is_old {
            &comparison.new.stream
        } else {
            &comparison.old.stream
        },
    );
    let expected_content = expected.without_covered(&furniture_tokens);
    let reported_content = reported.without_covered(&furniture_tokens);
    let furniture_covered_true = expected.total() - expected_content.total();
    let furniture_covered_reported = reported.total() - reported_content.total();
    let correct = reported_content.intersection(&expected_content);
    let true_tokens = expected_content.total();
    let reported_tokens = reported_content.total();
    let precision = ratio(correct, reported_tokens);
    let recall = ratio(correct, true_tokens);
    let forbidden = if case.kind == CaseKind::Identity {
        !comparison.diff.changes.is_empty()
    } else {
        comparison.diff.changes.iter().any(|change| {
            !change_is_furniture_covered(
                change,
                &comparison.old.stream,
                &comparison.new.stream,
                &furniture_tokens,
            ) && is_unexpected_kind(case.kind, change)
        })
    };
    let exact = !forbidden
        && precision == 1.0
        && recall == 1.0
        && comparison.old.complete
        && comparison.new.complete;
    let order_flips = order_flips(&identity.old.pages, &a_side.pages);
    let furniture_difference = furniture_difference(&identity.old.stream, &a_side.stream);
    let mut detail = format!(
        "pages={}-{} delete={}/{}t insert={}/{}t replace={}/{}t move={}/{}t layout={} furniture_changes={} furniture_covered={}/{} order_flips={:?} a_furniture_diff={} extraction_complete={}/{}",
        case.page,
        if case.right == 0 {
            case.page
        } else {
            case.right
        },
        stats.delete.changes,
        stats.delete.old_tokens,
        stats.insert.changes,
        stats.insert.new_tokens,
        stats.replace.changes,
        stats.replace.old_tokens,
        stats.moves.changes,
        stats.moves.old_tokens,
        stats.layout_only.changes,
        comparison.furniture.len(),
        furniture_covered_reported,
        furniture_covered_true,
        order_flips,
        furniture_difference,
        comparison.old.complete,
        comparison.new.complete,
    );
    if !order_flips.is_empty() {
        detail.push_str(" flips=");
        detail.push_str(&flip_evidence(&identity.old.pages, &a_side.pages));
    }
    if !exact {
        detail.push_str(" missing=");
        detail.push_str(&format_texts(
            reported_content.missing(&expected_content),
            6,
        ));
        detail.push_str(" extra=");
        detail.push_str(&format_texts(reported_content.extra(&expected_content), 6));
    }
    Ok(CaseOutcome::Record(MetamorphicRecord {
        file: file.to_owned(),
        case: case.kind.name().to_owned(),
        page: case.page,
        true_tokens,
        reported_tokens,
        correct_tokens: correct,
        precision,
        recall,
        exact,
        moves: stats.moves.changes,
        seconds,
        detail,
    }))
}

/// Applies one planned single-digit text edit and serializes the result.
fn edit_text_bytes(original: &[u8], plan: &TextEditPlan) -> Result<Vec<u8>> {
    let mut document = LopdfDocument::load_mem(original)
        .map_err(|error| invalid(format!("cannot parse PDF for text edit: {error}")))?;
    flow_edit::apply_text_edit(&mut document, plan)
        .map_err(|reason| invalid(format!("text edit failed: {reason}")))?;
    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|error| invalid(format!("cannot serialize text-edit PDF: {error}")))?;
    Ok(output)
}

/// Merges two adjacent pages and serializes the result.
fn merge_page_bytes(original: &[u8], left: u32, right: u32) -> Result<Vec<u8>> {
    let mut document = LopdfDocument::load_mem(original)
        .map_err(|error| invalid(format!("cannot parse PDF for merge: {error}")))?;
    flow_edit::merge_pages(&mut document, left, right)
        .map_err(|reason| invalid(format!("page merge failed: {reason}")))?;
    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|error| invalid(format!("cannot serialize merged PDF: {error}")))?;
    Ok(output)
}

/// Scores a text-edit case against its constructed token replacement.
fn evaluate_text_edit(
    file: &str,
    case: &Case,
    comparison: &Comparison,
    seconds: f64,
) -> Result<CaseOutcome> {
    let plan = case
        .text_edit
        .as_ref()
        .ok_or_else(|| invalid("text-edit case has no plan".to_owned()))?;
    let stats = comparison.diff.stats();
    let mut expected = Multiset::default();
    expected.add(&plan.old_token);
    expected.add(&plan.new_token);
    let reported = reported_tokens(
        &comparison.diff,
        &comparison.old.stream,
        &comparison.new.stream,
    );
    let correct = reported.intersection(&expected);
    let true_tokens = expected.total();
    let reported_tokens = reported.total();
    let mut single_replace = false;
    let mut replacement_matches = false;
    let mut boundary_tokens = None;
    if let [
        FlowChange::Replace {
            old: old_range,
            new: new_range,
        },
    ] = &comparison.diff.changes[..]
    {
        single_replace = true;
        let old_tokens = &comparison.old.stream.tokens[old_range.clone()];
        let new_tokens = &comparison.new.stream.tokens[new_range.clone()];
        if old_tokens.len() == 1
            && new_tokens.len() == 1
            && old_tokens[0].page.0 == plan.page
            && new_tokens[0].page.0 == plan.page
        {
            let old_text = &old_tokens[0].text;
            let new_text = &new_tokens[0].text;
            replacement_matches = *old_text == plan.old_token && *new_text == plan.new_token;
            if !replacement_matches
                && old_text.contains(plan.old_digit)
                && plan.old_token.contains(old_text.as_str())
                && plan.new_token.contains(new_text.as_str())
                && old_text.replacen(plan.old_digit, &plan.new_digit.to_string(), 1) == *new_text
            {
                // The edited glyph's token boundary is shorter than the
                // planned one (for example a superscript marker). The actual
                // token pair is the constructed truth.
                boundary_tokens = Some((old_text.clone(), new_text.clone()));
                replacement_matches = true;
            }
        }
    }
    let mut correct = correct;
    let mut true_tokens = true_tokens;
    let mut reported_tokens = reported_tokens;
    if let Some((old_text, new_text)) = &boundary_tokens {
        let mut boundary = Multiset::default();
        boundary.add(old_text);
        boundary.add(new_text);
        correct = reported.intersection(&boundary);
        true_tokens = boundary.total();
        reported_tokens = reported.total();
    }
    let precision = ratio(correct, reported_tokens);
    let recall = ratio(correct, true_tokens);
    let furniture_edit = !single_replace
        && reported.total() == 0
        && match_furniture_edit(&comparison.furniture, &plan.old_token, &plan.new_token);
    let exact = ((single_replace && replacement_matches) || furniture_edit)
        && comparison.old.complete
        && comparison.new.complete;
    let (true_tokens, reported_tokens, correct, precision, recall) = if furniture_edit {
        (2, 2, 2, 1.0, 1.0)
    } else {
        (true_tokens, reported_tokens, correct, precision, recall)
    };
    let mut detail = format!(
        "pages={} delete={}/{}t insert={}/{}t replace={}/{}t move={}/{}t layout={} expected={:?}->{:?} single_replace={} replacement_matches={} furniture_edit={} extraction_complete={}/{}",
        case.page,
        stats.delete.changes,
        stats.delete.old_tokens,
        stats.insert.changes,
        stats.insert.new_tokens,
        stats.replace.changes,
        stats.replace.old_tokens,
        stats.moves.changes,
        stats.moves.old_tokens,
        stats.layout_only.changes,
        plan.old_token,
        plan.new_token,
        single_replace,
        replacement_matches,
        furniture_edit,
        comparison.old.complete,
        comparison.new.complete,
    );
    if !exact {
        detail.push_str(" actual=");
        detail.push_str(&change_texts(
            &comparison.diff.changes,
            &comparison.old.stream,
            &comparison.new.stream,
        ));
    }
    if !single_replace
        && !furniture_edit
        && plan
            .old_token
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return Ok(CaseOutcome::Skip(
            "text-edit: page-number-like furniture run".to_owned(),
        ));
    }
    Ok(CaseOutcome::Record(MetamorphicRecord {
        file: file.to_owned(),
        case: case.kind.name().to_owned(),
        page: case.page,
        true_tokens,
        reported_tokens,
        correct_tokens: correct,
        precision,
        recall,
        exact,
        moves: stats.moves.changes,
        seconds,
        detail,
    }))
}

/// Scores a page-merge case: no content change and all glyphs preserved.
fn evaluate_merge(
    file: &str,
    case: &Case,
    comparison: &Comparison,
    seconds: f64,
) -> Result<CaseOutcome> {
    let stats = comparison.diff.stats();
    let reported = reported_tokens(
        &comparison.diff,
        &comparison.old.stream,
        &comparison.new.stream,
    );
    let mut furniture_tokens = furniture_token_multiset(&comparison.furniture);
    add_joint_furniture(
        &mut furniture_tokens,
        &comparison.old.stream,
        &comparison.new.stream,
    );
    let reported_content = reported.without_covered(&furniture_tokens);
    let content_changed = reported_content.total();
    let furniture_only = reported.total() - content_changed;
    let glyphs_equal = comparison.old.glyphs.total() == comparison.new.glyphs.total()
        && comparison.old.glyphs.intersection(&comparison.new.glyphs)
            == comparison.old.glyphs.total();
    let exact =
        content_changed == 0 && glyphs_equal && comparison.old.complete && comparison.new.complete;
    let precision = ratio(0, content_changed);
    let mut detail = format!(
        "pages={}-{} delete={}/{}t insert={}/{}t replace={}/{}t move={}/{}t layout={} all_glyphs_equal={} furniture_only={} furniture_changes={} extraction_complete={}/{}",
        case.page,
        case.right,
        stats.delete.changes,
        stats.delete.old_tokens,
        stats.insert.changes,
        stats.insert.new_tokens,
        stats.replace.changes,
        stats.replace.old_tokens,
        stats.moves.changes,
        stats.moves.old_tokens,
        stats.layout_only.changes,
        glyphs_equal,
        furniture_only,
        comparison.furniture.len(),
        comparison.old.complete,
        comparison.new.complete,
    );
    if !comparison.furniture.is_empty() {
        detail.push_str(" furniture=");
        detail.push_str(&format_texts(
            comparison
                .furniture
                .iter()
                .map(|change| change.text.as_str())
                .collect(),
            6,
        ));
    }
    if !glyphs_equal {
        detail.push_str(" missing_glyphs=");
        detail.push_str(&format_texts(
            comparison.old.glyphs.missing(&comparison.new.glyphs),
            4,
        ));
        detail.push_str(" extra_glyphs=");
        detail.push_str(&format_texts(
            comparison.old.glyphs.extra(&comparison.new.glyphs),
            4,
        ));
    }
    if !exact {
        detail.push_str(" content_changes=");
        detail.push_str(&change_texts(
            &comparison.diff.changes,
            &comparison.old.stream,
            &comparison.new.stream,
        ));
    }
    Ok(CaseOutcome::Record(MetamorphicRecord {
        file: file.to_owned(),
        case: case.kind.name().to_owned(),
        page: case.page,
        true_tokens: 0,
        reported_tokens: content_changed,
        correct_tokens: 0,
        precision,
        recall: 1.0,
        exact,
        moves: stats.moves.changes,
        seconds,
        detail,
    }))
}

/// Matches a digit edit inside a furniture line.
///
/// Furniture is diffed by its variable-masked line text. A constant digit run
/// (a form number, year, or version) stays literal, so editing it appears as a
/// Removed line and an Added line whose tokens differ at exactly the edited
/// position. Page-number-like runs are masked and do not reach this check.
fn match_furniture_edit(changes: &[FurnitureChange], old_token: &str, new_token: &str) -> bool {
    let old_line = |change: &FurnitureChange| {
        change.kind == FurnitureKind::Removed
            && change
                .text
                .split_whitespace()
                .any(|token| token == old_token)
    };
    let new_line = |change: &FurnitureChange| {
        change.kind == FurnitureKind::Added
            && change
                .text
                .split_whitespace()
                .any(|token| token == new_token)
    };
    let old_changes = changes
        .iter()
        .filter(|change| old_line(change))
        .collect::<Vec<_>>();
    let new_changes = changes
        .iter()
        .filter(|change| new_line(change))
        .collect::<Vec<_>>();
    if old_changes.len() != 1 || new_changes.len() != 1 {
        return false;
    }
    let old_words = old_changes[0].text.split_whitespace().collect::<Vec<_>>();
    let new_words = new_changes[0].text.split_whitespace().collect::<Vec<_>>();
    if old_words.len() != new_words.len() {
        return false;
    }
    let old_positions = old_words
        .iter()
        .enumerate()
        .filter(|(_, token)| **token == old_token)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let new_positions = new_words
        .iter()
        .enumerate()
        .filter(|(_, token)| **token == new_token)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if old_positions.len() != 1 || new_positions.len() != 1 || old_positions[0] != new_positions[0]
    {
        return false;
    }
    let index = old_positions[0];
    old_words
        .iter()
        .zip(&new_words)
        .enumerate()
        .all(|(position, (left, right))| {
            if position == index {
                *left == old_token && *right == new_token
            } else {
                left == right
            }
        })
}

/// Summarises the reported changes for a failing special case.
fn change_texts(changes: &[FlowChange], old: &FlowStream, new: &FlowStream) -> String {
    let mut parts = Vec::new();
    for change in changes.iter().take(8) {
        let text = match change {
            FlowChange::Delete { old: range } => format!(
                "delete={:?}",
                old.tokens[range.clone()]
                    .iter()
                    .map(|token| token.text.as_str())
                    .collect::<Vec<_>>()
            ),
            FlowChange::Insert { new: range } => format!(
                "insert={:?}",
                new.tokens[range.clone()]
                    .iter()
                    .map(|token| token.text.as_str())
                    .collect::<Vec<_>>()
            ),
            FlowChange::Replace {
                old: old_range,
                new: new_range,
            } => format!(
                "replace={:?}->{:?}",
                old.tokens[old_range.clone()]
                    .iter()
                    .map(|token| token.text.as_str())
                    .collect::<Vec<_>>(),
                new.tokens[new_range.clone()]
                    .iter()
                    .map(|token| token.text.as_str())
                    .collect::<Vec<_>>()
            ),
            FlowChange::Move {
                old: old_range,
                new: new_range,
            } => format!(
                "move={}t@{}->{}",
                old_range.len(),
                old_range.start,
                new_range.start
            ),
            FlowChange::ReadingOrder { old: old_range, .. } => {
                format!("reading-order={}t@{}", old_range.len(), old_range.start)
            }
            FlowChange::LayoutOnly { old: old_range, .. } => {
                format!("layout={}t@{}", old_range.len(), old_range.start)
            }
        };
        parts.push(text);
    }
    parts.join(";")
}

/// Whether a resubset case applies and which page carries the evidence.
enum ResubsetPlan {
    /// The document has no unmapped placeholders, so there is nothing to test.
    NoPlaceholders,
    /// A program-identified placeholder exists; changing the program would be
    /// a real identity change, so zero-change truth is not applicable.
    Skip(String),
    /// Every placeholder uses name or outline identity; rewrite font programs
    /// and require an empty diff.
    Eligible(u32),
}

/// Classifies one document for the resubset metamorphic case.
///
/// Name and outline identities deliberately ignore the program bytes, so
/// appending harmless trailing bytes to a font program must not change any
/// token. Program identity does include those bytes, so a document that still
/// has program-identified placeholders is skipped instead of asserting a truth
/// the evidence cannot support.
fn resubset_plan(identity: &Comparison) -> ResubsetPlan {
    let mut any = 0usize;
    let mut program = 0usize;
    let mut page = None;
    for token in identity
        .old
        .stream
        .tokens
        .iter()
        .chain(identity.new.stream.tokens.iter())
    {
        let Some(rest) = token.text.strip_prefix('\u{fffc}') else {
            continue;
        };
        any += 1;
        if rest.starts_with("program:") {
            program += 1;
        }
        if (rest.starts_with("name:") || rest.starts_with("outline:")) && page.is_none() {
            page = Some(token.page.0.saturating_add(1));
        }
    }
    if any == 0 {
        ResubsetPlan::NoPlaceholders
    } else if program > 0 {
        ResubsetPlan::Skip(format!(
            "resubset: {program} of {any} placeholders use program identity"
        ))
    } else {
        ResubsetPlan::Eligible(page.unwrap_or(1))
    }
}

/// Appends harmless trailing bytes to every embedded font program.
///
/// The marker changes the decoded program bytes and therefore today's program
/// hash, while the font's names and outlines are unchanged. Type 1 readers stop
/// at `eexec`, CFF readers use absolute offsets, and this engine does not parse
/// TrueType tables; a valid reader ignores the trailing comment either way.
fn resubset_bytes(original: &[u8]) -> Result<Vec<u8>> {
    const MAX_PROGRAM_BYTES: usize = 64 * 1024 * 1024;
    let mut document = LopdfDocument::load_mem(original)
        .map_err(|error| invalid(format!("cannot parse PDF for resubset: {error}")))?;
    let mut programs = BTreeSet::new();
    for object in document.objects.values() {
        let Object::Dictionary(dictionary) = object else {
            continue;
        };
        for key in [b"FontFile".as_slice(), b"FontFile2", b"FontFile3"] {
            if let Ok(Object::Reference(reference)) = dictionary.get(key) {
                programs.insert(*reference);
            }
        }
    }
    if programs.is_empty() {
        return Err(invalid(
            "resubset: document has no embedded font program".to_owned(),
        ));
    }
    let mut rewritten = 0usize;
    for reference in programs {
        let Some(stream) = document
            .objects
            .get_mut(&reference)
            .and_then(|object| object.as_stream_mut().ok())
        else {
            continue;
        };
        let Ok(mut content) = stream.decompressed_content_with_limit(MAX_PROGRAM_BYTES) else {
            continue;
        };
        content.extend_from_slice(b"\n% pdfdelta resubset\n");
        stream.set_plain_content(content);
        // Recompress when it saves space; an incompressible program may stay
        // plain, which is still a valid PDF stream.
        let _ = stream.compress();
        rewritten = rewritten.saturating_add(1);
    }
    if rewritten == 0 {
        return Err(invalid(
            "resubset: no embedded font program could be decoded".to_owned(),
        ));
    }
    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|error| invalid(format!("cannot serialize resubset PDF: {error}")))?;
    Ok(output)
}

/// Applies one page-tree edit and serializes the result.
fn edit_bytes(original: &[u8], case: &Case) -> Result<Vec<u8>> {
    let mut document = LopdfDocument::load_mem(original)
        .map_err(|error| invalid(format!("cannot parse PDF for surgery: {error}")))?;
    match case.kind {
        CaseKind::Identity => {}
        CaseKind::Delete | CaseKind::Insert => {
            if !document.get_pages().contains_key(&case.page) {
                return Err(invalid(format!("page {} does not exist", case.page)));
            }
            document.delete_pages(&[case.page]);
        }
        CaseKind::Swap => {
            if !swap_pages(&mut document, case.page, case.right)? {
                return Err(invalid(format!(
                    "pages {} and {} live under different page-tree nodes",
                    case.page, case.right
                )));
            }
        }
        CaseKind::Duplicate => duplicate_page(&mut document, case.page)?,
        CaseKind::TextEdit | CaseKind::Merge | CaseKind::Resubset => {
            return Err(invalid("special case reached edit_bytes".to_owned()));
        }
    }
    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|error| invalid(format!("cannot serialize edited PDF: {error}")))?;
    Ok(output)
}

/// Exchanges two page references that share a page-tree node.
///
/// Returns `false` when the pages live under different nodes, where swapping
/// the references would break inherited attributes. Such a case is skipped.
fn swap_pages(document: &mut LopdfDocument, left: u32, right: u32) -> Result<bool> {
    let pages = document.get_pages();
    let left_id = *pages
        .get(&left)
        .ok_or_else(|| invalid(format!("page {left} does not exist")))?;
    let right_id = *pages
        .get(&right)
        .ok_or_else(|| invalid(format!("page {right} does not exist")))?;
    let left_parent = parent_of(document, left_id)?;
    let right_parent = parent_of(document, right_id)?;
    if left_parent != right_parent {
        return Ok(false);
    }
    let parent = document
        .get_object_mut(left_parent)
        .map_err(|error| invalid(format!("cannot read page-tree node: {error}")))?
        .as_dict_mut()
        .map_err(|error| invalid(format!("page-tree node is not a dictionary: {error}")))?;
    let kids = parent
        .get_mut(b"Kids")
        .map_err(|error| invalid(format!("page-tree node has no Kids: {error}")))?
        .as_array_mut()
        .map_err(|error| invalid(format!("page-tree Kids is not an array: {error}")))?;
    let left_index = kids
        .iter()
        .position(|kid| kid.as_reference().ok() == Some(left_id))
        .ok_or_else(|| invalid(format!("page {left} is not in its parent's Kids")))?;
    let right_index = kids
        .iter()
        .position(|kid| kid.as_reference().ok() == Some(right_id))
        .ok_or_else(|| invalid(format!("page {right} is not in its parent's Kids")))?;
    kids.swap(left_index, right_index);
    Ok(true)
}

/// Inserts a shallow copy of `page` after itself and updates `/Count`.
fn duplicate_page(document: &mut LopdfDocument, page: u32) -> Result<()> {
    let page_id = *document
        .get_pages()
        .get(&page)
        .ok_or_else(|| invalid(format!("page {page} does not exist")))?;
    let page_dictionary = document
        .get_dictionary(page_id)
        .map_err(|error| invalid(format!("cannot read page {page}: {error}")))?
        .clone();
    let parent_id = page_dictionary
        .get(b"Parent")
        .map_err(|error| invalid(format!("page {page} has no Parent: {error}")))?
        .as_reference()
        .map_err(|error| invalid(format!("page {page} Parent is not a reference: {error}")))?;
    let copy_id = document.add_object(Object::Dictionary(page_dictionary));
    {
        let parent = document
            .get_object_mut(parent_id)
            .map_err(|error| invalid(format!("cannot read page-tree node: {error}")))?
            .as_dict_mut()
            .map_err(|error| invalid(format!("page-tree node is not a dictionary: {error}")))?;
        let kids = parent
            .get_mut(b"Kids")
            .map_err(|error| invalid(format!("page-tree node has no Kids: {error}")))?
            .as_array_mut()
            .map_err(|error| invalid(format!("page-tree Kids is not an array: {error}")))?;
        let index = kids
            .iter()
            .position(|kid| kid.as_reference().ok() == Some(page_id))
            .ok_or_else(|| invalid(format!("page {page} is not in its parent's Kids")))?;
        kids.insert(index + 1, Object::Reference(copy_id));
    }
    let mut next = Some(parent_id);
    while let Some(id) = next {
        let parent = document
            .get_object_mut(id)
            .map_err(|error| invalid(format!("cannot read page-tree node: {error}")))?
            .as_dict_mut()
            .map_err(|error| invalid(format!("page-tree node is not a dictionary: {error}")))?;
        if let Ok(count) = parent.get(b"Count").and_then(Object::as_i64) {
            parent.set("Count", count + 1);
        }
        next = parent
            .get(b"Parent")
            .ok()
            .and_then(|parent| parent.as_reference().ok());
    }
    Ok(())
}

fn parent_of(document: &LopdfDocument, page_id: lopdf::ObjectId) -> Result<lopdf::ObjectId> {
    document
        .get_dictionary(page_id)
        .map_err(|error| invalid(format!("cannot read page object: {error}")))?
        .get(b"Parent")
        .map_err(|error| invalid(format!("page object has no Parent: {error}")))?
        .as_reference()
        .map_err(|error| invalid(format!("page Parent is not a reference: {error}")))
}

/// Page token multisets for the chosen pages of the identity flow stream.
fn page_multisets(stream: &FlowStream, pages: &[u32]) -> HashMap<u32, Multiset> {
    let mut multisets = pages
        .iter()
        .map(|&page| (page, Multiset::default()))
        .collect::<HashMap<_, _>>();
    for token in &stream.tokens {
        let page = token.page.0 + 1;
        if let Some(multiset) = multisets.get_mut(&page) {
            multiset.add(&token.text);
        }
    }
    multisets
}

/// Token multiset of one page in a flow stream.
fn page_multiset(stream: &FlowStream, page: u32) -> Multiset {
    let mut multiset = Multiset::default();
    for token in &stream.tokens {
        if token.page.0 + 1 == page {
            multiset.add(&token.text);
        }
    }
    multiset
}

/// Whether the untouched side chose the same order as the identity run.
fn page_order_matches(
    identity: &[PageOrderEvidence],
    current: &[PageOrderEvidence],
    page: u32,
) -> bool {
    let find = |pages: &[PageOrderEvidence]| {
        pages
            .iter()
            .find(|evidence| evidence.page.0 + 1 == page)
            .map(|evidence| evidence.order)
    };
    find(identity) == find(current)
}

/// Multiset of token texts covered by content changes.
/// Counts visible glyph identities independently of page order.
fn glyph_multiset(document: &Document<Glyph>) -> Multiset {
    let mut counts = Multiset::default();
    for glyph in document.items() {
        let text = match &glyph.text {
            DecodedText::Mapped(text) => text.clone(),
            DecodedText::Unmapped { .. } => format!("unmapped:{}", hex_bytes(&glyph.raw_code)),
        };
        counts.add(&format!(
            "{}:{}:{}",
            glyph.font_id.0,
            hex_bytes(&glyph.raw_code),
            text
        ));
    }
    counts
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn reported_tokens(diff: &FlowDiff, old: &FlowStream, new: &FlowStream) -> Multiset {
    let mut reported = Multiset::default();
    for change in &diff.changes {
        match change {
            FlowChange::Delete { old: range } => {
                for token in &old.tokens[range.clone()] {
                    reported.add(&token.text);
                }
            }
            FlowChange::Insert { new: range } => {
                for token in &new.tokens[range.clone()] {
                    reported.add(&token.text);
                }
            }
            FlowChange::Replace {
                old: old_range,
                new: new_range,
            } => {
                for token in &old.tokens[old_range.clone()] {
                    reported.add(&token.text);
                }
                for token in &new.tokens[new_range.clone()] {
                    reported.add(&token.text);
                }
            }
            FlowChange::Move { .. }
            | FlowChange::ReadingOrder { .. }
            | FlowChange::LayoutOnly { .. } => {}
        }
    }
    reported
}

/// Token text counts with multiset arithmetic.
#[derive(Clone, Debug, Default)]
struct Multiset {
    counts: HashMap<String, usize>,
    total: usize,
}

impl Multiset {
    fn add(&mut self, text: &str) {
        *self.counts.entry(text.to_owned()).or_default() += 1;
        self.total += 1;
    }

    fn total(&self) -> usize {
        self.total
    }

    /// Number of texts shared with `other`, counting multiplicity.
    fn intersection(&self, other: &Self) -> usize {
        self.counts
            .iter()
            .map(|(text, count)| (*count).min(other.counts.get(text).copied().unwrap_or(0)))
            .sum()
    }

    /// Up to `limit` texts that `other` has more of than `self`.
    fn missing<'a>(&self, other: &'a Self) -> Vec<&'a str> {
        let mut texts = other
            .counts
            .iter()
            .filter(|(text, count)| **count > self.counts.get(*text).copied().unwrap_or(0))
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>();
        texts.sort_unstable();
        texts.truncate(8);
        texts
    }

    /// Up to `limit` texts that `self` has more of than `other`.
    fn extra(&self, other: &Self) -> Vec<&str> {
        let mut texts = self
            .counts
            .iter()
            .filter(|(text, count)| **count > other.counts.get(*text).copied().unwrap_or(0))
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>();
        texts.sort_unstable();
        texts.truncate(8);
        texts
    }

    /// Returns a copy without tokens covered by a masked furniture multiset.
    fn without_covered(&self, cover: &Self) -> Self {
        let mut reduced = Self::default();
        for (text, count) in &self.counts {
            let furniture = cover.counts.get(&mask_digits(text)).copied().unwrap_or(0);
            let remaining = count.saturating_sub(furniture);
            if remaining > 0 {
                reduced.counts.insert(text.clone(), remaining);
                reduced.total += remaining;
            }
        }
        reduced
    }

    /// Removes one masked occurrence, returning whether one was present.
    fn take_masked(&mut self, text: &str) -> bool {
        let masked = mask_digits(text);
        let Some(count) = self.counts.get_mut(&masked) else {
            return false;
        };
        if *count == 0 {
            return false;
        }
        *count -= 1;
        self.total -= 1;
        true
    }

    /// All texts, for identity evidence.
    fn texts(&self) -> Vec<&str> {
        let mut texts = self.counts.keys().map(String::as_str).collect::<Vec<_>>();
        texts.sort_unstable();
        texts
    }
}

/// Pages whose chosen order differs from the identity run on the untouched side.
fn order_flips(identity: &[PageOrderEvidence], current: &[PageOrderEvidence]) -> Vec<u32> {
    let baseline = identity
        .iter()
        .map(|page| (page.page.0, page.order))
        .collect::<HashMap<_, _>>();
    let mut flips = current
        .iter()
        .filter(|page| {
            baseline
                .get(&page.page.0)
                .is_some_and(|order| *order != page.order)
        })
        .map(|page| page.page.0 + 1)
        .collect::<Vec<_>>();
    flips.sort_unstable();
    flips
}

/// Chosen-order differences with the case side's hypothesis scores.
fn flip_evidence(identity: &[PageOrderEvidence], current: &[PageOrderEvidence]) -> String {
    let baseline = identity
        .iter()
        .map(|page| (page.page.0, page))
        .collect::<HashMap<_, _>>();
    current
        .iter()
        .filter_map(|page| {
            let previous = baseline.get(&page.page.0)?;
            previous.order.ne(&page.order).then(|| {
                format!(
                    "{}:A {} (l={},s={},b={},c={:.3}) -> edit {} (l={},s={},b={},c={:.3})",
                    page.page.0 + 1,
                    order_name(previous.order),
                    previous.layout_score,
                    previous.stream_score,
                    previous.baseline_score,
                    previous.coherence,
                    order_name(page.order),
                    page.layout_score,
                    page.stream_score,
                    page.baseline_score,
                    page.coherence,
                )
            })
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn order_name(order: OrderKind) -> &'static str {
    order.name()
}

/// Number of furniture lines that differ between the identity and case runs.
fn furniture_difference(identity: &FlowStream, current: &FlowStream) -> usize {
    fn signatures(furniture: &[FurnitureLine]) -> BTreeSet<(String, Vec<u32>, usize)> {
        furniture
            .iter()
            .map(|line| {
                (
                    line.text.clone(),
                    line.pages.iter().map(|page| page.0 + 1).collect(),
                    line.occurrences,
                )
            })
            .collect()
    }
    let left = signatures(&identity.furniture);
    let right = signatures(&current.furniture);
    left.symmetric_difference(&right).count()
}

/// Builds the multiset of masked tokens named by reported furniture changes.
///
/// A furniture change stands for the line's occurrences on every page it
/// lists, so its tokens are counted once per page. Reclassifying a line as
/// furniture removes one content occurrence per listed page, and the
/// allowance must cover each of those removals.
fn furniture_token_multiset(changes: &[FurnitureChange]) -> Multiset {
    let mut tokens = Multiset::default();
    for change in changes {
        let pages = match change.kind {
            FurnitureKind::Added => &change.new_pages,
            FurnitureKind::Removed => &change.old_pages,
        };
        let occurrences = pages.len().max(1);
        for text in change.text.split_whitespace() {
            let masked = mask_digits(text);
            for _ in 0..occurrences {
                tokens.add(&masked);
            }
        }
    }
    tokens
}

/// Adds furniture lines that are furniture on both sides of the comparison.
///
/// The engine's joint rule removes a line from both documents when it
/// qualifies on either side. Such a line produces no furniture *change*, yet
/// the identity truth can still contain its tokens. Only lines present in both
/// furniture lists are added, scaled by the untouched side's occurrence count,
/// so unrelated content that happens to share a furniture word is not excused.
fn add_joint_furniture(multiset: &mut Multiset, a_side: &FlowStream, other: &FlowStream) {
    let other_texts = other
        .furniture
        .iter()
        .map(|line| line.text.as_str())
        .collect::<BTreeSet<_>>();
    for line in &a_side.furniture {
        if !other_texts.contains(line.text.as_str()) {
            continue;
        }
        let occurrences = line.occurrences.max(1);
        for text in line.text.split_whitespace() {
            let masked = mask_digits(text);
            for _ in 0..occurrences {
                multiset.add(&masked);
            }
        }
    }
}

/// Returns whether a change is entirely explained by the furniture section.
fn change_is_furniture_covered(
    change: &FlowChange,
    old: &FlowStream,
    new: &FlowStream,
    furniture: &Multiset,
) -> bool {
    let mut remaining = furniture.clone();
    let tokens = match change {
        FlowChange::Delete { old: range } => &old.tokens[range.clone()],
        FlowChange::Insert { new: range } => &new.tokens[range.clone()],
        FlowChange::Replace {
            old: old_range,
            new: new_range,
        } => {
            if !range_covered(&mut remaining, &old.tokens[old_range.clone()]) {
                return false;
            }
            return range_covered(&mut remaining, &new.tokens[new_range.clone()]);
        }
        FlowChange::Move { .. }
        | FlowChange::ReadingOrder { .. }
        | FlowChange::LayoutOnly { .. } => return false,
    };
    range_covered(&mut remaining, tokens)
}

fn range_covered(multiset: &mut Multiset, tokens: &[FlowToken]) -> bool {
    tokens.iter().all(|token| multiset.take_masked(&token.text))
}

/// Whether a change kind is unexpected for a constructed edit.
fn is_unexpected_kind(case: CaseKind, change: &FlowChange) -> bool {
    match case {
        CaseKind::Identity => true,
        CaseKind::Delete => !matches!(change, FlowChange::Delete { .. }),
        CaseKind::Insert | CaseKind::Duplicate => !matches!(change, FlowChange::Insert { .. }),
        CaseKind::Swap => !matches!(
            change,
            FlowChange::Move { .. }
                | FlowChange::ReadingOrder { .. }
                | FlowChange::LayoutOnly { .. }
        ),
        CaseKind::TextEdit | CaseKind::Merge => false,
        CaseKind::Resubset => true,
    }
}

/// Collapses every ASCII digit run to one `0`, matching furniture text.
fn mask_digits(text: &str) -> String {
    let mut masked = String::with_capacity(text.len());
    let mut in_digits = false;
    for scalar in text.chars() {
        if scalar.is_ascii_digit() {
            if !in_digits {
                masked.push('0');
                in_digits = true;
            }
        } else {
            masked.push(scalar);
            in_digits = false;
        }
    }
    masked
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn format_texts(texts: Vec<&str>, limit: usize) -> String {
    texts
        .into_iter()
        .take(limit)
        .map(|text| format!("{text:?}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn tsv_field(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\t' | '\n' | '\r' => ' ',
            character => character,
        })
        .collect()
}

fn invalid(message: String) -> BenchError {
    BenchError::InvalidInput(message)
}

fn read_bounded_pdf(path: &Path) -> Result<Vec<u8>> {
    let limit = ParseLimits::default().max_input_bytes;
    let file = fs::File::open(path).map_err(|error| {
        BenchError::InvalidInput(format!("cannot read {}: {error}", path.display()))
    })?;
    let read_limit = u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            BenchError::InvalidInput(format!("cannot read {}: {error}", path.display()))
        })?;
    if bytes.len() > limit {
        return Err(BenchError::InvalidInput(format!(
            "{} exceeds the {limit}-byte input limit",
            path.display()
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_choice_is_deterministic_and_inside_the_document() {
        for name in ["a.pdf", "b.pdf", "gcc-13-3-to-13-4-old.pdf"] {
            let seed = seed_from_name(name);
            let pages = chosen_pages(10, seed);
            assert_eq!(pages, chosen_pages(10, seed));
            assert!(
                pages.windows(2).all(|pair| pair[0] < pair[1]),
                "pages are sorted and unique: {pages:?}"
            );
            assert!(pages.iter().all(|page| (1..=10).contains(page)));
            assert!(pages.contains(&1) && pages.contains(&5) && pages.contains(&10));
        }
        assert!(chosen_pages(3, 7).is_empty());
    }

    #[test]
    fn multiset_scores_counts_not_positions() {
        let mut truth = Multiset::default();
        truth.add("a");
        truth.add("a");
        truth.add("b");
        let mut reported = Multiset::default();
        reported.add("a");
        reported.add("c");
        assert_eq!(reported.intersection(&truth), 1);
        assert_eq!(reported.missing(&truth), ["a", "b"]);
        assert_eq!(reported.extra(&truth), ["c"]);
        assert_eq!(ratio(0, 0), 1.0);
        assert_eq!(ratio(1, 2), 0.5);
    }
}
