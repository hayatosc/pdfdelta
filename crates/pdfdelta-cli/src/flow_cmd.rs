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
    FlowCandidates, FlowChange, FlowDiff, FlowStats, FlowStream, FlowToken, ORDER_COHERENCE_MIN,
    OrderKind, PageOrderEvidence, flow_candidates_owned, flow_diff, flow_order_from_candidates,
    furniture_changes,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    compare::ExitStatus,
    evidence_text::escape_terminal_controls,
    fs::{parse_lopdf, read_limited, write_output_atomically},
};

/// Writes token texts as one JSON string per line next to the report.
///
/// The sidecar name is the report path without a trailing `.gz`, followed by
/// `.<side>-tokens.jsonl` and `.gz` again when the report itself is compressed.
/// Tokens stay out of the report object so a large corpus run can stream them
/// without holding every token in memory.
fn write_token_sidecars(path: &Path, old: &FlowStream, new: &FlowStream) -> Result<(), String> {
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
        })?;
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

/// One extracted input, with every extraction issue retained as evidence.
struct Side {
    stream: FlowStream,
    issues: Vec<ExtractionIssue>,
    order: Vec<PageOrderEvidence>,
    image_identity_pages: BTreeSet<u64>,
    image_identity_rules: BTreeMap<u64, &'static str>,
    glyph_name_identity_pages: BTreeSet<u64>,
}

impl Side {
    fn is_complete(&self) -> bool {
        self.issues.is_empty() && self.stream.conservation.is_complete()
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

fn issue_kind_label(kind: ExtractionIssueKind) -> &'static str {
    match kind {
        ExtractionIssueKind::Unsupported => "unsupported",
        ExtractionIssueKind::Unresolved => "unresolved",
    }
}

/// Distinguishes temporary candidate directories within one process.
static CACHE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A private per-run directory for cached candidate streams.
///
/// The directory is created with owner-only permissions and removed when it
/// goes out of scope, including every early error return.
struct CacheDirectory(PathBuf);

impl CacheDirectory {
    fn create() -> Result<Self, String> {
        let sequence = CACHE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("pdfdelta-flow-{}-{sequence}", std::process::id()));
        std::fs::create_dir(&path)
            .map_err(|error| format!("cannot create the flow cache directory: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("cannot protect the flow cache directory: {error}"))?;
        }
        Ok(Self(path))
    }

    fn file(&self, side: &str) -> PathBuf {
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
            issues: Vec::new(),
            image_issue_pages: BTreeSet::new(),
            encoded_image_pages: BTreeMap::new(),
            image_input: None,
            glyph_name_identity_pages: BTreeSet::new(),
            clip_uncertain_pages: BTreeSet::new(),
        }
    }
}

/// Builds one side's reading-order hypotheses in this process.
fn prepare_side(
    path: &Path,
    limits: ParseLimits,
    extraction_limits: ExtractionLimits,
) -> Result<(FlowCandidates, SideMeta), String> {
    let extracted = extract_side(path, limits, extraction_limits)?;
    let ExtractedSide {
        document,
        issues,
        image_issue_pages,
        encoded_image_pages,
        image_input,
        glyph_name_identity_pages,
        clip_uncertain_pages,
    } = extracted;
    let candidates = flow_candidates_owned(document)
        .map_err(|error| format!("failed to reconstruct reading order: {error}"))?;
    Ok((
        candidates,
        SideMeta {
            issues,
            image_issue_pages,
            encoded_image_pages,
            image_input,
            glyph_name_identity_pages,
            clip_uncertain_pages,
        },
    ))
}

/// Builds one side in a child process and returns its cache file.
///
/// Returns `Ok(None)` when the child reports that the side carries evidence
/// the compact cache does not represent, so the caller rebuilds both sides in
/// process.
fn build_cached_side(path: &Path, limit_scale: f64, cache: &Path) -> Result<bool, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate the pdfdelta executable: {error}"))?;
    let scale = limit_scale.to_string();
    let status = Command::new(executable)
        .arg("flow")
        .arg(path)
        .arg(path)
        .arg("--limit-scale")
        .arg(&scale)
        .env("PDFDELTA_FLOW_BUILD_OUTPUT", cache)
        .env("PDFDELTA_FLOW_BUILD_SCALE", &scale)
        .status()
        .map_err(|error| format!("cannot start the flow candidate builder: {error}"))?;
    if status.code() == Some(4) {
        return Ok(false);
    }
    if !status.success() {
        return Err(format!(
            "flow candidate builder exited with status {status}; no comparison was produced"
        ));
    }
    Ok(true)
}

