//! `pdfdelta flow` subcommand: a token-stream diff of two PDF documents.
//!
//! The flow engine compares reconstructed reading order, so wrapped lines,
//! page breaks, and reflowed paragraph boundaries are layout changes rather
//! than content changes. This command renders that comparison for humans and
//! serializes it for downstream tooling. Extraction problems are surfaced
//! instead of being hidden: an incomplete side is never presented as fully
//! compared, and it takes precedence over the content-change exit code.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::File,
    io::{BufReader, BufWriter, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use pdfdelta_core::{
    document::image_diff::ImageInventory,
    model::{Document, Glyph, PageId},
    pdf::{PageRef, ParseLimits},
    source::{
        ContentStreamGlyphExtractor, ExternalFontIdentities, ExtractionIssue, ExtractionIssueKind,
        ExtractionLimits, ExtractionScope,
    },
};
use pdfdelta_flow::{
    ChangeAssessment, FlowCandidates, FlowChange, FlowDiff, FlowError, FlowStats, FlowStream,
    FlowToken, FormattingChange, ORDER_COHERENCE_MIN, OrderKind, PageOrderEvidence, PaintIdentity,
    assess_changes, flow_candidates_owned, flow_diff, flow_order_from_candidates,
    formatting_changes, furniture_changes, selects_difference,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    evidence_text::escape_terminal_controls,
    flow_trace::{FlowTrace, TraceSide, issue_metrics},
    fs::{input_read_flow_error, parse_lopdf, read_limited_typed, write_output_atomically},
};

/// Writes token texts as one JSON string per line next to the report.
///
/// The sidecar name is the report path without a trailing `.gz`, followed by
/// `.<side>-tokens.jsonl` and `.gz` again when the report itself is compressed.
/// Tokens stay out of the report object so a large corpus run can stream them
/// without holding every token in memory.
fn write_token_sidecars(path: &Path, old: &FlowStream, new: &FlowStream) -> Result<(), FlowError> {
    for (side, stream) in [("old", old), ("new", new)] {
        let sidecar = token_sidecar_path(path, side);
        write_output_atomically(&sidecar, "flow token sidecar", |output| {
            for text in token_texts(stream) {
                serde_json::to_writer(&mut *output, text)
                    .map_err(|error| format!("failed to serialize token text: {error}"))?;
                output
                    .write_all(b"\n")
                    .map_err(|error| format!("failed to write token sidecar: {error}"))?;
            }
            Ok(())
        })
        .map_err(FlowError::backend)?;
    }
    Ok(())
}

/// Builds `<report without .gz>.<side>-tokens.jsonl[.gz]`.
fn token_sidecar_path(path: &Path, side: &str) -> PathBuf {
    let compressed = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gz"));
    let base = if compressed {
        path.with_extension("")
    } else {
        path.to_path_buf()
    };
    let suffix = if compressed { ".gz" } else { "" };
    PathBuf::from(format!("{}.{side}-tokens.jsonl{suffix}", base.display()))
}

/// Number of unchanged tokens shown on each side of a change.
const CONTEXT_TOKENS: usize = 8;
/// A changed side longer than this is elided in the middle.
const ELISION_HEAD: usize = 40;
/// Number of trailing tokens retained when a changed side is elided.
const ELISION_TAIL: usize = 40;
/// Number of formatting-only entries listed in the text report.
const FORMATTING_TEXT_LIMIT: usize = 20;

/// One extracted input, with every extraction issue retained as evidence.
pub(crate) struct Side {
    pub(crate) stream: FlowStream,
    pub(crate) issues: Vec<ExtractionIssue>,
    /// Number of paint-gate issues at the end of `issues`. These are the only
    /// issues that mean evidence was not examined rather than broken.
    pub(crate) paint_issues: usize,
    pub(crate) order: Vec<PageOrderEvidence>,
    pub(crate) image_identity_pages: BTreeSet<u64>,
    pub(crate) image_identity_rules: BTreeMap<u64, &'static str>,
    pub(crate) glyph_name_identity_pages: BTreeSet<u64>,
}

impl Side {
    pub(crate) fn is_complete(&self) -> bool {
        self.issues.is_empty() && self.stream.conservation.is_complete()
    }

    /// Evidence that was examined but cannot be trusted. Paint-gate issues are
    /// excluded because they mean the painted material was not examined.
    pub(crate) fn has_broken_evidence(&self) -> bool {
        !self.stream.conservation.is_complete() || self.issues.len() > self.paint_issues
    }

    /// The M4b paint gate found that text drawn as paint was not examined.
    pub(crate) fn has_unexamined_paint(&self) -> bool {
        self.paint_issues > 0
    }
}

/// Extraction incompleteness grouped by page, plus issues that cannot be tied
/// to one page.
#[derive(Default)]
struct IncompleteSummary {
    pages: BTreeMap<u64, BTreeSet<&'static str>>,
    unscoped: BTreeSet<&'static str>,
    conservation_incomplete: bool,
}

impl IncompleteSummary {
    fn of(side: &Side) -> Self {
        let mut summary = Self {
            conservation_incomplete: !side.stream.conservation.is_complete(),
            ..Self::default()
        };
        for issue in &side.issues {
            let kind = issue_kind_label(issue.kind());
            match issue.scope() {
                ExtractionScope::Page(page) => {
                    summary
                        .pages
                        .entry(u64::from(page.0) + 1)
                        .or_default()
                        .insert(kind);
                }
                ExtractionScope::PageGlyphGap { page, .. } => {
                    summary
                        .pages
                        .entry(u64::from(page.0) + 1)
                        .or_default()
                        .insert(kind);
                }
                _ => {
                    summary.unscoped.insert(kind);
                }
            }
        }
        summary
    }

    fn is_empty(&self) -> bool {
        self.pages.is_empty() && self.unscoped.is_empty() && !self.conservation_incomplete
    }

    /// Whether a change covering these pages cannot be fully trusted.
    ///
    /// Issues without a page, and incomplete glyph conservation, make every
    /// page potentially affected, so they mark every change.
    fn touches(&self, pages: &[u64]) -> bool {
        if self.unscoped.is_empty() && !self.conservation_incomplete {
            return pages.iter().any(|page| self.pages.contains_key(page));
        }
        true
    }
}

fn uncertain_order_pages(side: &Side) -> BTreeSet<u64> {
    side.order
        .iter()
        .filter(|page| page.uncertain)
        .map(|page| u64::from(page.page.0) + 1)
        .collect()
}

fn uncertain_order_page_ids(side: &Side) -> BTreeSet<PageId> {
    side.order
        .iter()
        .filter(|page| page.uncertain)
        .map(|page| page.page)
        .collect()
}

fn issue_kind_label(kind: ExtractionIssueKind) -> &'static str {
    match kind {
        ExtractionIssueKind::Unsupported => "unsupported",
        ExtractionIssueKind::Unresolved => "unresolved",
    }
}

/// Distinguishes temporary candidate directories within one process.
static CACHE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A private per-run directory for cached candidate and source streams.
///
/// The directory is created with owner-only permissions and removed when it
/// goes out of scope, including every early error return.
pub(crate) struct CacheDirectory(PathBuf);

impl CacheDirectory {
    pub(crate) fn create() -> Result<Self, FlowError> {
        let sequence = CACHE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("pdfdelta-flow-{}-{sequence}", std::process::id()));
        std::fs::create_dir(&path).map_err(|error| {
            FlowError::backend(format!("cannot create the flow cache directory: {error}"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(
                |error| {
                    FlowError::backend(format!("cannot protect the flow cache directory: {error}"))
                },
            )?;
        }
        Ok(Self(path))
    }

    pub(crate) fn file(&self, side: &str) -> PathBuf {
        self.0.join(format!("{side}.cache"))
    }
}

impl Drop for CacheDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Extraction evidence retained after a side's glyph document is consumed.
struct SideMeta {
    /// Exact input bytes, retained when a review bundle will publish them.
    bytes: Option<std::sync::Arc<[u8]>>,
    issues: Vec<ExtractionIssue>,
    image_issue_pages: BTreeSet<PageId>,
    encoded_image_pages: BTreeMap<PageId, String>,
    image_input: Option<ImageInput>,
    glyph_name_identity_pages: BTreeSet<u64>,
    clip_uncertain_pages: BTreeSet<PageId>,
}

impl SideMeta {
    fn empty() -> Self {
        Self {
            bytes: None,
            issues: Vec::new(),
            image_issue_pages: BTreeSet::new(),
            encoded_image_pages: BTreeMap::new(),
            image_input: None,
            glyph_name_identity_pages: BTreeSet::new(),
            clip_uncertain_pages: BTreeSet::new(),
        }
    }
}

/// What one side keeps after extraction.
#[derive(Clone, Copy)]
struct SideRetention {
    /// Per-token source glyph ids.
    sources: bool,
    /// Per-glyph records for a full report.
    source_glyphs: bool,
    /// Exact acquired bytes for a review bundle.
    input: bool,
}

/// Builds one side's reading-order hypotheses in this process.
fn prepare_side(
    path: &Path,
    options: &FlowSideOptions<'_>,
    retention: SideRetention,
    source_directory: Option<&Arc<CacheDirectory>>,
    mut trace: Option<&mut FlowTrace>,
    side_name: TraceSide,
) -> Result<
    (
        FlowCandidates,
        SideMeta,
        Option<crate::flow_sources::SourceCache>,
    ),
    FlowError,
> {
    let extracted = extract_side(
        path,
        options,
        retention.input,
        trace.as_deref_mut(),
        side_name,
    )?;
    let ExtractedSide {
        document,
        bytes,
        issues,
        image_issue_pages,
        encoded_image_pages,
        image_input,
        glyph_name_identity_pages,
        clip_uncertain_pages,
    } = extracted;
    let start = std::time::Instant::now();
    let (candidates, source_cache) = if retention.source_glyphs {
        let directory = source_directory.ok_or_else(|| {
            FlowError::backend("a full report needs a source cache directory for this side")
        })?;
        let name = match side_name {
            TraceSide::Old => "old",
            TraceSide::New => "new",
        };
        let source_path = directory.file(&format!("{name}.sources"));
        let mut writer =
            crate::flow_sources::SourceWriter::create(&source_path, Some(document.items()))
                .map_err(FlowError::backend)?;
        let candidates = pdfdelta_flow::flow_candidates_owned_with_sources(document, true)
            .map_err(|error| {
                FlowError::from(error).with_context("failed to reconstruct reading order")
            })?;
        writer
            .write_lists(&candidates)
            .map_err(FlowError::backend)?;
        writer.finish().map_err(FlowError::backend)?;
        let (cache, _) =
            crate::flow_sources::SourceCache::open(&source_path, Arc::clone(directory))
                .map_err(FlowError::backend)?;
        (candidates, Some(cache))
    } else if retention.sources {
        let candidates = pdfdelta_flow::flow_candidates_owned_with_sources(document, true)
            .map_err(|error| {
                FlowError::from(error).with_context("failed to reconstruct reading order")
            })?;
        (candidates, None)
    } else {
        let candidates = flow_candidates_owned(document).map_err(|error| {
            FlowError::from(error).with_context("failed to reconstruct reading order")
        })?;
        (candidates, None)
    };
    if let Some(trace) = trace.as_mut() {
        trace.complete(
            "tokenization_normalization",
            Some(side_name),
            [
                ("tokens", candidates.token_count()),
                ("issues", issues.len()),
                (
                    "conservation_incomplete",
                    usize::from(!candidates.conservation_complete()),
                ),
            ],
            start.elapsed(),
        );
    }
    Ok((
        candidates,
        SideMeta {
            bytes,
            issues,
            image_issue_pages,
            encoded_image_pages,
            image_input,
            glyph_name_identity_pages,
            clip_uncertain_pages,
        },
        source_cache,
    ))
}

/// Builds one side in a child process and returns its cache file.
///
/// Returns `Ok(None)` when the child reports that the side carries evidence
/// the compact cache does not represent, so the caller rebuilds both sides in
/// process.
fn build_cached_side(
    path: &Path,
    limit_scale: f64,
    cache: &Path,
    sources: Option<&Path>,
    retain_source_glyphs: bool,
) -> Result<bool, FlowError> {
    let executable = std::env::current_exe().map_err(|error| {
        FlowError::backend(format!("cannot locate the pdfdelta executable: {error}"))
    })?;
    let scale = limit_scale.to_string();
    let mut command = Command::new(executable);
    command
        .arg("flow")
        .arg(path)
        .arg(path)
        .arg("--limit-scale")
        .arg(&scale)
        .env("PDFDELTA_FLOW_BUILD_OUTPUT", cache)
        .env("PDFDELTA_FLOW_BUILD_SCALE", &scale);
    if let Some(sources) = sources {
        command.env("PDFDELTA_FLOW_BUILD_SOURCES", sources).env(
            "PDFDELTA_FLOW_BUILD_SOURCE_GLYPHS",
            if retain_source_glyphs { "1" } else { "0" },
        );
    }
    let status = command.status().map_err(|error| {
        FlowError::backend(format!("cannot start the flow candidate builder: {error}"))
    })?;
    if status.code() == Some(4) {
        return Ok(false);
    }
    if !status.success() {
        return Err(FlowError::backend(format!(
            "flow candidate builder exited with status {status}; no comparison was produced"
        )));
    }
    Ok(true)
}

/// Loads a cache file produced by [`build_cached_side`].
fn load_cached_side(path: &Path) -> Result<FlowCandidates, FlowError> {
    let file = File::open(path).map_err(|error| {
        FlowError::backend(format!("cannot open cached flow candidates: {error}"))
    })?;
    FlowCandidates::read_cache(BufReader::new(file))
        .map_err(|error| FlowError::backend(format!("cannot read cached flow candidates: {error}")))
}

/// Writes one side's candidates for the parent flow process.
///
/// Returns exit code `4` when the extracted side has image, issue, or
/// glyph-name evidence that the compact cache does not represent.
fn build_candidate_cache(path: &Path, output: &Path, limit_scale: f64) -> Result<u8, FlowError> {
    #[cfg(debug_assertions)]
    if std::env::var_os("PDFDELTA_FLOW_BUILD_FAIL").is_some() {
        return Err(FlowError::backend(
            "candidate cache build failed for this test",
        ));
    }
    let limits = ParseLimits::default();
    let extraction_limits = ExtractionLimits::default()
        .scaled(limit_scale)
        .map_err(|error| FlowError::from(error).with_context("invalid flow limit scale"))?;
    let fonts = ExternalFontIdentities::default();
    let options = FlowSideOptions {
        limits,
        extraction_limits,
        password: None,
        font_identities: &fonts,
        cache: None,
    };
    let extracted = extract_side(path, &options, false, None, TraceSide::Old)?;
    if !extracted.issues.is_empty()
        || extracted.image_input.is_some()
        || !extracted.encoded_image_pages.is_empty()
        || !extracted.glyph_name_identity_pages.is_empty()
    {
        return Ok(4);
    }
    let source_output = std::env::var_os("PDFDELTA_FLOW_BUILD_SOURCES").map(PathBuf::from);
    let retain_source_glyphs =
        std::env::var_os("PDFDELTA_FLOW_BUILD_SOURCE_GLYPHS").is_some_and(|value| value == "1");
    let mut source_writer = match &source_output {
        Some(path) => Some(
            crate::flow_sources::SourceWriter::create(
                Path::new(path),
                retain_source_glyphs.then(|| extracted.document.items()),
            )
            .map_err(FlowError::backend)?,
        ),
        None => None,
    };
    let candidates = if source_output.is_some() {
        pdfdelta_flow::flow_candidates_owned_with_sources(extracted.document, true).map_err(
            |error| FlowError::from(error).with_context("failed to reconstruct reading order"),
        )?
    } else {
        flow_candidates_owned(extracted.document).map_err(|error| {
            FlowError::from(error).with_context("failed to reconstruct reading order")
        })?
    };
    if let Some(writer) = &mut source_writer {
        writer
            .write_lists(&candidates)
            .map_err(FlowError::backend)?;
    }
    if let Some(writer) = source_writer {
        writer.finish().map_err(FlowError::backend)?;
    }
    let file = File::create(output).map_err(|error| {
        FlowError::backend(format!("cannot create cached flow candidates: {error}"))
    })?;
    let mut writer = BufWriter::new(file);
    candidates.write_cache(&mut writer).map_err(|error| {
        FlowError::backend(format!("cannot write cached flow candidates: {error}"))
    })?;
    writer.flush().map_err(|error| {
        FlowError::backend(format!("cannot flush cached flow candidates: {error}"))
    })?;
    Ok(0)
}

/// Renders the human-readable report for one comparison.
pub(crate) fn render_text(comparison: &FlowComparison) -> String {
    let mut text = String::new();
    write_summary(
        &mut text,
        &comparison.diff,
        &comparison.stats,
        &comparison.old_side,
        &comparison.new_side,
    );
    write_changes(
        &mut text,
        &comparison.diff,
        &comparison.old_side.stream,
        &comparison.new_side.stream,
        &comparison.old_side,
        &comparison.new_side,
        &comparison.assessments,
    );
    write_formatting(
        &mut text,
        &comparison.formatting,
        &comparison.old_side.stream,
        &comparison.new_side.stream,
    );
    write_furniture(
        &mut text,
        &comparison.old_side.stream,
        &comparison.new_side.stream,
    );
    text
}

/// Runs the flow command.
///
/// Returns the process exit code: `0` for no content change, `1` for content
/// changes, and `3` when either side has extraction issues. Fatal problems are
/// returned as errors (the caller maps them to exit code `2`).
///
/// # Errors
///
/// Returns a message when an input cannot be read, parsed, or extracted, when
/// the flow diff fails verification, or when the report cannot be written.
/// Per-side extraction inputs for the flow engine.
pub(crate) struct FlowSideOptions<'a> {
    pub(crate) limits: ParseLimits,
    pub(crate) extraction_limits: ExtractionLimits,
    pub(crate) password: Option<&'a str>,
    pub(crate) font_identities: &'a ExternalFontIdentities,
    pub(crate) cache: Option<&'a crate::extraction_cache::ExtractionCache>,
}

/// Inputs for one flow comparison.
pub(crate) struct FlowComparisonOptions<'a> {
    pub(crate) limit_scale: f64,
    /// Retain the per-token source glyph ids.
    pub(crate) retain_sources: bool,
    /// Retain the per-glyph records a full report serializes.
    pub(crate) retain_source_glyphs: bool,
    /// Retain the exact acquired bytes for a review bundle.
    pub(crate) retain_input: bool,
    pub(crate) old_password: Option<&'a str>,
    pub(crate) new_password: Option<&'a str>,
    pub(crate) old_font_identities: &'a ExternalFontIdentities,
    pub(crate) new_font_identities: &'a ExternalFontIdentities,
    pub(crate) cache: Option<&'a crate::extraction_cache::ExtractionCache>,
}

/// One verified flow comparison shared by `pdfdelta flow` and `--engine flow`.
pub(crate) struct FlowComparison {
    pub(crate) diff: FlowDiff,
    pub(crate) stats: FlowStats,
    pub(crate) old_side: Side,
    pub(crate) new_side: Side,
    /// Exact input bytes, retained when a review bundle publishes them.
    pub(crate) old_bytes: Option<std::sync::Arc<[u8]>>,
    pub(crate) new_bytes: Option<std::sync::Arc<[u8]>>,
    pub(crate) assessments: Vec<ChangeAssessment>,
    pub(crate) formatting: Vec<FormattingChange>,
    /// Source caches for a full report.
    pub(crate) old_source_cache: Option<crate::flow_sources::SourceCache>,
    pub(crate) new_source_cache: Option<crate::flow_sources::SourceCache>,
}

impl FlowComparison {
    pub(crate) fn text_complete(&self) -> bool {
        self.old_side.is_complete() && self.new_side.is_complete()
    }

    pub(crate) fn text_broken_evidence(&self) -> bool {
        self.old_side.has_broken_evidence() || self.new_side.has_broken_evidence()
    }

    pub(crate) fn text_unexamined_evidence(&self) -> bool {
        self.old_side.has_unexamined_paint() || self.new_side.has_unexamined_paint()
    }