/// Loads a cache file produced by [`build_cached_side`].
fn load_cached_side(path: &Path) -> Result<FlowCandidates, String> {
    let file =
        File::open(path).map_err(|error| format!("cannot open cached flow candidates: {error}"))?;
    FlowCandidates::read_cache(BufReader::new(file))
        .map_err(|error| format!("cannot read cached flow candidates: {error}"))
}

/// Writes one side's candidates for the parent flow process.
///
/// Returns exit code `4` when the extracted side has image, issue, or
/// glyph-name evidence that the compact cache does not represent.
fn build_candidate_cache(path: &Path, output: &Path, limit_scale: f64) -> Result<u8, String> {
    #[cfg(debug_assertions)]
    if std::env::var_os("PDFDELTA_FLOW_BUILD_FAIL").is_some() {
        return Err("candidate cache build failed for this test".to_owned());
    }
    let limits = ParseLimits::default();
    let extraction_limits = ExtractionLimits::default()
        .scaled(limit_scale)
        .map_err(|error| format!("invalid flow limit scale: {error}"))?;
    let extracted = extract_side(path, limits, extraction_limits)?;
    if !extracted.issues.is_empty()
        || extracted.image_input.is_some()
        || !extracted.encoded_image_pages.is_empty()
        || !extracted.glyph_name_identity_pages.is_empty()
    {
        return Ok(4);
    }
    let candidates = flow_candidates_owned(extracted.document)
        .map_err(|error| format!("failed to reconstruct reading order: {error}"))?;
    let file = File::create(output)
        .map_err(|error| format!("cannot create cached flow candidates: {error}"))?;
    let mut writer = BufWriter::new(file);
    candidates
        .write_cache(&mut writer)
        .map_err(|error| format!("cannot write cached flow candidates: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("cannot flush cached flow candidates: {error}"))?;
    Ok(0)
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
pub fn run(
    old_path: &Path,
    new_path: &Path,
    json_path: Option<&Path>,
    include_tokens: bool,
    include_geometry: bool,
    limit_scale: f64,
    writer: &mut impl Write,
) -> Result<u8, String> {
    if let Some(output) = std::env::var_os("PDFDELTA_FLOW_BUILD_OUTPUT") {
        let scale = std::env::var("PDFDELTA_FLOW_BUILD_SCALE")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(limit_scale);
        return build_candidate_cache(old_path, Path::new(&output), scale);
    }

    let limits = ParseLimits::default();
    let extraction_limits = ExtractionLimits::default()
        .scaled(limit_scale)
        .map_err(|error| format!("invalid flow limit scale: {error}"))?;
    // Raised extraction limits mean a document large enough that the second
    // side's extraction must not overlap the first side's normalization
    // buffers. Build both sides in child processes, then load the compact
    // candidate streams; the parent holds no glyph document or candidate data
    // while a child is extracting.
    let use_cache = limit_scale > 1.0;
    let cached = if use_cache {
        let directory = CacheDirectory::create()?;
        let old_cache = directory.file("old");
        let new_cache = directory.file("new");
        let old_built = build_cached_side(old_path, limit_scale, &old_cache)?;
        let new_built = build_cached_side(new_path, limit_scale, &new_cache)?;
        if old_built && new_built {
            let old_candidates = load_cached_side(&old_cache)?;
            let new_candidates = load_cached_side(&new_cache)?;
            Some((old_candidates, new_candidates))
        } else {
            // A side with image, issue, or glyph-name evidence is not
            // represented by the compact cache; rebuild both sides in process.
            None
        }
    } else {
        None
    };
    let (old_candidates, old_meta, new_candidates, new_meta) = if let Some((old, new)) = cached {
        (old, SideMeta::empty(), new, SideMeta::empty())
    } else {
        let (old_candidates, old_meta) = prepare_side(old_path, limits, extraction_limits)?;
        let (new_candidates, new_meta) = prepare_side(new_path, limits, extraction_limits)?;
        (old_candidates, old_meta, new_candidates, new_meta)
    };
    let SideMeta {
        issues: old_issues,
        image_issue_pages: old_image_issue_pages,
        encoded_image_pages: old_encoded_image_pages,
        image_input: old_image_input,
        glyph_name_identity_pages: old_glyph_name_pages,
        clip_uncertain_pages: old_clip_pages,
    } = old_meta;
    let SideMeta {
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
    let pair = flow_order_from_candidates(old_candidates, new_candidates)
        .map_err(|error| format!("failed to choose reading order: {error}"))?;

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
    let old_side = Side {
        stream: old_stream,
        issues: retained_image_issues(old_issues, &images.old_resolved),
        order: pair.old_pages,
        image_identity_pages: old_image_pages,
        image_identity_rules: images.old_rules,
        glyph_name_identity_pages: old_glyph_name_pages,
    };
    let new_side = Side {
        stream: new_stream,
        issues: retained_image_issues(new_issues, &images.new_resolved),
        order: pair.new_pages,
        image_identity_pages: new_image_pages,
        image_identity_rules: images.new_rules,
        glyph_name_identity_pages: new_glyph_name_pages,
    };

    let diff = flow_diff(&old_side.stream.tokens, &new_side.stream.tokens)
        .map_err(|error| format!("flow diff failed verification: {error}"))?;
    let stats = diff.stats();

    let mut text = String::new();
    write_summary(&mut text, &diff, &stats, &old_side, &new_side);
    write_changes(
        &mut text,
        &diff,
        &old_side.stream,
        &new_side.stream,
        &old_side,
        &new_side,
    );
    write_furniture(&mut text, &old_side.stream, &new_side.stream);
    writer
        .write_all(text.as_bytes())
        .map_err(|error| format!("failed to write flow report: {error}"))?;

    if let Some(path) = json_path {
        let report = json_report(&diff, &stats, &old_side, &new_side, include_geometry);
        write_output_atomically(path, "flow report", |output| {
            serde_json::to_writer(output, &report)
                .map_err(|error| format!("failed to serialize flow report: {error}"))
        })?;
        if include_tokens {
            write_token_sidecars(path, &old_side.stream, &new_side.stream)?;
        }
    }

    if !old_side.is_complete() || !new_side.is_complete() {
        return Ok(ExitStatus::IncompleteComparison.code());
    }
    let content_changes = stats.delete.changes + stats.insert.changes + stats.replace.changes;
    if content_changes > 0 {
        Ok(ExitStatus::ContentChanges.code())
    } else {
        Ok(ExitStatus::NoContentChanges.code())
    }
}

/// Reads, parses, and extracts one PDF with the same limits as the comparison
/// pipeline.
fn extract_side(
    path: &Path,
    limits: ParseLimits,
    extraction_limits: ExtractionLimits,
) -> Result<ExtractedSide, String> {
    let bytes = read_limited(path, limits.max_input_bytes)?;
    let image_bytes = bytes.clone();
    let pdf = parse_lopdf(bytes, limits, None)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    let outcome = ContentStreamGlyphExtractor
        .extract_outcome_with_external_font_identities(
            pdf.as_ref(),
            extraction_limits,
            &ExternalFontIdentities::default(),
        )
        .map_err(|error| format!("failed to extract {}: {error}", path.display()))?;
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
        let pages = pdf
            .pages()
            .map_err(|error| format!("failed to enumerate {}: {error}", path.display()))?;
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
        stream.tokens.insert(
            position,
            FlowToken {
                text: text.clone(),
                page: *page,
                bounds: None,
                block: 0,
                uncertain: false,
                line_break_after: false,
            },
        );
    }
}

/// One extracted input before reading-order selection.
struct ExtractedSide {
    document: Document<Glyph>,
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
    writeln!(
        text,
        "  content changes: {}",
        stats.delete.changes + stats.insert.changes + stats.replace.changes
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
) {
    let old_incomplete = IncompleteSummary::of(old_side);
    let new_incomplete = IncompleteSummary::of(new_side);
    let old_order = uncertain_order_pages(old_side);
    let new_order = uncertain_order_pages(new_side);
    let old_glyph_names = old_side.glyph_name_identity_pages.clone();
    let new_glyph_names = new_side.glyph_name_identity_pages.clone();
    let mut listed = 0;
    for change in diff.changes.as_slice() {
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
        writeln!(
            text,
            "\n{listed}. {kind} old {} -> new {}{suffix}",
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
    diff: &FlowDiff,
    stats: &FlowStats,
    old_side: &Side,
    new_side: &Side,
    include_geometry: bool,
) -> Value {
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
        "stats": stats_json(stats),
        "changes": diff.changes.as_slice().iter().map(|change| change_json(change, old_side, new_side, diff.spine.as_slice(), &evidence)).collect::<Vec<_>>(),
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

fn stats_json(stats: &FlowStats) -> Value {
    json!({
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
    if matches!(change, FlowChange::Move { .. }) {
        let old_gap = spine.partition_point(|(old_index, _)| *old_index < old_range.start);
        let new_gap = spine.partition_point(|(_, new_index)| *new_index < new_range.start);
        value["old_gap"] = json!(old_gap);
        value["new_gap"] = json!(new_gap);
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