    pub(crate) fn non_layout_changes(&self) -> usize {
        self.stats.delete.changes
            + self.stats.insert.changes
            + self.stats.replace.changes
            + self.stats.moves.changes
    }
}

/// Runs the shared flow pipeline without rendering a report.
pub(crate) fn compare_flow(
    old_path: &Path,
    new_path: &Path,
    options: &FlowComparisonOptions<'_>,
    mut trace: Option<&mut FlowTrace>,
) -> Result<FlowComparison, FlowError> {
    if old_path == Path::new("-") && new_path == Path::new("-") {
        return Err(FlowError::backend(
            "cannot read both OLD_PDF and NEW_PDF from standard input",
        ));
    }
    let limit_scale = options.limit_scale;
    let old_password = options.old_password;
    let new_password = options.new_password;
    let old_font_identities = options.old_font_identities;
    let new_font_identities = options.new_font_identities;
    let cache = options.cache;
    let limits = ParseLimits::default();
    let extraction_limits = ExtractionLimits::default()
        .scaled(limit_scale)
        .map_err(|error| FlowError::from(error).with_context("invalid flow limit scale"))?;
    // Raised extraction limits mean a document large enough that the second
    // side's extraction must not overlap the first side's normalization
    // buffers. Build both sides in child processes, then load the compact
    // candidate streams; the parent holds no glyph document or candidate data
    // while a child is extracting.
    let default_fonts = ExternalFontIdentities::default();
    let use_cache = !options.retain_input
        && trace.is_none()
        && limit_scale > 1.0
        && old_password.is_none()
        && new_password.is_none()
        && *old_font_identities == default_fonts
        && *new_font_identities == default_fonts
        && cache.is_none();
    let old_options = FlowSideOptions {
        limits,
        extraction_limits,
        password: old_password,
        font_identities: old_font_identities,
        cache,
    };
    let new_options = FlowSideOptions {
        limits,
        extraction_limits,
        password: new_password,
        font_identities: new_font_identities,
        cache,
    };
    let cached = if use_cache {
        let directory = std::sync::Arc::new(CacheDirectory::create()?);
        let old_cache = directory.file("old");
        let new_cache = directory.file("new");
        let old_sources = options
            .retain_sources
            .then(|| directory.file("old.sources"));
        let new_sources = options
            .retain_sources
            .then(|| directory.file("new.sources"));
        let old_built = build_cached_side(
            old_path,
            limit_scale,
            &old_cache,
            old_sources.as_deref(),
            options.retain_source_glyphs,
        )?;
        let new_built = build_cached_side(
            new_path,
            limit_scale,
            &new_cache,
            new_sources.as_deref(),
            options.retain_source_glyphs,
        )?;
        if old_built && new_built {
            let mut old_candidates = load_cached_side(&old_cache)?;
            let mut new_candidates = load_cached_side(&new_cache)?;
            let (old_source_cache, new_source_cache) = match (&old_sources, &new_sources) {
                (Some(old_path), Some(new_path)) => {
                    let (old_cache, old_lists) =
                        crate::flow_sources::SourceCache::open(old_path, directory.clone())
                            .map_err(FlowError::backend)?;
                    let (new_cache, new_lists) =
                        crate::flow_sources::SourceCache::open(new_path, directory.clone())
                            .map_err(FlowError::backend)?;
                    let [old_layout, old_stream, old_baseline] = old_lists;
                    old_candidates
                        .attach_sources(old_layout, old_stream, old_baseline)
                        .map_err(FlowError::from)?;
                    let [new_layout, new_stream, new_baseline] = new_lists;
                    new_candidates
                        .attach_sources(new_layout, new_stream, new_baseline)
                        .map_err(FlowError::from)?;
                    (
                        options.retain_source_glyphs.then_some(old_cache),
                        options.retain_source_glyphs.then_some(new_cache),
                    )
                }
                _ => (None, None),
            };
            Some((
                old_candidates,
                new_candidates,
                old_source_cache,
                new_source_cache,
            ))
        } else {
            // A side with image, issue, or glyph-name evidence is not
            // represented by the compact cache; rebuild both sides in process.
            None
        }
    } else {
        None
    };
    let (old_candidates, old_meta, new_candidates, new_meta, old_source_cache, new_source_cache) =
        if let Some((old, new, old_sources, new_sources)) = cached {
            (
                old,
                SideMeta::empty(),
                new,
                SideMeta::empty(),
                old_sources,
                new_sources,
            )
        } else {
            let source_directory = if options.retain_source_glyphs {
                Some(Arc::new(CacheDirectory::create()?))
            } else {
                None
            };
            let retention = SideRetention {
                sources: options.retain_sources,
                source_glyphs: options.retain_source_glyphs,
                input: options.retain_input,
            };
            let (old_candidates, old_meta, old_source_cache) = prepare_side(
                old_path,
                &old_options,
                retention,
                source_directory.as_ref(),
                trace.as_deref_mut(),
                TraceSide::Old,
            )?;
            let (new_candidates, new_meta, new_source_cache) = prepare_side(
                new_path,
                &new_options,
                retention,
                source_directory.as_ref(),
                trace.as_deref_mut(),
                TraceSide::New,
            )?;
            (
                old_candidates,
                old_meta,
                new_candidates,
                new_meta,
                old_source_cache,
                new_source_cache,
            )
        };
    let old_paint = old_candidates.paint_inventory().map(<[_]>::to_vec);
    let new_paint = new_candidates.paint_inventory().map(<[_]>::to_vec);
    let (old_paint_issues, new_paint_issues) =
        paint_issues(old_paint.as_deref(), new_paint.as_deref())?;
    let SideMeta {
        bytes: old_bytes,
        issues: old_issues,
        image_issue_pages: old_image_issue_pages,
        encoded_image_pages: old_encoded_image_pages,
        image_input: old_image_input,
        glyph_name_identity_pages: old_glyph_name_pages,
        clip_uncertain_pages: old_clip_pages,
    } = old_meta;
    let SideMeta {
        bytes: new_bytes,
        issues: new_issues,
        image_issue_pages: new_image_issue_pages,
        encoded_image_pages: new_encoded_image_pages,
        image_input: new_image_input,
        glyph_name_identity_pages: new_glyph_name_pages,
        clip_uncertain_pages: new_clip_pages,
    } = new_meta;

    let images = resolve_image_pages(
        &ImageSide {
            issue_pages: old_image_issue_pages,
            encoded: old_encoded_image_pages,
            input: old_image_input,
        },
        &ImageSide {
            issue_pages: new_image_issue_pages,
            encoded: new_encoded_image_pages,
            input: new_image_input,
        },
    );
    let start = std::time::Instant::now();
    let pair = flow_order_from_candidates(old_candidates, new_candidates).map_err(|error| {
        let error = FlowError::from(error).with_context("failed to choose reading order");
        if let Some(trace) = trace.as_mut() {
            trace.fail("furniture_order", None, &error);
        }
        error
    })?;
    if let Some(trace) = trace.as_mut() {
        let uncertain = pair
            .old_pages
            .iter()
            .chain(pair.new_pages.iter())
            .filter(|page| page.uncertain)
            .count();
        trace.complete(
            "furniture_order",
            None,
            [
                ("old_tokens", pair.old.tokens.len()),
                ("new_tokens", pair.new.tokens.len()),
                ("uncertain_pages", uncertain),
                (
                    "furniture_lines",
                    pair.old.furniture.len() + pair.new.furniture.len(),
                ),
            ],
            start.elapsed(),
        );
    }

    let mut old_stream = pair.old;
    let mut new_stream = pair.new;
    insert_image_tokens(&mut old_stream, &images.old_tokens);
    insert_image_tokens(&mut new_stream, &images.new_tokens);
    mark_uncertain_pages(&mut old_stream, &old_clip_pages);
    mark_uncertain_pages(&mut new_stream, &new_clip_pages);
    let old_image_pages = images
        .old_tokens
        .keys()
        .map(|page| u64::from(page.0) + 1)
        .collect();
    let new_image_pages = images
        .new_tokens
        .keys()
        .map(|page| u64::from(page.0) + 1)
        .collect();
    let old_paint_issue_count = old_paint_issues.len();
    let new_paint_issue_count = new_paint_issues.len();
    let mut old_issues = retained_image_issues(old_issues, &images.old_resolved);
    old_issues.extend(old_paint_issues);
    let mut new_issues = retained_image_issues(new_issues, &images.new_resolved);
    new_issues.extend(new_paint_issues);
    let old_side = Side {
        stream: old_stream,
        issues: old_issues,
        paint_issues: old_paint_issue_count,
        order: pair.old_pages,
        image_identity_pages: old_image_pages,
        image_identity_rules: images.old_rules,
        glyph_name_identity_pages: old_glyph_name_pages,
    };
    let new_side = Side {
        stream: new_stream,
        issues: new_issues,
        paint_issues: new_paint_issue_count,
        order: pair.new_pages,
        image_identity_pages: new_image_pages,
        image_identity_rules: images.new_rules,
        glyph_name_identity_pages: new_glyph_name_pages,
    };

    let start = std::time::Instant::now();
    let diff = flow_diff(&old_side.stream.tokens, &new_side.stream.tokens).map_err(|error| {
        let error = FlowError::backend(format!("flow diff failed verification: {error}"));
        if let Some(trace) = trace.as_mut() {
            trace.fail("flow_diff", None, &error);
        }
        error
    })?;
    let stats = diff.stats();
    if let Some(trace) = trace.as_mut() {
        trace.complete(
            "flow_diff",
            None,
            [
                ("changes", diff.changes.len()),
                ("delete_changes", stats.delete.changes),
                ("insert_changes", stats.insert.changes),
                ("replace_changes", stats.replace.changes),
                ("move_changes", stats.moves.changes),
                ("layout_only_changes", stats.layout_only.changes),
            ],
            start.elapsed(),
        );
    }
    let start = std::time::Instant::now();
    let assessments = assess_changes(
        &old_side.stream.tokens,
        &new_side.stream.tokens,
        &diff,
        &uncertain_order_page_ids(&old_side),
        &uncertain_order_page_ids(&new_side),
    );
    let formatting = formatting_changes(
        &old_side.stream.tokens,
        &new_side.stream.tokens,
        &diff.spine,
    );
    if let Some(trace) = trace.as_mut() {
        trace.complete(
            "assessment_formatting",
            None,
            [
                ("assessments", assessments.len()),
                ("formatting_entries", formatting.len()),
                (
                    "ambiguous_assessments",
                    assessments
                        .iter()
                        .filter(|assessment| !assessment.ambiguity.is_empty())
                        .count(),
                ),
            ],
            start.elapsed(),
        );
    }

    Ok(FlowComparison {
        diff,
        stats,
        old_side,
        new_side,
        old_bytes,
        new_bytes,
        assessments,
        formatting,
        old_source_cache,
        new_source_cache,
    })
}

/// Inputs for one standalone `pdfdelta flow` report.
#[derive(Clone, Copy)]
pub struct FlowRunOptions<'a> {
    pub json_path: Option<&'a Path>,
    pub include_tokens: bool,
    pub include_geometry: bool,
    pub limit_scale: f64,
    pub color: bool,
}

pub fn run(
    old_path: &Path,
    new_path: &Path,
    options: &FlowRunOptions<'_>,
    writer: &mut impl Write,
) -> Result<u8, FlowError> {
    let FlowRunOptions {
        json_path,
        include_tokens,
        include_geometry,
        limit_scale,
        color,
    } = *options;
    if let Some(output) = std::env::var_os("PDFDELTA_FLOW_BUILD_OUTPUT") {
        let scale = std::env::var("PDFDELTA_FLOW_BUILD_SCALE")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(limit_scale);
        return build_candidate_cache(old_path, Path::new(&output), scale);
    }

    let comparison = compare_flow(
        old_path,
        new_path,
        &FlowComparisonOptions {
            limit_scale,
            retain_sources: false,
            retain_source_glyphs: false,
            retain_input: false,
            old_password: None,
            new_password: None,
            old_font_identities: &ExternalFontIdentities::default(),
            new_font_identities: &ExternalFontIdentities::default(),
            cache: None,
        },
        None,
    )?;
    let detected = selects_difference(&comparison.stats);
    let exit = crate::flow_exit::select(
        detected,
        comparison.text_broken_evidence(),
        comparison.text_unexamined_evidence(),
    );
    let mut text = render_text(&comparison);
    if exit.basis == "detected_with_unexamined_evidence" {
        text.push('\n');
        text.push_str(DETECTED_WITH_UNEXAMINED_NOTE);
        text.push('\n');
    }
    let text = if color { paint_report(&text) } else { text };
    writer
        .write_all(text.as_bytes())
        .map_err(|error| FlowError::backend(format!("failed to write flow report: {error}")))?;

    if let Some(path) = json_path {
        let report = json_report(&comparison, include_geometry, exit);
        write_output_atomically(path, "flow report", |output| {
            serde_json::to_writer(output, &report)
                .map_err(|error| format!("failed to serialize flow report: {error}"))
        })
        .map_err(FlowError::backend)?;
        if include_tokens {
            write_token_sidecars(
                path,
                &comparison.old_side.stream,
                &comparison.new_side.stream,
            )?;
        }
    }

    Ok(exit.code)
}

/// One line added when a detected difference coexists with unexamined evidence.
pub(crate) const DETECTED_WITH_UNEXAMINED_NOTE: &str =
    "difference detected, but some evidence was not examined (non-text paint or visual evidence)";

const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const RESET: &str = "\x1b[0m";

/// Applies the report's ANSI conventions to a plain flow or multi-channel
/// report.
///
/// The flow renderer is line-oriented and has no hunk structure, so the
/// painter classifies complete lines instead of decorating individual spans.
/// Styling is presentation-only: the uncoloured text is unchanged, and a
/// report that contains no styled line is returned unchanged. Every style is
/// reset before the newline so a caller cannot leak colour into the next line
/// or into a pipe consumed without a terminal reset.
pub(crate) fn paint_report(text: &str) -> String {
    let mut painted = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |body| (body, "\n"));
        match report_line_color(body) {
            Some(color) => {
                painted.push_str(color);
                painted.push_str(body);
                painted.push_str(RESET);
            }
            None => painted.push_str(body),
        }
        painted.push_str(newline);
    }
    painted
}

/// Chooses a style for one report line: removals red, additions green, warnings
/// yellow, moves magenta, and section headers bold.
fn report_line_color(line: &str) -> Option<&'static str> {
    let content = line.trim_start();
    if content.is_empty() {
        return None;
    }
    if content.starts_with("flow summary")
        || content.starts_with("text engine: flow")
        || content.starts_with("Document comparison:")
        || content.starts_with("Text channel (flow):")
        || content.starts_with("comparison scope:")
    {
        return Some(BOLD);
    }
    if content.starts_with("difference detected")
        || content.contains("incomplete")
        || content.contains("unresolved")
        || content.starts_with("! extraction issue")
    {
        return Some(YELLOW);
    }
    if content.starts_with("delete:")
        || content.starts_with("old:")
        || content.starts_with("- ")
        || content.contains("removed")
    {
        return Some(RED);
    }
    if content.starts_with("insert:")
        || content.starts_with("new:")
        || content.starts_with("+ ")
        || content.contains("inserted")
    {
        return Some(GREEN);
    }
    if content.starts_with("replace:") || content.starts_with("move:") {
        return Some(MAGENTA);
    }
    if content.split_once('.').is_some_and(|(number, rest)| {
        !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit()) && !rest.is_empty()
    }) {
        return Some(BOLD);
    }
    None
}

/// Native flow status for reports. A detected change stays detected even when
/// paint evidence is unexamined.
pub(crate) fn difference_status(comparison: &FlowComparison, detected: bool) -> &'static str {
    if detected {
        "detected"
    } else if comparison.text_complete() {
        "no_content_change"
    } else {
        "indeterminate"
    }
}

/// Reads, parses, and extracts one PDF with the same limits as the comparison
/// pipeline.
fn extract_side(
    path: &Path,
    options: &FlowSideOptions<'_>,
    retain_input: bool,
    mut trace: Option<&mut FlowTrace>,
    side: TraceSide,
) -> Result<ExtractedSide, FlowError> {
    let start = std::time::Instant::now();
    let bytes = match read_limited_typed(path, options.limits.max_input_bytes) {
        Ok(bytes) => bytes,
        Err(error) => {
            let error =
                input_read_flow_error(error, "PDF input bytes", options.limits.max_input_bytes);
            if let Some(trace) = trace.as_mut() {
                trace.fail("input_reading", Some(side), &error);
            }
            return Err(error);
        }
    };
    let input_bytes = bytes.len();
    if let Some(trace) = trace.as_mut() {
        trace.complete(
            "input_reading",
            Some(side),
            [("input_bytes", input_bytes)],
            start.elapsed(),
        );
    }
    let image_bytes = bytes.clone();
    let start = std::time::Instant::now();
    let pdf = match parse_lopdf(bytes.clone(), options.limits, options.password) {
        Ok(pdf) => pdf,
        Err(error) => {
            let error =
                FlowError::from(error).with_context(format!("failed to parse {}", path.display()));
            if let Some(trace) = trace.as_mut() {
                trace.fail("parse", Some(side), &error);
            }
            return Err(error);
        }
    };
    if let Some(trace) = trace.as_mut() {
        let pages = pdf.pages().map_or(0, |pages| pages.len());
        trace.complete("parse", Some(side), [("pages", pages)], start.elapsed());
    }
    let start = std::time::Instant::now();
    let extract = || {
        ContentStreamGlyphExtractor
            .extract_outcome_with_external_font_identities(
                pdf.as_ref(),
                options.extraction_limits,
                options.font_identities,
            )
            .map_err(|error| {
                FlowError::from(error).with_context(format!("failed to extract {}", path.display()))
            })
    };
    let outcome = match options.cache {
        Some(cache) => cache
            .get_or_extract_with_limits(
                &bytes,
                &options.limits,
                options.password,
                options.font_identities,
                &options.extraction_limits,
                extract,
            )
            .map(|(outcome, _)| outcome)
            .inspect_err(|error| {
                if let Some(trace) = trace.as_mut() {
                    trace.fail("glyph_extraction", Some(side), error);
                }
            })?,
        None => extract().inspect_err(|error| {
            if let Some(trace) = trace.as_mut() {
                trace.fail("glyph_extraction", Some(side), error);
            }
        })?,
    };
    if let Some(trace) = trace.as_mut() {
        let glyphs = outcome.document().items().len();
        let issues = outcome.issues();
        let mut metrics = issue_metrics(issues);
        metrics.insert("glyphs", glyphs);
        if let Some(issue) = issues.first() {
            let error = FlowError::from_extraction_issue(issue);
            trace.incomplete(
                "glyph_extraction",
                Some(side),
                metrics,
                start.elapsed(),
                &error,
            );
        } else {
            trace.complete("glyph_extraction", Some(side), metrics, start.elapsed());
        }
    }
    let clip_uncertain_pages = outcome
        .issues()
        .iter()
        .filter(|issue| {
            matches!(issue.kind(), ExtractionIssueKind::Unsupported)
                && issue.description().contains("clipping path")
        })
        .filter_map(issue_page)
        .collect();
    let glyph_name_identity_pages = outcome
        .glyph_name_identity_pages()
        .iter()
        .map(|page| u64::from(page.0) + 1)
        .collect();
    let (document, issues) = outcome.into_parts();
    let image_issue_pages = issues
        .iter()
        .filter(|issue| is_image_only_issue(issue))
        .filter_map(issue_page)
        .collect::<BTreeSet<_>>();
    let mut encoded_image_pages = BTreeMap::new();
    let mut image_input = None;
    if !image_issue_pages.is_empty() {
        let pages = pdf.pages().map_err(|error| {
            FlowError::from(error).with_context(format!("failed to enumerate {}", path.display()))
        })?;
        for &page in &image_issue_pages {
            let encoded =
                crate::image_hashes::collect_encoded_page(&image_bytes, &pages, page, false);
            if encoded.complete
                && let Some(hash) = encoded.sha256
            {
                encoded_image_pages.insert(page, hash);
            }
        }
        image_input = Some(ImageInput {
            bytes: image_bytes,
            pages,
        });
    }
    Ok(ExtractedSide {
        document,
        bytes: retain_input.then_some(bytes),
        issues,
        image_issue_pages,
        encoded_image_pages,
        image_input,
        glyph_name_identity_pages,
        clip_uncertain_pages,
    })
}

/// Inputs retained for the decoded-pixel fallback after both sides are known.
struct ImageInput {
    bytes: Arc<[u8]>,
    pages: Vec<PageRef>,
}

/// Image identity evidence retained after the extracted document is consumed.
struct ImageSide {
    issue_pages: BTreeSet<PageId>,
    encoded: BTreeMap<PageId, String>,
    input: Option<ImageInput>,
}

#[derive(Default)]
struct ImageResolution {
    old_tokens: BTreeMap<PageId, String>,
    new_tokens: BTreeMap<PageId, String>,
    old_resolved: BTreeSet<PageId>,
    new_resolved: BTreeSet<PageId>,
    old_rules: BTreeMap<u64, &'static str>,
    new_rules: BTreeMap<u64, &'static str>,
}

/// Resolves image-only pages with the strongest identity available on both
/// sides. Encoded hashes may certify equality only; unequal encoded evidence
/// falls back to decoded pixels or stays unresolved.
fn resolve_image_pages(old: &ImageSide, new: &ImageSide) -> ImageResolution {
    let mut result = ImageResolution::default();
    for page in old.issue_pages.intersection(&new.issue_pages).copied() {
        let old_encoded = old.encoded.get(&page);
        let new_encoded = new.encoded.get(&page);
        if let (Some(old_hash), Some(new_hash)) = (old_encoded, new_encoded)
            && old_hash == new_hash
        {
            let token = format!("⟦image-encoded:{old_hash}⟧");
            result.old_tokens.insert(page, token.clone());
            result.new_tokens.insert(page, token);
            result.old_resolved.insert(page);
            result.new_resolved.insert(page);
            result.old_rules.insert(u64::from(page.0) + 1, "encoded");
            result.new_rules.insert(u64::from(page.0) + 1, "encoded");
            continue;
        }
        let old_token = old
            .input
            .as_ref()
            .and_then(|input| decoded_image_token(input, page));
        let new_token = new
            .input
            .as_ref()
            .and_then(|input| decoded_image_token(input, page));
        if let (Some(old_token), Some(new_token)) = (old_token, new_token) {
            result.old_tokens.insert(page, old_token);
            result.new_tokens.insert(page, new_token);
            result.old_resolved.insert(page);
            result.new_resolved.insert(page);
            result.old_rules.insert(u64::from(page.0) + 1, "pixels");
            result.new_rules.insert(u64::from(page.0) + 1, "pixels");
        }
    }
    // A page present only on one side has no counterpart image page. Once its
    // own encoded or decoded identity is available, the synthetic token is a
    // complete image-page deletion/insertion rather than an unknown glyph gap.
    for page in old.issue_pages.difference(&new.issue_pages).copied() {
        if let Some((token, rule)) = one_sided_image_token(old, page) {
            result.old_tokens.insert(page, token);
            result.old_resolved.insert(page);
            result.old_rules.insert(u64::from(page.0) + 1, rule);
        }
    }
    for page in new.issue_pages.difference(&old.issue_pages).copied() {
        if let Some((token, rule)) = one_sided_image_token(new, page) {
            result.new_tokens.insert(page, token);
            result.new_resolved.insert(page);
            result.new_rules.insert(u64::from(page.0) + 1, rule);
        }
    }
    result
}

fn one_sided_image_token(side: &ImageSide, page: PageId) -> Option<(String, &'static str)> {
    if let Some(hash) = side.encoded.get(&page) {
        return Some((format!("⟦image-encoded:{hash}⟧"), "encoded"));
    }
    side.input
        .as_ref()
        .and_then(|input| decoded_image_token(input, page))
        .map(|token| (token, "pixels"))
}

fn decoded_image_token(input: &ImageInput, page: PageId) -> Option<String> {
    let inventory =
        crate::image_hashes::collect_page(input.bytes.as_ref(), &input.pages, page, false);
    image_page_token(&inventory, page)
}

fn retained_image_issues(
    mut issues: Vec<ExtractionIssue>,
    resolved: &BTreeSet<PageId>,
) -> Vec<ExtractionIssue> {
    issues.retain(|issue| {
        !is_image_only_issue(issue)
            || issue_page(issue).is_none_or(|page| !resolved.contains(&page))
    });
    issues
}

fn is_image_only_issue(issue: &ExtractionIssue) -> bool {
    issue
        .description()
        .starts_with("page has a potentially visible image and no comparison-visible native glyphs")
}

/// Number of differing pages named in one paint-difference reason.
const MAX_PAINT_PAGES_IN_REASON: usize = 12;

/// Builds one page-scoped incompleteness reason for each side when the
/// non-text paint inventory cannot prove that text drawn as paint is unchanged.
///
/// The inventory is a multiset keyed only by page-relative identity, so an
/// identical page moved to another page number stays equal. Any missing
/// identity, count mismatch, or item mismatch disables the completeness claim.
fn paint_issues(
    old: Option<&[PaintIdentity]>,
    new: Option<&[PaintIdentity]>,
) -> Result<(Vec<ExtractionIssue>, Vec<ExtractionIssue>), FlowError> {
    let (old_summary, new_summary) = match (old, new) {
        (Some(old), Some(new)) if paint_equal(old, new) => {
            return Ok((Vec::new(), Vec::new()));
        }
        (Some(old), Some(new)) => (paint_side_summary(old, new), paint_side_summary(new, old)),
        _ => (
            PaintSideSummary::unavailable(),
            PaintSideSummary::unavailable(),
        ),
    };
    let mut issues = Vec::with_capacity(2);
    for (side, summary) in [("old", old_summary), ("new", new_summary)] {
        let description = format!(
            "non-text paint differs; text drawn as paths or images was not examined ({side} side: {}; {}{})",
            summary.unmatched_items.map_or_else(
                || "inventory unavailable".to_owned(),
                |count| format!("{count} unmatched paint items")
            ),
            if summary.pages.is_empty() {
                "pages unavailable".to_owned()
            } else {
                format!("pages {}", bounded_pages(&summary.pages))
            },
            if summary.truncated_pages > 0 {
                format!(" (+{} more pages)", summary.truncated_pages)
            } else {
                String::new()
            }
        );
        issues.push(
            ExtractionIssue::new(
                ExtractionIssueKind::Unresolved,
                ExtractionScope::Document,
                description,
            )
            .map_err(|error| {
                FlowError::from(error).with_context("cannot record paint incompleteness")
            })?,
        );
    }
    let new_issue = issues.pop().expect("two paint issues were just built");
    let old_issue = issues.pop().expect("two paint issues were just built");
    Ok((vec![old_issue], vec![new_issue]))
}

/// Whether two paint inventories certify that all painted content is unchanged.
fn paint_equal(old: &[PaintIdentity], new: &[PaintIdentity]) -> bool {
    if old.iter().chain(new).any(|paint| paint.identity.is_none()) {
        return false;
    }
    let mut counts = BTreeMap::<&str, i64>::new();
    for paint in old {
        if let Some(identity) = paint.identity.as_deref() {
            *counts.entry(identity).or_default() += 1;
        }
    }
    for paint in new {
        if let Some(identity) = paint.identity.as_deref() {
            *counts.entry(identity).or_default() -= 1;
        }
    }
    counts.values().all(|count| *count == 0)
}

struct PaintSideSummary {
    unmatched_items: Option<usize>,
    pages: Vec<u64>,
    truncated_pages: usize,
}

impl PaintSideSummary {
    fn unavailable() -> Self {
        Self {
            unmatched_items: None,
            pages: Vec::new(),
            truncated_pages: 0,
        }
    }
}

/// Summarizes the excess occurrences of each paint identity on one side.
fn paint_side_summary(paints: &[PaintIdentity], other: &[PaintIdentity]) -> PaintSideSummary {
    let mut own_counts = BTreeMap::<&str, usize>::new();
    let mut other_counts = BTreeMap::<&str, usize>::new();
    for paint in paints {
        if let Some(identity) = paint.identity.as_deref() {
            *own_counts.entry(identity).or_default() += 1;
        }
    }
    for paint in other {
        if let Some(identity) = paint.identity.as_deref() {
            *other_counts.entry(identity).or_default() += 1;
        }
    }
    let excess = own_counts
        .iter()
        .filter_map(|(identity, count)| {
            let missing = count.saturating_sub(*other_counts.get(identity).unwrap_or(&0));
            (missing > 0).then_some((*identity, missing))
        })
        .collect::<BTreeMap<_, _>>();
    let mut unmatched_items = excess.values().copied().sum::<usize>();
    let mut pages = BTreeSet::new();
    for paint in paints {
        match paint.identity.as_deref() {
            Some(identity) if excess.contains_key(identity) => {
                pages.insert(u64::from(paint.page.0) + 1);
            }
            Some(_) => {}
            None => {
                unmatched_items += 1;
                pages.insert(u64::from(paint.page.0) + 1);
            }
        }
    }
    let pages = pages.into_iter().collect::<Vec<_>>();
    let truncated = pages.len().saturating_sub(MAX_PAINT_PAGES_IN_REASON);
    PaintSideSummary {
        unmatched_items: Some(unmatched_items),
        pages: pages.into_iter().take(MAX_PAINT_PAGES_IN_REASON).collect(),
        truncated_pages: truncated,
    }
}

fn bounded_pages(pages: &[u64]) -> String {
    pages
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn issue_page(issue: &ExtractionIssue) -> Option<PageId> {
    match issue.scope() {
        ExtractionScope::Page(page) | ExtractionScope::PageGlyphGap { page, .. } => Some(page),
        _ => None,
    }
}

fn image_page_token(inventory: &ImageInventory, page: PageId) -> Option<String> {
    if !inventory.complete {
        return None;
    }
    let mut images = inventory
        .images
        .iter()
        .filter(|image| image.page == page)
        .collect::<Vec<_>>();
    if images.is_empty() {
        return None;
    }
    images.sort_unstable_by_key(|image| image.occurrence);
    let mut hash = Sha256::new();
    hash.update(b"flow-image-page-v1\0");
    for image in images {
        let image_hash = image.sha256.as_ref()?;
        hash.update(image.width.to_be_bytes());
        hash.update(image.height.to_be_bytes());
        hash.update(image_hash.as_bytes());
    }
    Some(format!(
        "⟦image:{}⟧",
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn mark_uncertain_pages(stream: &mut FlowStream, pages: &BTreeSet<PageId>) {
    for token in &mut stream.tokens {
        if pages.contains(&token.page) {
            token.uncertain = true;
        }
    }
}

fn insert_image_tokens(stream: &mut FlowStream, image_tokens: &BTreeMap<PageId, String>) {
    for (page, text) in image_tokens {
        if stream.tokens.iter().any(|token| token.page == *page) {
            continue;
        }
        let position = stream.tokens.partition_point(|token| token.page.0 < page.0);
        if !stream.source_glyphs.is_empty() {
            stream.source_glyphs.insert(position, Vec::new());
        }
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

/// One extracted input before reading-order selection.
struct ExtractedSide {
    document: Document<Glyph>,
    /// Exact bytes read for this side, when a bundle will publish them.
    bytes: Option<std::sync::Arc<[u8]>>,
    issues: Vec<ExtractionIssue>,
    image_issue_pages: BTreeSet<PageId>,
    encoded_image_pages: BTreeMap<PageId, String>,
    image_input: Option<ImageInput>,
    glyph_name_identity_pages: BTreeSet<u64>,
    clip_uncertain_pages: BTreeSet<PageId>,
}

fn write_summary(
    text: &mut String,
    diff: &FlowDiff,
    stats: &FlowStats,
    old_side: &Side,
    new_side: &Side,
) {
    writeln!(text, "flow summary").expect("writing to a String cannot fail");
    writeln!(text, "  old: {} tokens", stats.old_tokens).expect("writing to a String cannot fail");
    writeln!(text, "  new: {} tokens", stats.new_tokens).expect("writing to a String cannot fail");
    writeln!(text, "  equal: {} tokens", stats.equal_tokens)
        .expect("writing to a String cannot fail");
    writeln!(text, "  delete: {}", describe_change_stats(&stats.delete))
        .expect("writing to a String cannot fail");
    writeln!(text, "  insert: {}", describe_change_stats(&stats.insert))
        .expect("writing to a String cannot fail");
    writeln!(text, "  replace: {}", describe_change_stats(&stats.replace))
        .expect("writing to a String cannot fail");
    writeln!(text, "  move: {}", describe_change_stats(&stats.moves))
        .expect("writing to a String cannot fail");
    writeln!(
        text,
        "  layout-only: {}",
        describe_change_stats(&stats.layout_only)
    )
    .expect("writing to a String cannot fail");
    let content_changes = stats.delete.changes + stats.insert.changes + stats.replace.changes;
    writeln!(text, "  content changes: {content_changes}")
        .expect("writing to a String cannot fail");
    writeln!(text, "  moved changes: {}", stats.moves.changes)
        .expect("writing to a String cannot fail");
    writeln!(
        text,
        "  detected changes: {}",
        content_changes + stats.moves.changes
    )
    .expect("writing to a String cannot fail");
    writeln!(text, "  changes: {}", diff.changes.as_slice().len())
        .expect("writing to a String cannot fail");
    writeln!(text, "  old extraction: {}", describe_extraction(old_side))
        .expect("writing to a String cannot fail");
    writeln!(text, "  new extraction: {}", describe_extraction(new_side))
        .expect("writing to a String cannot fail");
    write_incomplete_pages(text, "old", &IncompleteSummary::of(old_side));
    write_incomplete_pages(text, "new", &IncompleteSummary::of(new_side));
    write_order_summary(text, "old", &old_side.order);
    write_order_summary(text, "new", &new_side.order);
    write_image_identity_pages(
        text,
        "old",
        &old_side.image_identity_pages,
        &old_side.image_identity_rules,
    );
    write_image_identity_pages(
        text,
        "new",
        &new_side.image_identity_pages,
        &new_side.image_identity_rules,
    );
    write_glyph_name_identity_pages(text, "old", &old_side.glyph_name_identity_pages);
    write_glyph_name_identity_pages(text, "new", &new_side.glyph_name_identity_pages);
    writeln!(
        text,
        "  old glyph conservation: {}/{}",
        old_side.stream.conservation.used, old_side.stream.conservation.visible
    )
    .expect("writing to a String cannot fail");
    writeln!(
        text,
        "  new glyph conservation: {}/{}",
        new_side.stream.conservation.used, new_side.stream.conservation.visible
    )
    .expect("writing to a String cannot fail");
    writeln!(
        text,
        "  furniture: {} old, {} new masked lines",
        old_side.stream.furniture.len(),
        new_side.stream.furniture.len()
    )
    .expect("writing to a String cannot fail");
}

fn write_incomplete_pages(text: &mut String, side: &str, summary: &IncompleteSummary) {
    if summary.is_empty() {
        return;
    }
    let mut entries = summary
        .pages
        .iter()
        .map(|(page, kinds)| {
            format!(
                "{page} [{}]",
                kinds.iter().copied().collect::<Vec<_>>().join(",")
            )
        })
        .collect::<Vec<_>>();
    if !summary.unscoped.is_empty() {
        entries.push(format!(
            "document-wide [{}]",
            summary
                .unscoped
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if summary.conservation_incomplete {
        entries.push("conservation [incomplete]".to_owned());
    }
    writeln!(text, "  {side} incomplete pages: {}", entries.join(", "))
        .expect("writing to a String cannot fail");
}

fn write_image_identity_pages(
    text: &mut String,
    side: &str,
    pages: &BTreeSet<u64>,
    rules: &BTreeMap<u64, &'static str>,
) {
    if pages.is_empty() {
        return;
    }
    let encoded = rules.values().filter(|rule| **rule == "encoded").count();
    let pixels = rules.values().filter(|rule| **rule == "pixels").count();
    writeln!(
        text,
        "  {side} image-identity pages: {} (encoded: {encoded}, pixels: {pixels})",
        pages
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
    .expect("writing to a String cannot fail");
}

fn write_glyph_name_identity_pages(text: &mut String, side: &str, pages: &BTreeSet<u64>) {
    if pages.is_empty() {
        return;
    }
    writeln!(
        text,
        "  {side} glyph-name identity pages: {}",
        pages
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
    .expect("writing to a String cannot fail");
}

fn write_order_summary(text: &mut String, side: &str, pages: &[PageOrderEvidence]) {
    if pages.is_empty() {
        return;
    }
    let stream = pages
        .iter()
        .filter(|page| page.order == OrderKind::Stream)
        .count();
    let baseline = pages
        .iter()
        .filter(|page| page.order == OrderKind::Baseline)
        .count();
    let uncertain = pages.iter().filter(|page| page.uncertain).count();
    writeln!(
        text,
        "  {side} order: {} pages, {stream} stream, {baseline} baseline, {uncertain} uncertain",
        pages.len()
    )
    .expect("writing to a String cannot fail");
    if uncertain > 0 {
        let list = pages
            .iter()
            .filter(|page| page.uncertain)
            .map(|page| (u64::from(page.page.0) + 1).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(text, "  {side} order-uncertain pages: {list}")
            .expect("writing to a String cannot fail");
    }
    let stacked = pages
        .iter()
        .filter(|page| !page.bands.is_empty())
        .map(|page| {
            let bands = page
                .bands
                .iter()
                .map(|band| band.order.name())
                .collect::<Vec<_>>()
                .join("+");
            format!("{} ({bands})", u64::from(page.page.0) + 1)
        })
        .collect::<Vec<_>>();
    if !stacked.is_empty() {
        writeln!(text, "  {side} stacked-band pages: {}", stacked.join(", "))
            .expect("writing to a String cannot fail");
    }
    let low = pages
        .iter()
        .filter(|page| page.coherence < ORDER_COHERENCE_MIN)
        .map(|page| format!("{} ({:.2})", u64::from(page.page.0) + 1, page.coherence))
        .collect::<Vec<_>>();
    if !low.is_empty() {
        writeln!(
            text,
            "  {side} low-coherence pages (<{ORDER_COHERENCE_MIN:.2}): {}",
            low.join(", ")
        )
        .expect("writing to a String cannot fail");
    }
}

fn describe_change_stats(stats: &pdfdelta_flow::ChangeStats) -> String {
    format!(
        "{} changes, {} old tokens, {} new tokens",
        stats.changes, stats.old_tokens, stats.new_tokens
    )
}

fn describe_extraction(side: &Side) -> String {
    if side.is_complete() {
        return String::from("complete");
    }
    let mut unsupported = 0;
    let mut unresolved = 0;
    for issue in &side.issues {
        match issue.kind() {
            pdfdelta_core::source::ExtractionIssueKind::Unsupported => unsupported += 1,
            pdfdelta_core::source::ExtractionIssueKind::Unresolved => unresolved += 1,
        }
    }
    format!(
        "incomplete ({} issues: {unsupported} unsupported, {unresolved} unresolved)",
        side.issues.len()
    )
}

/// Lists content changes in old-document order. Layout-only changes are
/// counted in the summary but intentionally not listed.
fn write_changes(
    text: &mut String,
    diff: &FlowDiff,
    old: &FlowStream,
    new: &FlowStream,
    old_side: &Side,
    new_side: &Side,
    assessments: &[ChangeAssessment],
) {
    let old_incomplete = IncompleteSummary::of(old_side);
    let new_incomplete = IncompleteSummary::of(new_side);
    let old_order = uncertain_order_pages(old_side);
    let new_order = uncertain_order_pages(new_side);
    let old_glyph_names = old_side.glyph_name_identity_pages.clone();
    let new_glyph_names = new_side.glyph_name_identity_pages.clone();
    let mut listed = 0;
    for (index, change) in diff.changes.as_slice().iter().enumerate() {
        let (kind, old_range, new_range) = change_ranges(change);
        if matches!(change, FlowChange::LayoutOnly { .. }) {
            continue;
        }
        listed += 1;
        let old_pages = page_numbers(old, &old_range);
        let new_pages = page_numbers(new, &new_range);
        let touches_incomplete =
            old_incomplete.touches(&old_pages) || new_incomplete.touches(&new_pages);
        let touches_order = old_pages.iter().any(|page| old_order.contains(page))
            || new_pages.iter().any(|page| new_order.contains(page));
        let mut flags = Vec::new();
        if touches_incomplete {
            flags.push("incomplete pages");
        }
        if touches_order {
            flags.push("order-uncertain pages");
        }
        if old_pages.iter().any(|page| old_glyph_names.contains(page))
            || new_pages.iter().any(|page| new_glyph_names.contains(page))
        {
            flags.push("glyph-name identity pages");
        }
        let suffix = if flags.is_empty() {
            String::new()
        } else {
            format!(" ({})", flags.join(", "))
        };
        let marker = assessments
            .get(index)
            .map_or_else(String::new, assessment_marker);
        writeln!(
            text,
            "\n{listed}. {kind} old {} -> new {}{suffix}{marker}",
            describe_pages(old, &old_range),
            describe_pages(new, &new_range)
        )
        .expect("writing to a String cannot fail");
        writeln!(text, "   old: {}", context_line(old, &old_range))
            .expect("writing to a String cannot fail");
        writeln!(text, "   new: {}", context_line(new, &new_range))
            .expect("writing to a String cannot fail");
    }
}

fn assessment_marker(assessment: &ChangeAssessment) -> String {
    if assessment.ambiguity.is_empty() {
        return String::new();
    }
    let reasons = assessment
        .ambiguity
        .iter()
        .map(|reason| reason.label())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        " [confidence: {}; ambiguity: {reasons}]",
        assessment.confidence.label()
    )
}

/// Lists formatting-only changes on matched tokens. They never select exit 1.
fn write_formatting(
    text: &mut String,
    changes: &[FormattingChange],
    old: &FlowStream,
    new: &FlowStream,
) {
    if changes.is_empty() {
        return;
    }
    writeln!(text, "\nformatting-only: {}", changes.len())
        .expect("writing to a String cannot fail");
    for (index, change) in changes.iter().take(FORMATTING_TEXT_LIMIT).enumerate() {
        let reasons = change
            .reasons
            .iter()
            .map(|reason| reason.label())
            .collect::<Vec<_>>()
            .join(", ");
        let sizes = match (change.old_size, change.new_size) {
            (Some(old_size), Some(new_size)) => {
                format!(", font_size old {old_size:.2} -> new {new_size:.2}")
            }
            _ => String::new(),
        };
        writeln!(
            text,
            "  {}. {reasons}{sizes}, pages old {} -> new {}",
            index + 1,
            describe_pages(old, &change.old),
            describe_pages(new, &change.new)
        )
        .expect("writing to a String cannot fail");
        writeln!(
            text,
            "     old: [{}]",
            elided_tokens(&old.tokens[change.old.clone()])
        )
        .expect("writing to a String cannot fail");
        writeln!(
            text,
            "     new: [{}]",
            elided_tokens(&new.tokens[change.new.clone()])
        )
        .expect("writing to a String cannot fail");
    }
    if changes.len() > FORMATTING_TEXT_LIMIT {
        writeln!(
            text,
            "  ... {} more formatting-only entries in the JSON report",
            changes.len() - FORMATTING_TEXT_LIMIT
        )
        .expect("writing to a String cannot fail");
    }
}

/// Lists furniture changes separately: repeated headers, footers, and page
/// numbers are reported but never counted as content changes.
fn write_furniture(text: &mut String, old: &FlowStream, new: &FlowStream) {
    let changes = furniture_changes(old, new);
    if old.furniture.is_empty() && new.furniture.is_empty() {
        return;
    }
    writeln!(text, "\nfurniture:").expect("writing to a String cannot fail");
    if changes.is_empty() {
        writeln!(text, "  unchanged masked lines").expect("writing to a String cannot fail");
        return;
    }
    for change in &changes {
        let kind = match change.kind {
            pdfdelta_flow::FurnitureKind::Added => "added",
            pdfdelta_flow::FurnitureKind::Removed => "removed",
        };
        let pages = if change.old_pages.is_empty() {
            &change.new_pages
        } else {
            &change.old_pages
        };
        writeln!(
            text,
            "  {kind}: \"{}\" on {} pages",
            escape_terminal_controls(&change.text),
            pages.len()
        )
        .expect("writing to a String cannot fail");
    }
}

fn change_ranges(change: &FlowChange) -> (&'static str, Range<usize>, Range<usize>) {
    match change {
        FlowChange::Delete { old } => ("delete", old.clone(), 0..0),
        FlowChange::Insert { new } => ("insert", 0..0, new.clone()),
        FlowChange::Replace { old, new } => ("replace", old.clone(), new.clone()),
        FlowChange::Move { old, new } => ("move", old.clone(), new.clone()),
        FlowChange::LayoutOnly { old, new } => ("layout-only", old.clone(), new.clone()),
    }
}

/// Pages touched by a token range, in first-appearance order.
fn describe_pages(stream: &FlowStream, range: &Range<usize>) -> String {
    let mut pages = Vec::new();
    for token in &stream.tokens[range.clone()] {
        let page = u64::from(token.page.0) + 1;
        if !pages.contains(&page) {
            pages.push(page);
        }
    }
    if pages.is_empty() {
        return String::from("none");
    }
    pages
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Renders `context [changed] context`, escaping terminal controls. The
/// changed part is elided in the middle when it is long.
fn context_line(stream: &FlowStream, range: &Range<usize>) -> String {
    let total = stream.tokens.len();
    let start = range.start.min(total);
    let end = range.end.min(total).max(start);
    let before_start = start.saturating_sub(CONTEXT_TOKENS);
    let after_end = (end + CONTEXT_TOKENS).min(total);

    let mut line = String::new();
    append_tokens(&mut line, &stream.tokens[before_start..start]);
    if !line.is_empty() {
        line.push(' ');
    }
    line.push('[');
    line.push_str(&elided_tokens(&stream.tokens[start..end]));
    line.push(']');
    let mut after = String::new();
    append_tokens(&mut after, &stream.tokens[end..after_end]);
    if !after.is_empty() {
        line.push(' ');
        line.push_str(&after);
    }
    line
}

fn append_tokens(text: &mut String, tokens: &[pdfdelta_flow::FlowToken]) {
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 {
            text.push(' ');
        }
        text.push_str(&escape_terminal_controls(&token.text));
    }
}

fn elided_tokens(tokens: &[pdfdelta_flow::FlowToken]) -> String {
    if tokens.len() > ELISION_HEAD + ELISION_TAIL {
        let mut head = String::new();
        append_tokens(&mut head, &tokens[..ELISION_HEAD]);
        let mut tail = String::new();
        append_tokens(&mut tail, &tokens[tokens.len() - ELISION_TAIL..]);
        format!("{head} … {tail}")
    } else {
        let mut whole = String::new();
        append_tokens(&mut whole, tokens);
        whole
    }
}

fn json_report(
    comparison: &FlowComparison,
    include_geometry: bool,
    exit: crate::flow_exit::FlowExit,
) -> Value {
    let diff = &comparison.diff;
    let stats = &comparison.stats;
    let old_side = &comparison.old_side;
    let new_side = &comparison.new_side;
    let assessments = &comparison.assessments;
    let formatting = &comparison.formatting;
    let old_incomplete = IncompleteSummary::of(old_side);
    let new_incomplete = IncompleteSummary::of(new_side);
    let evidence = ChangeEvidence {
        old_incomplete: &old_incomplete,
        new_incomplete: &new_incomplete,
        old_order: uncertain_order_pages(old_side),
        new_order: uncertain_order_pages(new_side),
        old_glyph_names: old_side.glyph_name_identity_pages.clone(),
        new_glyph_names: new_side.glyph_name_identity_pages.clone(),
    };
    let mut report = json!({
        "schema": "pdfdelta-flow/1",
        "exit_basis": exit.basis,
        "difference_status": difference_status(comparison, selects_difference(stats)),
        "old": side_json(old_side),
        "new": side_json(new_side),
        "order": {
            "agreement_min": pdfdelta_flow::ORDER_AGREEMENT_MIN,
            "coherence_min": ORDER_COHERENCE_MIN,
            "old": order_pages_json(&old_side.order),
            "new": order_pages_json(&new_side.order),
            "old_low_coherence_pages": low_coherence_pages(&old_side.order),
            "new_low_coherence_pages": low_coherence_pages(&new_side.order),
        },
        "stats": stats_json(stats, formatting.len()),
        "changes": diff.changes.as_slice().iter().enumerate().map(|(index, change)| change_json(change, old_side, new_side, diff.spine.as_slice(), &evidence, assessments.get(index))).collect::<Vec<_>>(),
        "formatting_only": formatting.iter().map(|change| formatting_json(change, old_side, new_side)).collect::<Vec<_>>(),
        "furniture": {
            "old": old_side.stream.furniture.iter().map(furniture_json).collect::<Vec<_>>(),
            "new": new_side.stream.furniture.iter().map(furniture_json).collect::<Vec<_>>(),
            "changes": furniture_changes(&old_side.stream, &new_side.stream)
                .iter()
                .map(furniture_change_json)
                .collect::<Vec<_>>(),
        },
    });
    if include_geometry {
        report["old_geometry"] = geometry_json(&old_side.stream);
        report["new_geometry"] = geometry_json(&new_side.stream);
    }
    report
}

fn furniture_json(line: &pdfdelta_flow::FurnitureLine) -> Value {
    json!({
        "text": line.text,
        "pages": line.pages.iter().map(|page| u64::from(page.0) + 1).collect::<Vec<_>>(),
        "occurrences": line.occurrences,
    })
}

fn furniture_change_json(change: &pdfdelta_flow::FurnitureChange) -> Value {
    let kind = match change.kind {
        pdfdelta_flow::FurnitureKind::Added => "added",
        pdfdelta_flow::FurnitureKind::Removed => "removed",
    };
    json!({
        "kind": kind,
        "text": change.text,
        "old_pages": change.old_pages.iter().map(|page| u64::from(page.0) + 1).collect::<Vec<_>>(),
        "new_pages": change.new_pages.iter().map(|page| u64::from(page.0) + 1).collect::<Vec<_>>(),
    })
}

fn geometry_json(stream: &FlowStream) -> Value {
    stream
        .tokens
        .iter()
        .map(|token| {
            let bounds = token.bounds.map_or(Value::Null, |bounds| {
                json!([bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y])
            });
            json!({
                "text": token.text,
                "page": u64::from(token.page.0) + 1,
                "bounds": bounds,
                "block": token.block,
                "line_break_after": token.line_break_after,
                "uncertain": token.uncertain,
            })
        })
        .collect()
}

fn order_pages_json(pages: &[PageOrderEvidence]) -> Vec<Value> {
    pages
        .iter()
        .map(|page| {
            json!({
                "page": u64::from(page.page.0) + 1,
                "order": page.order.name(),
                "order_uncertain": page.uncertain,
                "agreement": page.agreement,
                "layout_score": page.layout_score,
                "stream_score": page.stream_score,
                "baseline_score": page.baseline_score,
                "order_coherence": page.coherence,
                "bands": page
                    .bands
                    .iter()
                    .map(|band| {
                        json!({
                            "bottom": band.bottom,
                            "top": band.top,
                            "order": band.order.name(),
                        })
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn low_coherence_pages(pages: &[PageOrderEvidence]) -> Vec<Value> {
    pages
        .iter()
        .filter(|page| page.coherence < ORDER_COHERENCE_MIN)
        .map(|page| {
            json!({
                "page": u64::from(page.page.0) + 1,
                "order_coherence": page.coherence,
                "bands": page
                    .bands
                    .iter()
                    .map(|band| {
                        json!({
                            "bottom": band.bottom,
                            "top": band.top,
                            "order": band.order.name(),
                        })
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn side_json(side: &Side) -> Value {
    let incomplete = IncompleteSummary::of(side);
    json!({
        "tokens": side.stream.tokens.len(),
        "extraction_complete": side.is_complete(),
        "incomplete_pages": incomplete
            .pages
            .iter()
            .map(|(page, kinds)| {
                json!({
                    "page": page,
                    "kinds": kinds.iter().copied().collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>(),
        "incomplete_document_kinds": incomplete.unscoped.iter().copied().collect::<Vec<_>>(),
        "incomplete_conservation": incomplete.conservation_incomplete,
        "image_identity_pages": side.image_identity_pages.iter().copied().collect::<Vec<_>>(),
        "image_identity_rules": side
            .image_identity_rules
            .iter()
            .map(|(page, rule)| json!({"page": page, "rule": rule}))
            .collect::<Vec<_>>(),
        "glyph_name_identity_pages": side
            .glyph_name_identity_pages
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        "conservation": conservation_json(side),
        "issues": side
            .issues
            .iter()
            .map(|issue| {
                json!({
                    "kind": issue.kind(),
                    "scope": issue.scope(),
                    "description": issue.description(),
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn conservation_json(side: &Side) -> Value {
    let conservation = &side.stream.conservation;
    json!({
        "used": conservation.used,
        "visible": conservation.visible,
        "complete": conservation.is_complete(),
        "pages": conservation
            .pages
            .iter()
            .map(|page| {
                json!({
                    "page": u64::from(page.page.0) + 1,
                    "used": page.used,
                    "visible": page.visible,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn stats_json(stats: &FlowStats, formatting_only: usize) -> Value {
    let content_changes = stats.delete.changes + stats.insert.changes + stats.replace.changes;
    json!({
        "content_changes": content_changes,
        "moved_changes": stats.moves.changes,
        "detected_changes": content_changes + stats.moves.changes,
        "formatting_only": formatting_only,
        "old_tokens": stats.old_tokens,
        "new_tokens": stats.new_tokens,
        "equal_tokens": stats.equal_tokens,
        "delete": change_stats_json(&stats.delete),
        "insert": change_stats_json(&stats.insert),
        "replace": change_stats_json(&stats.replace),
        "move": change_stats_json(&stats.moves),
        "layout_only": change_stats_json(&stats.layout_only),
    })
}

fn change_stats_json(stats: &pdfdelta_flow::ChangeStats) -> Value {
    json!({
        "changes": stats.changes,
        "old_tokens": stats.old_tokens,
        "new_tokens": stats.new_tokens,
    })
}

struct ChangeEvidence<'a> {
    old_incomplete: &'a IncompleteSummary,
    new_incomplete: &'a IncompleteSummary,
    old_order: BTreeSet<u64>,
    new_order: BTreeSet<u64>,
    old_glyph_names: BTreeSet<u64>,
    new_glyph_names: BTreeSet<u64>,
}

fn change_json(
    change: &FlowChange,
    old: &Side,
    new: &Side,
    spine: &[(usize, usize)],
    evidence: &ChangeEvidence<'_>,
    assessment: Option<&ChangeAssessment>,
) -> Value {
    let (kind, old_range, new_range) = change_ranges(change);
    let old_pages = page_numbers(&old.stream, &old_range);
    let new_pages = page_numbers(&new.stream, &new_range);
    let touches_incomplete_page =
        evidence.old_incomplete.touches(&old_pages) || evidence.new_incomplete.touches(&new_pages);
    let touches_order_uncertain_page = old_pages
        .iter()
        .any(|page| evidence.old_order.contains(page))
        || new_pages
            .iter()
            .any(|page| evidence.new_order.contains(page));
    let mut value = json!({
        "kind": kind,
        "old": [old_range.start, old_range.end],
        "new": [new_range.start, new_range.end],
        "old_pages": old_pages,
        "new_pages": new_pages,
        "old_text": join_tokens(&old.stream.tokens[old_range.clone()]),
        "new_text": join_tokens(&new.stream.tokens[new_range.clone()]),
        "touches_incomplete_page": touches_incomplete_page,
        "touches_order_uncertain_page": touches_order_uncertain_page,
        "touches_glyph_name_identity_page": old_pages
            .iter()
            .any(|page| evidence.old_glyph_names.contains(page))
            || new_pages
                .iter()
                .any(|page| evidence.new_glyph_names.contains(page)),
    });
    if let Some(assessment) = assessment {
        value["confidence"] = json!(assessment.confidence.label());
        value["ambiguity"] = json!(
            assessment
                .ambiguity
                .iter()
                .map(|reason| reason.label())
                .collect::<Vec<_>>()
        );
    }
    if matches!(change, FlowChange::Move { .. }) {
        let old_gap = spine.partition_point(|(old_index, _)| *old_index < old_range.start);
        let new_gap = spine.partition_point(|(_, new_index)| *new_index < new_range.start);
        value["old_gap"] = json!(old_gap);
        value["new_gap"] = json!(new_gap);
    }
    value
}

fn formatting_json(change: &FormattingChange, old: &Side, new: &Side) -> Value {
    let reasons = change
        .reasons
        .iter()
        .map(|reason| reason.label())
        .collect::<Vec<_>>();
    let mut value = json!({
        // `reason` stays the first reason for older readers; `reasons` is the
        // authoritative list once several kinds can coexist.
        "reason": reasons.first().copied().unwrap_or("font_size"),
        "reasons": reasons,
        "old": [change.old.start, change.old.end],
        "new": [change.new.start, change.new.end],
        "old_pages": page_numbers(&old.stream, &change.old),
        "new_pages": page_numbers(&new.stream, &change.new),
        "old_text": join_tokens(&old.stream.tokens[change.old.clone()]),
        "new_text": join_tokens(&new.stream.tokens[change.new.clone()]),
    });
    if let (Some(old_size), Some(new_size)) = (change.old_size, change.new_size) {
        value["old_size"] = json!(old_size);
        value["new_size"] = json!(new_size);
    }
    value
}

fn page_numbers(stream: &FlowStream, range: &Range<usize>) -> Vec<u64> {
    let mut pages = Vec::new();
    for token in &stream.tokens[range.clone()] {
        let page = u64::from(token.page.0) + 1;
        if !pages.contains(&page) {
            pages.push(page);
        }
    }
    pages
}

fn token_texts(stream: &FlowStream) -> Vec<&str> {
    stream
        .tokens
        .iter()
        .map(|token| token.text.as_str())
        .collect()
}

fn join_tokens(tokens: &[pdfdelta_flow::FlowToken]) -> String {
    tokens
        .iter()
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfdelta_core::model::PageId;

    fn issue(kind: ExtractionIssueKind, scope: ExtractionScope) -> ExtractionIssue {
        ExtractionIssue::new(kind, scope, "test issue").expect("issue is valid")
    }

    fn side(issues: Vec<ExtractionIssue>) -> Side {
        Side {
            stream: FlowStream::default(),
            issues,
            paint_issues: 0,
            order: Vec::new(),
            image_identity_pages: BTreeSet::new(),
            image_identity_rules: BTreeMap::new(),
            glyph_name_identity_pages: BTreeSet::new(),
        }
    }

    #[test]
    fn page_scoped_issues_only_mark_that_page() {
        let summary = IncompleteSummary::of(&side(vec![issue(
            ExtractionIssueKind::Unsupported,
            ExtractionScope::Page(PageId(2)),
        )]));
        assert!(summary.pages.contains_key(&3));
        assert!(summary.touches(&[3]));
        assert!(!summary.touches(&[1, 2]));
    }

    #[test]
    fn issues_without_a_page_mark_every_change() {
        let summary = IncompleteSummary::of(&side(vec![issue(
            ExtractionIssueKind::Unresolved,
            ExtractionScope::GlyphGap {
                retained_before: 10,
            },
        )]));
        assert!(summary.pages.is_empty());
        assert!(summary.unscoped.contains("unresolved"));
        assert!(summary.touches(&[1]));
        assert!(summary.touches(&[]));
    }

    #[test]
    fn incomplete_conservation_marks_every_change() {
        let mut side = side(Vec::new());
        side.stream.conservation.visible = 2;
        side.stream.conservation.used = 1;
        let summary = IncompleteSummary::of(&side);
        assert!(summary.conservation_incomplete);
        assert!(summary.touches(&[9]));
    }

    #[test]
    fn complete_sides_have_no_incomplete_pages() {
        let summary = IncompleteSummary::of(&side(Vec::new()));
        assert!(summary.is_empty());
        assert!(!summary.touches(&[1]));
    }
}
