//! Real-world revision-pair benchmark track.
//!
//! Unlike the synthetic acceptance matrix, this track compares two genuinely
//! different public revisions of the same document. Documents are never
//! vendored: the manifest records stable URLs, capture dates, byte sizes, and
//! SHA-256 checksums, and operators download the files into a local cache
//! directory with `benchmark/realworld/fetch.sh`.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use pdfdelta_core::{
    alignment::BlockSeparator,
    diff::{ChangeKind, Comparison, TextSpan},
    model::Document,
    normalize::{BlockText, ComparableToken},
    pdf::{LopdfParser, ParseLimits},
    pipeline::{
        PipelineDiagnostics, PipelineOptions, PipelinePhase,
        compare_extraction_outcomes_with_diagnostics,
    },
    report::{self, DocumentSide, summarize},
    source::{
        ContentStreamGlyphExtractor, ExtractionIssue, ExtractionIssueKind, ExtractionLimits,
        ExtractionOutcome, ExtractionScope, ParserBackedGlyphSource,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    BenchError, Result,
    candidate_eval::{CandidateVisitPressure, evaluate_candidate_visit_pressure},
};

pub const QUALITY_SKIP_RESOURCE_LIMIT: &str = "comparison stopped at a resource limit";
pub const QUALITY_SKIP_INCOMPLETE_EXTRACTION: &str =
    "extraction was incomplete so reported diffs are suppressed";
pub const QUALITY_SKIP_NO_ANNOTATIONS: &str = "no expected annotations are recorded for this pair";

/// Column order of `benchmark/realworld/manifest.tsv`.
pub const MANIFEST_HEADER: [&str; 17] = [
    "pair_id",
    "set",
    "role",
    "document_type",
    "layout",
    "in_scope",
    "known_issues",
    "captured",
    "expected_extraction",
    "limit_scale_hint",
    "expected_file",
    "old_url",
    "old_byte_count",
    "old_sha256",
    "new_url",
    "new_byte_count",
    "new_sha256",
];

const SHA256_HEX_LEN: usize = 64;

/// A reported change flattened to the texts a human reviewer compares against
/// expected quotes.
#[derive(Debug)]
pub struct ActualChange {
    pub kind: ChangeKind,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub old_comparable_len: Option<usize>,
    pub new_comparable_len: Option<usize>,
    pub resolvable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct QualityMetrics {
    pub annotation: Annotation,
    pub expected_changes: usize,
    pub reported_changes: usize,
    pub recall: Option<f64>,
    pub precision: Option<f64>,
    pub kind_accuracy: Option<f64>,
    /// Reported hunks divided by quote-matched expected changes. Under
    /// `Partial` annotations the denominator is only the reviewed subset,
    /// so this overstates fragmentation and is not comparable with the
    /// complete-annotation `review_hunks_per_expected_change`.
    pub reported_hunks_per_matched_change: Option<f64>,
    /// Reported hunks per expected semantic change; computed only for
    /// `Complete` annotations where every semantic change was reviewed.
    pub review_hunks_per_expected_change: Option<f64>,
    pub unmatched_tiny_changes: usize,
    pub unresolvable_reported_spans: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IssueLine {
    pub side: &'static str,
    pub kind: &'static str,
    pub scope: String,
    pub description: String,
}

/// Truncated span texts of one reported change, giving human reviewers the
/// evidence needed to maintain expected-annotation files without a separate
/// inspection pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReportedChangeText {
    pub kind: &'static str,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
}

/// Outcome of one pair run. `Limit` records a comparison that stopped at a
/// documented resource budget before producing a diff; it is distinct from
/// `Ok` and `Failed` so incomplete measurements never masquerade as healthy
/// results.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PairRunStatus {
    Ok,
    Limit,
    Failed,
}

impl PairRunStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Limit => "LIMIT",
            Self::Failed => "FAIL",
        }
    }
}

const CHANGE_TEXT_PREVIEW_CHARS: usize = 240;

fn truncate_preview(text: &str) -> String {
    text.chars().take(CHANGE_TEXT_PREVIEW_CHARS).collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct PairRunReport {
    pub pair_id: String,
    pub set: &'static str,
    pub role: &'static str,
    pub document_type: String,
    pub in_scope: bool,
    pub status: PairRunStatus,
    pub provenance_verified: bool,
    pub compared: bool,
    /// Both sides produced glyph evidence without document- or page-scoped
    /// extraction issues; independent of alignment quality.
    pub extraction_complete: Option<bool>,
    /// No unresolved regions and full comparable-token coverage on both
    /// sides; false does not imply an extraction failure.
    pub comparison_complete: Option<bool>,
    pub extraction_issues: Vec<IssueLine>,
    pub coverage_old: Option<f64>,
    pub coverage_new: Option<f64>,
    pub coverage_comparison: Option<f64>,
    pub unresolved_regions: Option<usize>,
    pub unresolved_old_token_share: Option<f64>,
    pub unresolved_new_token_share: Option<f64>,
    pub reported_content_changes: Option<usize>,
    pub formatting_only_changes: Option<usize>,
    /// Count of low-confidence changes identified during comparison.
    /// Skipped in serde serialization (`#[serde(skip)]`) to preserve the
    /// byte-for-byte schema and key set of full `--json-output` v1 reports,
    /// while allowing compact summary JSON to expose this computed evidence
    /// via `reported_uncertain_changes`.
    #[serde(skip)]
    pub uncertain_changes: Option<usize>,
    pub reported_changes_preview: Vec<ReportedChangeText>,
    pub quality: Option<QualityMetrics>,
    pub quality_skipped_reason: Option<String>,
    pub resource_limit_failure: Option<String>,
    /// Sum of `CandidateGenerator::estimated_visits` charged against
    /// `max_candidate_visits` for non-anchor old blocks; on a candidate
    /// limit stop this is the attempted cumulative charge including the
    /// exceeding block. `None` when alignment was never reached (e.g. an
    /// earlier n-gram or diff budget limit).
    pub candidate_visits: Option<usize>,
    /// Checked sum of `CandidateGenerator::estimated_visits` over every
    /// non-anchor old block, independent of the budget: the full candidate
    /// work the alignment would need. `Some` when the full sum completed
    /// (including on a candidate limit stop); `None` when an estimate error
    /// or overflow made the sum unavailable, or the candidate preflight was
    /// never reached (e.g. an earlier n-gram or diff budget limit).
    pub candidate_visits_required: Option<usize>,
    /// Exact-match posting visits of the required candidate sum; `Some`
    /// only when every non-anchor old block reported a breakdown and every
    /// component sum completed.
    pub candidate_visits_required_exact: Option<usize>,
    /// N-gram posting visits of the required candidate sum; `Some` under
    /// the same conditions as `candidate_visits_required_exact`.
    pub candidate_visits_required_ngram: Option<usize>,
    /// Short-block fallback visits of the required candidate sum; `Some`
    /// under the same conditions as `candidate_visits_required_exact`.
    pub candidate_visits_required_short_fallback: Option<usize>,
    /// The `AlignmentOptions::max_candidate_visits` budget the charge was
    /// compared against.
    pub max_candidate_visits: Option<usize>,
    /// All-old-block candidate visit pressure measured from the extracted
    /// documents; `None` when either side's extraction is incomplete or the
    /// comparison stopped at a pre-alignment resource limit/error.
    pub candidate_visit_pressure: Option<CandidateVisitPressure>,
    pub runtime_ms: u128,
    pub limit_scale_used: f64,
    pub failure: Option<String>,
}

impl PairRunReport {
    /// Only `Ok` records are healthy: a resource-limit stop is not a failure,
    /// but it also did not measure anything and must not be counted as done.
    pub fn healthy(&self) -> bool {
        self.status == PairRunStatus::Ok
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairSet {
    Dev,
    Holdout,
}

impl PairSet {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Holdout => "holdout",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "dev" => Some(Self::Dev),
            "holdout" => Some(Self::Holdout),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairRole {
    Standard,
    Stress,
}

impl PairRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Stress => "stress",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "standard" => Some(Self::Standard),
            "stress" => Some(Self::Stress),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedExtraction {
    Complete,
    Incomplete,
}

impl ExpectedExtraction {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "complete" => Some(Self::Complete),
            "incomplete" => Some(Self::Incomplete),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideProvenance {
    pub url: String,
    pub byte_count: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RevisionPair {
    pub pair_id: String,
    pub set: PairSet,
    pub role: PairRole,
    pub document_type: String,
    pub layout: String,
    pub in_scope: bool,
    pub known_issues: Option<String>,
    pub captured: String,
    pub expected_extraction: ExpectedExtraction,
    pub limit_scale_hint: f64,
    pub expected_file: Option<String>,
    pub old: SideProvenance,
    pub new: SideProvenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Annotation {
    /// Every semantic change visible in review was annotated, so precision is
    /// meaningful.
    Complete,
    /// Only representative changes were annotated; recall and kind accuracy
    /// remain meaningful while precision does not.
    Partial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpectedKind {
    Replacement,
    Insertion,
    Deletion,
    Move,
}

impl ExpectedKind {
    fn agrees_with(self, kind: ChangeKind) -> bool {
        matches!(
            (self, kind),
            (Self::Replacement, ChangeKind::Replacement)
                | (Self::Insertion, ChangeKind::Insertion)
                | (Self::Deletion, ChangeKind::Deletion)
                | (Self::Move, ChangeKind::Move)
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Replacement => "replacement",
            Self::Insertion => "insertion",
            Self::Deletion => "deletion",
            Self::Move => "move",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedChange {
    pub id: String,
    pub kind: ExpectedKind,
    #[serde(default)]
    pub old_quote: Option<String>,
    #[serde(default)]
    pub new_quote: Option<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedDocument {
    pub version: u32,
    pub pair: String,
    pub reviewed_on: String,
    pub annotation: Annotation,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub changes: Vec<ExpectedChange>,
}

struct MatchOutcome {
    matched: usize,
    kind_agreements: usize,
    claimed_actuals: HashSet<usize>,
}

struct ActualTexts {
    old: Option<String>,
    new: Option<String>,
}

pub fn parse_manifest(manifest: &str) -> Result<Vec<RevisionPair>> {
    let mut pairs = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut header_seen = false;
    for (index, raw_line) in manifest.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<&str> = line.split('\t').collect();
        if !header_seen {
            validate_header(&columns, line_number)?;
            header_seen = true;
            continue;
        }
        if columns.len() != MANIFEST_HEADER.len() {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest row {line_number} has {} columns, expected {}",
                columns.len(),
                MANIFEST_HEADER.len()
            )));
        }
        let pair_id = require_nonblank(columns[0], "pair_id", line_number)?;
        if !seen_ids.insert(pair_id.clone()) {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest row {line_number} duplicates pair_id {pair_id}"
            )));
        }
        let set = PairSet::parse(columns[1])
            .ok_or_else(|| invalid_row(line_number, "set", columns[1]))?;
        let role = PairRole::parse(columns[2])
            .ok_or_else(|| invalid_row(line_number, "role", columns[2]))?;
        let in_scope = parse_bool(columns[5], "in_scope", line_number)?;
        let expected_extraction = ExpectedExtraction::parse(columns[8])
            .ok_or_else(|| invalid_row(line_number, "expected_extraction", columns[8]))?;
        let limit_scale_hint = parse_limit_scale_hint(columns[9], line_number)?;
        let old = parse_side_provenance(columns[11], columns[12], columns[13], "old", line_number)?;
        let new = parse_side_provenance(columns[14], columns[15], columns[16], "new", line_number)?;
        pairs.push(RevisionPair {
            pair_id,
            set,
            role,
            document_type: require_nonblank(columns[3], "document_type", line_number)?,
            layout: require_nonblank(columns[4], "layout", line_number)?,
            in_scope,
            known_issues: optional_text(columns[6]),
            captured: require_nonblank(columns[7], "captured", line_number)?,
            expected_extraction,
            limit_scale_hint,
            expected_file: optional_text(columns[10]),
            old,
            new,
        });
    }
    if !header_seen {
        return Err(BenchError::InvalidInput(
            "revision manifest is missing its header row".to_owned(),
        ));
    }
    if pairs.is_empty() {
        return Err(BenchError::InvalidInput(
            "revision manifest records no pairs".to_owned(),
        ));
    }
    Ok(pairs)
}

fn validate_header(columns: &[&str], line_number: usize) -> Result<()> {
    if columns != MANIFEST_HEADER.as_slice() {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest header on line {line_number} does not match the documented column order"
        )));
    }
    Ok(())
}

fn invalid_row(line_number: usize, column: &str, value: &str) -> BenchError {
    BenchError::InvalidInput(format!(
        "revision manifest row {line_number} has invalid {column} {value:?}"
    ))
}

fn require_nonblank(value: &str, column: &str, line_number: usize) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has blank {column}"
        )));
    }
    Ok(trimmed.to_owned())
}

fn optional_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "-" {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn parse_bool(value: &str, column: &str, line_number: usize) -> Result<bool> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid {column} {other:?}: expected true or false"
        ))),
    }
}

fn parse_limit_scale_hint(value: &str, line_number: usize) -> Result<f64> {
    let parsed: f64 = value.trim().parse().map_err(|_| {
        BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid limit_scale_hint {value:?}"
        ))
    })?;
    if !parsed.is_finite() || parsed < 1.0 {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has limit_scale_hint {parsed} below 1.0"
        )));
    }
    Ok(parsed)
}

fn parse_side_provenance(
    url: &str,
    byte_count: &str,
    sha256: &str,
    side: &'static str,
    line_number: usize,
) -> Result<SideProvenance> {
    let byte_count: u64 = byte_count.trim().parse().map_err(|_| {
        BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid {side}_byte_count {byte_count:?}"
        ))
    })?;
    Ok(SideProvenance {
        url: require_nonblank(url, &format!("{side}_url"), line_number)?,
        byte_count,
        sha256: validate_sha256(sha256, side, line_number)?,
    })
}

fn validate_sha256(value: &str, side: &str, line_number: usize) -> Result<String> {
    let trimmed = value.trim();
    let valid = trimmed.len() == SHA256_HEX_LEN
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid {side}_sha256 {trimmed:?}: expected 64 lowercase hex characters"
        )));
    }
    Ok(trimmed.to_owned())
}

pub fn load_expected_document(expected_json: &str) -> Result<ExpectedDocument> {
    let document: ExpectedDocument = serde_json::from_str(expected_json).map_err(|error| {
        BenchError::InvalidInput(format!("invalid expected-revision JSON: {error}"))
    })?;
    if document.version != 1 {
        return Err(BenchError::InvalidInput(format!(
            "expected-revision JSON has unsupported version {}; version 1 is required",
            document.version
        )));
    }
    if document.pair.trim().is_empty() || document.reviewed_on.trim().is_empty() {
        return Err(BenchError::InvalidInput(
            "expected-revision JSON requires nonblank pair and reviewed_on values".to_owned(),
        ));
    }
    let mut seen_ids = HashSet::new();
    for change in &document.changes {
        if change.id.trim().is_empty() || !seen_ids.insert(change.id.clone()) {
            return Err(BenchError::InvalidInput(format!(
                "expected-revision JSON change id {:?} is blank or duplicated",
                change.id
            )));
        }
        let old_present = change
            .old_quote
            .as_deref()
            .is_some_and(|quote| !collapse_whitespace(quote).is_empty());
        let new_present = change
            .new_quote
            .as_deref()
            .is_some_and(|quote| !collapse_whitespace(quote).is_empty());
        let shape_valid = match change.kind {
            ExpectedKind::Replacement | ExpectedKind::Move => old_present && new_present,
            ExpectedKind::Insertion => new_present && !old_present,
            ExpectedKind::Deletion => old_present && !new_present,
        };
        if !shape_valid {
            return Err(BenchError::InvalidInput(format!(
                "expected-revision JSON change {} of kind {} violates the quote rules: \
                 replacement and move require both quotes, insertion only new_quote, deletion only old_quote",
                change.id,
                change.kind.name()
            )));
        }
    }
    Ok(document)
}

pub fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn validate_limit_scale(scale: f64) -> Result<f64> {
    if !scale.is_finite() || scale < 1.0 {
        return Err(BenchError::InvalidInput(format!(
            "limit-scale must be a finite value >= 1.0 so documented defaults are never silently weakened, found {scale}"
        )));
    }
    Ok(scale)
}

fn scaled_pipeline_options(scale: f64) -> PipelineOptions {
    let mut options = PipelineOptions::default();
    let bump = |value: usize| ((value as f64 * scale) as usize).max(value);
    options.max_ngram_token_elements = bump(options.max_ngram_token_elements);
    options.alignment.max_candidate_visits = bump(options.alignment.max_candidate_visits);
    options.alignment.max_dp_cells = bump(options.alignment.max_dp_cells);
    options.diff.max_tokens = bump(options.diff.max_tokens);
    options.diff.max_edit_distance = bump(options.diff.max_edit_distance);
    options
}

fn side_cache_path(cache_dir: &Path, pair_id: &str, side: &str) -> PathBuf {
    cache_dir.join(format!("{pair_id}-{side}.pdf"))
}

fn verify_provenance(
    provenance: &SideProvenance,
    path: &Path,
    side: &str,
) -> std::result::Result<(), String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "missing or unreadable {side} download {}: {error}",
            path.display()
        )
    })?;
    if bytes.len() as u64 != provenance.byte_count {
        return Err(format!(
            "{side} download {} has {} bytes but the manifest captured {} bytes",
            path.display(),
            bytes.len(),
            provenance.byte_count
        ));
    }
    let digest = Sha256::digest(&bytes);
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if actual != provenance.sha256 {
        return Err(format!(
            "{side} download {} has sha256 {actual} but the manifest captured {}",
            path.display(),
            provenance.sha256
        ));
    }
    Ok(())
}

fn build_block_map(blocks: &[BlockText]) -> HashMap<u64, &BlockText> {
    blocks.iter().map(|block| (block.block.0, block)).collect()
}

fn resolve_span(map: &HashMap<u64, &BlockText>, span: &TextSpan) -> Option<(String, usize)> {
    if span.blocks.is_empty() {
        return None;
    }
    let mut tokens: Vec<ComparableToken> = Vec::new();
    for (position, block_id) in span.blocks.iter().enumerate() {
        let next = map.get(&block_id.0)?.canonical.comparable_tokens().ok()?;
        if position == 0 {
            tokens.extend(next);
            continue;
        }
        append_with_separator(&mut tokens, span.separator? == BlockSeparator::Space, &next);
    }
    if span.comparable_range.end > tokens.len()
        || span.comparable_range.start > span.comparable_range.end
    {
        return None;
    }
    let comparable_len = span.comparable_range.end - span.comparable_range.start;
    let mut text = String::new();
    for token in &tokens {
        if let ComparableToken::Scalar(scalar) = token {
            text.push(*scalar);
        }
    }
    if span.canonical_range.start > span.canonical_range.end
        || span.canonical_range.end > text.chars().count()
    {
        return None;
    }
    let selected: String = text
        .chars()
        .skip(span.canonical_range.start)
        .take(span.canonical_range.end - span.canonical_range.start)
        .collect();
    Some((selected, comparable_len))
}

/// Mirrors the core block-separator concatenation rule without exposing the
/// `pub(crate)` implementation detail.
fn append_with_separator(
    tokens: &mut Vec<ComparableToken>,
    space_separator: bool,
    next: &[ComparableToken],
) {
    let insert_space = space_separator
        && !tokens.last().is_some_and(is_space_token)
        && !next.first().is_some_and(is_space_token);
    if insert_space {
        tokens.push(ComparableToken::Scalar(' '));
    }
    tokens.extend_from_slice(next);
}

fn is_space_token(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(' '))
}

fn flatten_actual_changes(
    comparison: &Comparison,
    blocks_by_side: [&HashMap<u64, &BlockText>; 2],
) -> Vec<ActualChange> {
    comparison
        .changes
        .iter()
        .map(|change| {
            let old_resolved = change
                .old_span
                .as_ref()
                .map(|span| resolve_span(blocks_by_side[0], span));
            let new_resolved = change
                .new_span
                .as_ref()
                .map(|span| resolve_span(blocks_by_side[1], span));
            let resolvable = old_resolved.as_ref().is_none_or(Option::is_some)
                && new_resolved.as_ref().is_none_or(Option::is_some);
            let unwrap_resolved = |resolved: Option<Option<(String, usize)>>| match resolved {
                Some(Some((text, len))) => (Some(collapse_whitespace(&text)), Some(len)),
                _ => (None, None),
            };
            let (old_text, old_len) = unwrap_resolved(old_resolved);
            let (new_text, new_len) = unwrap_resolved(new_resolved);
            ActualChange {
                kind: change.kind,
                old_text,
                new_text,
                old_comparable_len: old_len,
                new_comparable_len: new_len,
                resolvable,
            }
        })
        .collect()
}

fn contains_needle(haystack: Option<&str>, needle: Option<&str>) -> bool {
    match (haystack, needle) {
        (_, None) => true,
        (Some(haystack), Some(needle)) => haystack.contains(needle),
        (None, Some(_)) => false,
    }
}

fn match_changes(expected: &[ExpectedChange], actuals: &[ActualChange]) -> MatchOutcome {
    let collapsed_actuals: Vec<ActualTexts> = actuals
        .iter()
        .map(|change| ActualTexts {
            old: change.old_text.as_deref().map(collapse_whitespace),
            new: change.new_text.as_deref().map(collapse_whitespace),
        })
        .collect();
    let mut claimed_actuals = HashSet::new();
    let mut kind_agreements = 0_usize;
    for change in expected {
        let needle_old = change.old_quote.as_deref().map(collapse_whitespace);
        let needle_new = change.new_quote.as_deref().map(collapse_whitespace);
        for (index, actual) in collapsed_actuals.iter().enumerate() {
            if claimed_actuals.contains(&index) || !actuals[index].resolvable {
                continue;
            }
            let old_ok = contains_needle(actual.old.as_deref(), needle_old.as_deref());
            let new_ok = contains_needle(actual.new.as_deref(), needle_new.as_deref());
            if old_ok && new_ok {
                claimed_actuals.insert(index);
                if change.kind.agrees_with(actuals[index].kind) {
                    kind_agreements += 1;
                }
                break;
            }
        }
    }
    MatchOutcome {
        matched: claimed_actuals.len(),
        kind_agreements,
        claimed_actuals,
    }
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(numerator as f64 / denominator as f64)
    }
}

pub fn compute_quality(
    annotation: Annotation,
    expected: &[ExpectedChange],
    actuals: &[ActualChange],
) -> QualityMetrics {
    let outcome = match_changes(expected, actuals);
    let reported = actuals.len();
    let unmatched_tiny = actuals
        .iter()
        .enumerate()
        .filter(|(index, change)| {
            !outcome.claimed_actuals.contains(index) && change.resolvable && is_tiny(change)
        })
        .count();
    QualityMetrics {
        annotation,
        expected_changes: expected.len(),
        reported_changes: reported,
        recall: ratio(outcome.matched, expected.len()),
        precision: (annotation == Annotation::Complete)
            .then(|| ratio(outcome.matched, reported))
            .flatten(),
        kind_accuracy: (outcome.matched > 0)
            .then(|| ratio(outcome.kind_agreements, outcome.matched))
            .flatten(),
        reported_hunks_per_matched_change: (outcome.matched > 0)
            .then(|| reported as f64 / outcome.matched as f64),
        review_hunks_per_expected_change: (annotation == Annotation::Complete)
            .then(|| ratio(reported, expected.len()))
            .flatten(),
        unmatched_tiny_changes: unmatched_tiny,
        unresolvable_reported_spans: actuals.iter().filter(|change| !change.resolvable).count(),
    }
}

/// One- or two-token edits on every existing side, the shape of changes bad
/// alignment tends to fabricate. Changes without any resolved span length
/// (unresolvable spans) are never tiny: they are counted separately as
/// unresolvable and must not inflate the suspicious-edit signal.
fn is_tiny(change: &ActualChange) -> bool {
    let mut longest_existing: Option<usize> = None;
    for length in [change.old_comparable_len, change.new_comparable_len]
        .into_iter()
        .flatten()
    {
        longest_existing = Some(longest_existing.map_or(length, |current| current.max(length)));
    }
    longest_existing.is_some_and(|longest| longest <= 2)
}

fn unresolved_token_shares(comparison: &Comparison) -> (Option<f64>, Option<f64>) {
    let region_tokens = |side: DocumentSide| -> usize {
        comparison
            .unresolved_regions
            .iter()
            .filter_map(|region| match side {
                DocumentSide::Old => region.old_span.as_ref(),
                DocumentSide::New => region.new_span.as_ref(),
            })
            .map(|span| {
                span.comparable_range
                    .end
                    .saturating_sub(span.comparable_range.start)
            })
            .sum()
    };
    let share = |total: usize, sum: usize| (total > 0).then(|| sum as f64 / total as f64);
    (
        share(
            comparison.old_coverage.total_tokens,
            region_tokens(DocumentSide::Old),
        ),
        share(
            comparison.new_coverage.total_tokens,
            region_tokens(DocumentSide::New),
        ),
    )
}

fn change_kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Replacement => "replacement",
        ChangeKind::Insertion => "insertion",
        ChangeKind::Deletion => "deletion",
        ChangeKind::Move => "move",
    }
}

fn issue_lines(extraction: &report::ExtractionStatus) -> Vec<IssueLine> {
    extraction
        .issues
        .iter()
        .map(|issue| IssueLine {
            side: match issue.side {
                DocumentSide::Old => "old",
                DocumentSide::New => "new",
            },
            kind: match issue.kind {
                ExtractionIssueKind::Unsupported => "unsupported",
                ExtractionIssueKind::Unresolved => "unresolved",
            },
            scope: match issue.scope {
                ExtractionScope::Document => "document".to_owned(),
                ExtractionScope::Page(page) => format!("page {}", page.0),
            },
            description: issue.description.clone(),
        })
        .collect()
}

struct PairRunContext<'a> {
    cache_dir: &'a Path,
    manifest_dir: &'a Path,
    /// Uniform override; when unset every pair uses its manifest hint.
    limit_scale_override: Option<f64>,
    compare: bool,
}

fn run_pair(pair: &RevisionPair, context: &PairRunContext<'_>) -> PairRunReport {
    let started = Instant::now();
    let effective_scale = context
        .limit_scale_override
        .unwrap_or(pair.limit_scale_hint);
    let old_path = side_cache_path(context.cache_dir, &pair.pair_id, "old");
    let new_path = side_cache_path(context.cache_dir, &pair.pair_id, "new");
    let mut record = PairRunReport {
        pair_id: pair.pair_id.clone(),
        set: pair.set.label(),
        role: pair.role.label(),
        document_type: pair.document_type.clone(),
        in_scope: pair.in_scope,
        provenance_verified: false,
        compared: false,
        extraction_complete: None,
        comparison_complete: None,
        extraction_issues: Vec::new(),
        coverage_old: None,
        coverage_new: None,
        coverage_comparison: None,
        unresolved_regions: None,
        unresolved_old_token_share: None,
        unresolved_new_token_share: None,
        reported_content_changes: None,
        formatting_only_changes: None,
        uncertain_changes: None,
        reported_changes_preview: Vec::new(),
        quality: None,
        quality_skipped_reason: None,
        resource_limit_failure: None,
        candidate_visits: None,
        candidate_visits_required: None,
        candidate_visits_required_exact: None,
        candidate_visits_required_ngram: None,
        candidate_visits_required_short_fallback: None,
        max_candidate_visits: None,
        candidate_visit_pressure: None,
        runtime_ms: 0,
        limit_scale_used: effective_scale,
        status: PairRunStatus::Ok,
        failure: None,
    };

    let verification = verify_provenance(&pair.old, &old_path, "old")
        .and_then(|()| verify_provenance(&pair.new, &new_path, "new"));
    if let Err(reason) = verification {
        record.failure = Some(reason);
        return finish(record, started);
    }
    record.provenance_verified = true;
    if !context.compare {
        return finish(record, started);
    }

    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let outcome =
        match run_extraction_and_comparison(&source, &old_path, &new_path, effective_scale) {
            Ok(ComparisonWithMetrics {
                outcome,
                metrics,
                pressure,
            }) => {
                metrics.apply_to(&mut record);
                record.candidate_visit_pressure = pressure;
                outcome
            }
            Err(RevisionRunError::Read(reason)) => {
                record.failure = Some(reason);
                return finish(record, started);
            }
            Err(RevisionRunError::Limit {
                message,
                metrics,
                pressure,
            }) => {
                record.resource_limit_failure = Some(message);
                metrics.apply_to(&mut record);
                record.candidate_visit_pressure = pressure.map(|pressure| *pressure);
                record.quality_skipped_reason = Some(QUALITY_SKIP_RESOURCE_LIMIT.to_owned());
                return finish(record, started);
            }
            Err(RevisionRunError::Other(stage, message)) => {
                record.failure = Some(format!("{stage}: {message}"));
                return finish(record, started);
            }
        };
    record.compared = true;

    let summary = match summarize(&outcome.comparison, &outcome.extraction) {
        Ok(summary) => summary,
        Err(error) => {
            record.failure = Some(format!("summary failed: {error}"));
            return finish(record, started);
        }
    };
    let extraction_complete = summary.old_extraction_complete && summary.new_extraction_complete;
    record.extraction_complete = Some(extraction_complete);
    record.comparison_complete = Some(summary.comparison_complete);
    record.extraction_issues = issue_lines(&outcome.extraction);
    record.coverage_old = summary.old_alignment_coverage;
    record.coverage_new = summary.new_alignment_coverage;
    record.coverage_comparison = summary.comparison_coverage;
    record.unresolved_regions = Some(summary.unresolved_regions);
    let (old_share, new_share) = unresolved_token_shares(&outcome.comparison);
    record.unresolved_old_token_share = old_share;
    record.unresolved_new_token_share = new_share;
    record.reported_content_changes = Some(summary.content_changes);
    record.formatting_only_changes = Some(summary.formatting_only_changes);
    record.uncertain_changes = Some(summary.uncertain_changes);

    match (pair.expected_extraction, extraction_complete) {
        (ExpectedExtraction::Complete, true) | (ExpectedExtraction::Incomplete, false) => {}
        (ExpectedExtraction::Complete, false) => {
            record.failure = Some(
                "manifest expected complete extraction but extraction was incomplete".to_owned(),
            );
        }
        (ExpectedExtraction::Incomplete, true) => {
            record.failure =
                Some("manifest expected incomplete extraction but extraction completed".to_owned());
        }
    }

    let expected_document = pair.expected_file.as_ref().map(|file| {
        let path = context.manifest_dir.join(file);
        fs::read_to_string(&path)
            .map_err(|error| {
                format!(
                    "cannot read expected annotations {}: {error}",
                    path.display()
                )
            })
            .and_then(|text| load_expected_document(&text).map_err(|error| error.to_string()))
            .and_then(|document| {
                if document.pair == pair.pair_id {
                    Ok(document)
                } else {
                    Err(format!(
                        "expected annotations {} describe pair {:?} but this manifest row is {:?}",
                        path.display(),
                        document.pair,
                        pair.pair_id
                    ))
                }
            })
    });

    let expected = match expected_document {
        None => None,
        Some(Ok(document)) => Some(document),
        Some(Err(reason)) => {
            if record.failure.is_none() {
                record.failure = Some(reason);
            }
            None
        }
    };

    let actuals = if extraction_complete {
        let old_map = build_block_map(&outcome.old_blocks);
        let new_map = build_block_map(&outcome.new_blocks);
        Some(flatten_actual_changes(
            &outcome.comparison,
            [&old_map, &new_map],
        ))
    } else {
        None
    };

    if let Some(actuals) = &actuals {
        record.reported_changes_preview = actuals
            .iter()
            .map(|change| ReportedChangeText {
                kind: change_kind_name(change.kind),
                old_text: change.old_text.as_deref().map(truncate_preview),
                new_text: change.new_text.as_deref().map(truncate_preview),
            })
            .collect();
    }

    match (expected, extraction_complete) {
        (Some(document), true) => {
            let actuals = actuals.unwrap_or_default();
            record.quality = Some(compute_quality(
                document.annotation,
                &document.changes,
                &actuals,
            ));
        }
        (Some(_), false) => {
            record.quality_skipped_reason = Some(QUALITY_SKIP_INCOMPLETE_EXTRACTION.to_owned());
        }
        (None, _) if pair.expected_file.is_some() => {}
        (None, _) => {
            record.quality_skipped_reason = Some(QUALITY_SKIP_NO_ANNOTATIONS.to_owned());
        }
    }

    finish(record, started)
}

/// Single exit point that derives the final status: an explicit failure wins,
/// then a resource-limit stop; everything else stays `Ok`.
fn finish(mut record: PairRunReport, started: Instant) -> PairRunReport {
    record.status = if record.failure.is_some() {
        PairRunStatus::Failed
    } else if record.resource_limit_failure.is_some() {
        PairRunStatus::Limit
    } else {
        PairRunStatus::Ok
    };
    record.runtime_ms = started.elapsed().as_millis();
    record
}

#[derive(Debug)]
enum RevisionRunError {
    Read(String),
    Limit {
        message: String,
        metrics: Box<VisitMetrics>,
        pressure: Option<Box<CandidateVisitPressure>>,
    },
    Other(&'static str, String),
}

type RevisionOutcome = pdfdelta_core::pipeline::ComparisonOutcome;

/// Alignment candidate visit metrics: attempted charge, required full sum,
/// its exact/ngram/short-fallback components, and the budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct VisitMetrics {
    candidate_visits: Option<usize>,
    candidate_visits_required: Option<usize>,
    candidate_visits_required_exact: Option<usize>,
    candidate_visits_required_ngram: Option<usize>,
    candidate_visits_required_short_fallback: Option<usize>,
    max_candidate_visits: Option<usize>,
}

impl VisitMetrics {
    fn apply_to(self, record: &mut PairRunReport) {
        record.candidate_visits = self.candidate_visits;
        record.candidate_visits_required = self.candidate_visits_required;
        record.candidate_visits_required_exact = self.candidate_visits_required_exact;
        record.candidate_visits_required_ngram = self.candidate_visits_required_ngram;
        record.candidate_visits_required_short_fallback =
            self.candidate_visits_required_short_fallback;
        record.max_candidate_visits = self.max_candidate_visits;
    }
}

/// Validates one alignment metrics record. Valid states: alignment reached
/// with the required sum completed and either a full component breakdown
/// (inverted index) or no breakdown (generic generator); a limit stop where
/// the required sum is unavailable; and alignment never reached. Any other
/// partial state, or a component sum that disagrees with the required
/// total, is a contract violation and is reported as an error rather than
/// silently treated as a measurement.
fn validate_visit_metrics(metrics: VisitMetrics) -> std::result::Result<VisitMetrics, String> {
    let VisitMetrics {
        candidate_visits,
        candidate_visits_required,
        candidate_visits_required_exact,
        candidate_visits_required_ngram,
        candidate_visits_required_short_fallback,
        max_candidate_visits,
    } = metrics;
    let components = (
        candidate_visits_required_exact,
        candidate_visits_required_ngram,
        candidate_visits_required_short_fallback,
    );
    let valid = match (
        candidate_visits,
        candidate_visits_required,
        max_candidate_visits,
    ) {
        (Some(_), Some(_), Some(_)) => {
            matches!(components, (Some(_), Some(_), Some(_)) | (None, None, None))
        }
        (Some(_), None, Some(_)) | (None, None, None) => matches!(components, (None, None, None)),
        _ => false,
    };
    if !valid {
        return Err(format!(
            "alignment metrics contract violation: candidate_visits={candidate_visits:?}, candidate_visits_required={candidate_visits_required:?}, candidate_visits_required_exact={candidate_visits_required_exact:?}, candidate_visits_required_ngram={candidate_visits_required_ngram:?}, candidate_visits_required_short_fallback={candidate_visits_required_short_fallback:?}, max_candidate_visits={max_candidate_visits:?}"
        ));
    }
    if let (Some(required), Some(exact), Some(ngram), Some(short_fallback)) = (
        candidate_visits_required,
        candidate_visits_required_exact,
        candidate_visits_required_ngram,
        candidate_visits_required_short_fallback,
    ) {
        let components_sum = exact
            .checked_add(ngram)
            .and_then(|sum| sum.checked_add(short_fallback));
        if components_sum != Some(required) {
            return Err(format!(
                "alignment metrics contract violation: required candidate components {exact}+{ngram}+{short_fallback} do not sum to {required}"
            ));
        }
    }
    Ok(metrics)
}

/// Extracts the alignment candidate visit metrics from the diagnostics.
/// Returns all-`None` metrics when alignment was never reached or recorded
/// no charge; a partial record is a contract violation and is reported as
/// an error.
fn alignment_visit_metrics(
    diagnostics: &PipelineDiagnostics,
) -> std::result::Result<VisitMetrics, String> {
    let Some(record) = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::Alignment)
    else {
        return Ok(VisitMetrics::default());
    };
    validate_visit_metrics(VisitMetrics {
        candidate_visits: record.metrics.candidate_visits,
        candidate_visits_required: record.metrics.candidate_visits_required,
        candidate_visits_required_exact: record.metrics.candidate_visits_required_exact,
        candidate_visits_required_ngram: record.metrics.candidate_visits_required_ngram,
        candidate_visits_required_short_fallback: record
            .metrics
            .candidate_visits_required_short_fallback,
        max_candidate_visits: record.metrics.max_candidate_visits,
    })
}

/// Resolves the supplementary pressure from the alignment charge and the
/// pressure attempt. Alignment reached requires a successful attempt; an
/// absent attempt is an internal contract violation and fails the pair.
fn resolve_pressure(
    candidate_visits: Option<usize>,
    pressure_result: Option<Result<CandidateVisitPressure>>,
) -> std::result::Result<Option<CandidateVisitPressure>, RevisionRunError> {
    match (candidate_visits, pressure_result) {
        (Some(_), Some(Ok(pressure))) => Ok(Some(pressure)),
        (Some(_), Some(Err(error))) => Err(RevisionRunError::Other(
            "candidate visit pressure",
            error.to_string(),
        )),
        (None, _) => Ok(None),
        (Some(_), None) => Err(RevisionRunError::Other(
            "candidate visit pressure contract violation",
            "alignment reached without a pressure attempt".to_owned(),
        )),
    }
}

/// Comparison outcome plus the alignment candidate visit metrics and
/// supplementary pressure.
#[derive(Debug)]
struct ComparisonWithMetrics {
    outcome: RevisionOutcome,
    metrics: VisitMetrics,
    pressure: Option<CandidateVisitPressure>,
}

/// Compares two extracted outcomes and returns the alignment candidate
/// visit metrics and the supplementary all-old-block pressure alongside the
/// outcome; a candidate limit stop carries the metrics and the pressure in
/// the error. A metrics contract violation takes priority over the core
/// result. Once alignment is reached, a pressure measurement failure is a
/// benchmark instrumentation failure, not missing supplementary
/// information: it takes priority over the core Ok/Limit result and
/// surfaces as `Failed`.
fn compare_outcomes_with_metrics(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
) -> std::result::Result<ComparisonWithMetrics, RevisionRunError> {
    // Measure the supplementary pressure from the borrowed documents before
    // the outcomes are consumed; whether it is required depends on whether
    // alignment is reached below.
    let pressure_result = if old.is_complete() && new.is_complete() {
        Some(evaluate_candidate_visit_pressure(
            old.document(),
            new.document(),
            options,
        ))
    } else {
        None
    };
    let mut diagnostics = PipelineDiagnostics::new();
    let result = compare_extraction_outcomes_with_diagnostics(old, new, options, &mut diagnostics);
    let metrics = alignment_visit_metrics(&diagnostics).map_err(|message| {
        RevisionRunError::Other("alignment metrics contract violation", message)
    })?;
    let pressure = resolve_pressure(metrics.candidate_visits, pressure_result)?;
    let outcome = result.map_err(|error| match error {
        pdfdelta_core::Error::LimitExceeded { .. } => RevisionRunError::Limit {
            message: error.to_string(),
            metrics: Box::new(metrics),
            pressure: pressure.map(Box::new),
        },
        other => RevisionRunError::Other("comparison failed", other.to_string()),
    })?;
    Ok(ComparisonWithMetrics {
        outcome,
        metrics,
        pressure,
    })
}

fn run_extraction_and_comparison(
    source: &ParserBackedGlyphSource<LopdfParser, ContentStreamGlyphExtractor>,
    old_path: &Path,
    new_path: &Path,
    limit_scale: f64,
) -> std::result::Result<ComparisonWithMetrics, RevisionRunError> {
    let read = |path: &Path| {
        fs::read(path).map_err(|error| {
            RevisionRunError::Read(format!("cannot read {}: {error}", path.display()))
        })
    };
    let old_bytes = read(old_path)?;
    let new_bytes = read(new_path)?;
    let extract = |bytes: Vec<u8>| {
        // Backend failures are fatal in core by design; the benchmark records
        // them as document-scoped unresolved outcomes so manifest
        // expectations can classify the pair instead of aborting the run.
        match source.extract_outcome(
            Arc::from(bytes),
            ParseLimits::default(),
            ExtractionLimits::default(),
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                let issue = ExtractionIssue::new(
                    ExtractionIssueKind::Unresolved,
                    ExtractionScope::Document,
                    format!("backend failure: {error}"),
                )
                .expect("backend failure description is never blank");
                ExtractionOutcome::new(Document::new(Vec::new()), vec![issue])
                    .expect("a single document-scoped issue is always a valid outcome")
            }
        }
    };
    let old_outcome = extract(old_bytes);
    let new_outcome = extract(new_bytes);
    let options = scaled_pipeline_options(limit_scale);
    compare_outcomes_with_metrics(old_outcome, new_outcome, options)
}

pub fn run_revision_benchmark(
    manifest_path: &Path,
    cache_dir: &Path,
    set_filter: Option<PairSet>,
    pair_filter: Option<&str>,
    limit_scale: Option<f64>,
    compare: bool,
) -> Result<Vec<PairRunReport>> {
    if let Some(scale) = limit_scale {
        validate_limit_scale(scale)?;
    }
    let manifest_text = fs::read_to_string(manifest_path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "cannot read revision manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let mut pairs = parse_manifest(&manifest_text)?;
    if let Some(set_filter) = set_filter {
        pairs.retain(|pair| pair.set == set_filter);
    }
    if let Some(pair_filter) = pair_filter {
        if !pairs.iter().any(|pair| pair.pair_id == pair_filter) {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest {} records no pair {pair_filter:?}",
                manifest_path.display()
            )));
        }
        pairs.retain(|pair| pair.pair_id == pair_filter);
    }
    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let context = PairRunContext {
        cache_dir,
        manifest_dir,
        limit_scale_override: limit_scale,
        compare,
    };
    Ok(pairs.iter().map(|pair| run_pair(pair, &context)).collect())
}

pub fn summarize_reports(reports: &[PairRunReport]) -> String {
    let healthy = reports.iter().filter(|record| record.healthy()).count();
    let limited = reports
        .iter()
        .filter(|record| record.status == PairRunStatus::Limit)
        .count();
    let failed = reports.len() - healthy - limited;
    format!(
        "{healthy}/{} revision pairs healthy; {limited} stopped at resource limits; {failed} failed",
        reports.len()
    )
}

/// Resolves a path to its canonical parent directory and file name, rejecting
/// missing parent directories and paths without a file name.
pub fn normalize_output_destination(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let canonical_parent = parent.canonicalize().map_err(|error| {
        BenchError::InvalidInput(format!(
            "destination directory does not exist or cannot be resolved {}: {error}",
            parent.display()
        ))
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        BenchError::InvalidInput(format!(
            "destination path {} must have a file name",
            path.display()
        ))
    })?;
    Ok(canonical_parent.join(file_name))
}

static TEMP_PUBLISH_COUNTER: AtomicU64 = AtomicU64::new(0);
pub(crate) const MAX_TEMP_CREATE_ATTEMPTS: usize = 64;

fn create_temp_artifact_with_counter(
    parent: &Path,
    counter: &AtomicU64,
) -> Result<(fs::File, PathBuf)> {
    let pid = std::process::id();
    for _ in 0..MAX_TEMP_CREATE_ATTEMPTS {
        let seq = counter.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(".pdfbench-artifact-{pid}-{seq}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((file, temp_path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                continue;
            }
            Err(error) => {
                return Err(BenchError::Publication(format!(
                    "cannot create temporary artifact {}: {error}",
                    temp_path.display()
                )));
            }
        }
    }
    Err(BenchError::Publication(format!(
        "exhausted {MAX_TEMP_CREATE_ATTEMPTS} attempts creating unique temporary artifact in {}",
        parent.display()
    )))
}

/// Atomically publishes `bytes` to a new file at `path`, refusing to overwrite
/// any existing destination file, symlink, or directory. Cleans up temporary
/// files on failure.
pub fn publish_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    publish_new_file_with_counter(path, bytes, &TEMP_PUBLISH_COUNTER)
}

pub(crate) fn publish_new_file_with_counter(
    path: &Path,
    bytes: &[u8],
    counter: &AtomicU64,
) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    if !parent.exists() {
        return Err(BenchError::Publication(format!(
            "destination directory does not exist: {}",
            parent.display()
        )));
    }

    if path.symlink_metadata().is_ok() {
        return Err(BenchError::Publication(format!(
            "destination path already exists: {}",
            path.display()
        )));
    }

    let (mut temp_file, temp_path) = create_temp_artifact_with_counter(parent, counter)?;

    let write_result = (|| -> Result<()> {
        temp_file.write_all(bytes).map_err(|error| {
            BenchError::Publication(format!(
                "cannot write temporary artifact {}: {error}",
                temp_path.display()
            ))
        })?;
        temp_file.flush().map_err(|error| {
            BenchError::Publication(format!(
                "cannot flush temporary artifact {}: {error}",
                temp_path.display()
            ))
        })?;
        temp_file.sync_all().map_err(|error| {
            BenchError::Publication(format!(
                "cannot sync temporary artifact {}: {error}",
                temp_path.display()
            ))
        })?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    let link_result = fs::hard_link(&temp_path, path);
    let _ = fs::remove_file(&temp_path);

    link_result.map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            BenchError::Publication(format!(
                "destination path already exists: {}",
                path.display()
            ))
        } else {
            BenchError::Publication(format!(
                "cannot publish artifact to {}: {error}",
                path.display()
            ))
        }
    })
}

/// Writes every record as a pretty-printed JSON array to a new file, refusing to
/// overwrite an existing destination path via atomic publication.
pub fn write_reports_json(path: &Path, reports: &[PairRunReport]) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(reports).map_err(|error| {
        BenchError::Publication(format!("cannot serialize revision JSON report: {error}"))
    })?;
    publish_new_file(path, &bytes)
}

/// Compact machine-readable revision summary document.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RevisionSummaryReport {
    pub schema_version: u32,
    pub records: Vec<RevisionSummaryRecord>,
}

impl RevisionSummaryReport {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn from_reports(reports: &[PairRunReport]) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            records: reports
                .iter()
                .map(RevisionSummaryRecord::from_pair_report)
                .collect(),
        }
    }
}

/// A compact, stable machine-readable record for one revision pair evaluation.
/// Contains all evidence fields needed to reproduce benchmark claims without
/// runtime measurements, raw text previews, or host-specific paths. Detailed
/// error diagnostics remain available in the full report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RevisionSummaryRecord {
    pub pair_id: String,
    pub set: String,
    pub role: String,
    pub in_scope: bool,
    pub status: PairRunStatus,
    pub provenance_verified: bool,
    pub compared: bool,
    pub extraction_complete: Option<bool>,
    pub comparison_complete: Option<bool>,
    pub limit_scale_used: f64,
    pub resource_limit_failure: Option<String>,
    pub coverage_old: Option<f64>,
    pub coverage_new: Option<f64>,
    pub coverage_comparison: Option<f64>,
    pub unresolved_regions: Option<usize>,
    pub unresolved_old_token_share: Option<f64>,
    pub unresolved_new_token_share: Option<f64>,
    pub reported_content_changes: Option<usize>,
    pub reported_formatting_changes: Option<usize>,
    pub reported_uncertain_changes: Option<usize>,
    pub quality: Option<QualityMetrics>,
    pub quality_skipped_reason: Option<String>,
}

impl RevisionSummaryRecord {
    pub fn from_pair_report(report: &PairRunReport) -> Self {
        Self {
            pair_id: report.pair_id.clone(),
            set: report.set.to_owned(),
            role: report.role.to_owned(),
            in_scope: report.in_scope,
            status: report.status,
            provenance_verified: report.provenance_verified,
            compared: report.compared,
            extraction_complete: report.extraction_complete,
            comparison_complete: report.comparison_complete,
            limit_scale_used: report.limit_scale_used,
            resource_limit_failure: report.resource_limit_failure.clone(),
            coverage_old: report.coverage_old,
            coverage_new: report.coverage_new,
            coverage_comparison: report.coverage_comparison,
            unresolved_regions: report.unresolved_regions,
            unresolved_old_token_share: report.unresolved_old_token_share,
            unresolved_new_token_share: report.unresolved_new_token_share,
            reported_content_changes: report.reported_content_changes,
            reported_formatting_changes: report.formatting_only_changes,
            reported_uncertain_changes: report.uncertain_changes,
            quality: report.quality,
            quality_skipped_reason: report.quality_skipped_reason.clone(),
        }
    }
}

/// Writes a compact summary of every record as pretty-printed JSON with a trailing
/// newline to a new file, refusing to overwrite an existing destination path via
/// atomic publication.
pub fn write_summary_json(path: &Path, reports: &[PairRunReport]) -> Result<()> {
    let summary = RevisionSummaryReport::from_reports(reports);
    let mut bytes = serde_json::to_vec_pretty(&summary).map_err(|error| {
        BenchError::Publication(format!(
            "cannot serialize revision summary JSON report: {error}"
        ))
    })?;
    bytes.push(b'\n');
    publish_new_file(path, &bytes)
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::{
        alignment::AlignmentOptions,
        model::{
            DecodedText, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect, TextRenderMode,
            Vec2,
        },
        pdf::ObjectRef,
    };

    use super::*;

    fn manifest_row(
        pair_id: &str,
        set: &str,
        role: &str,
        in_scope: &str,
        extraction: &str,
    ) -> String {
        format!(
            "{pair_id}\t{set}\t{role}\tgovernment-guidance\tsingle-column\t{in_scope}\t-\t2026-08-24\t{extraction}\t1.0\texpected/{pair_id}.json\t\
             https://example.test/{pair_id}-old.pdf\t100\t{}\thttps://example.test/{pair_id}-new.pdf\t200\t{}",
            "a".repeat(64),
            "b".repeat(64)
        )
    }

    fn full_manifest() -> String {
        format!(
            "# captured fixtures\n{}\n{}\n{}\n{}\n",
            MANIFEST_HEADER.join("\t"),
            manifest_row("alpha", "dev", "standard", "true", "complete"),
            manifest_row("beta", "holdout", "stress", "false", "incomplete"),
            manifest_row("gamma", "holdout", "standard", "true", "complete")
        )
    }

    #[test]
    fn manifest_rows_round_trip_through_the_parser() {
        let pairs = parse_manifest(&full_manifest()).expect("valid manifest");
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].set, PairSet::Dev);
        assert_eq!(pairs[1].role, PairRole::Stress);
        assert!(!pairs[1].in_scope);
        assert_eq!(pairs[1].known_issues, None);
        assert_eq!(pairs[0].expected_extraction, ExpectedExtraction::Complete);
        assert_eq!(
            pairs[0].expected_file.as_deref(),
            Some("expected/alpha.json")
        );
        assert_eq!(pairs[0].limit_scale_hint, 1.0);
        assert_eq!(pairs[0].old.sha256, "a".repeat(64));
        assert_eq!(pairs[0].new.byte_count, 200);
    }

    #[test]
    fn manifest_parser_rejects_malformed_input() {
        let header = MANIFEST_HEADER.join("\t");
        assert!(
            parse_manifest(&header).is_err(),
            "header alone records no pairs"
        );
        assert!(parse_manifest(&format!("{header}\nshort\trow\n")).is_err());
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "maybe", "complete")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "complete")
                    .replace(&"a".repeat(64), "zz")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "complete"),
                manifest_row("alpha", "holdout", "stress", "true", "incomplete")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "complete").replace('\t', "|")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&manifest_row(
                "alpha", "dev", "standard", "true", "complete"
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "sideways", "standard", "true", "complete")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "huge", "true", "complete")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "sometimes")
            ))
            .is_err()
        );
        assert!(
            parse_manifest(&format!(
                "{header}\n{}\n",
                manifest_row("alpha", "dev", "standard", "true", "complete")
                    .replace("\t1.0\t", "\t0.5\t")
            ))
            .is_err()
        );
    }

    #[test]
    fn expected_documents_enforce_version_and_quote_shapes() {
        let template = r#"{"version":1,"pair":"p","reviewed_on":"2026-08-24","annotation":"partial","changes":[CHANGES]}"#;
        let replacement =
            r#"{"id":"c1","kind":"replacement","old_quote":"ten days","new_quote":"twenty days"}"#;
        let insertion_bad =
            r#"{"id":"c2","kind":"insertion","old_quote":"leftover","new_quote":"added"}"#;
        let insertion_good = r#"{"id":"c2","kind":"insertion","new_quote":"added"}"#;
        let deletion_good = r#"{"id":"c3","kind":"deletion","old_quote":"removed"}"#;
        let with_changes = |changes: &str| template.replace("[CHANGES]", &format!("[{changes}]"));

        assert!(load_expected_document(&with_changes(replacement)).is_ok());
        assert!(load_expected_document(&with_changes(insertion_bad)).is_err());
        assert!(load_expected_document(&with_changes(insertion_good)).is_ok());
        assert!(load_expected_document(&with_changes(deletion_good)).is_ok());
        assert!(
            load_expected_document(
                &with_changes(replacement).replace(r#""version":1"#, r#""version":2"#)
            )
            .is_err()
        );

        let duplicate_ids = r#"{"version":1,"pair":"p","reviewed_on":"x","annotation":"complete","changes":[{"id":"c1","kind":"deletion","old_quote":"a"},{"id":"c1","kind":"deletion","old_quote":"b"}]}"#;
        assert!(load_expected_document(duplicate_ids).is_err());

        let unknown_field =
            r#"{"version":1,"pair":"p","reviewed_on":"x","annotation":"partial","surprise":true}"#;
        assert!(load_expected_document(unknown_field).is_err());
    }

    fn expected_change(
        id: &str,
        kind: ExpectedKind,
        old: Option<&str>,
        new: Option<&str>,
    ) -> ExpectedChange {
        ExpectedChange {
            id: id.to_owned(),
            kind,
            old_quote: old.map(str::to_owned),
            new_quote: new.map(str::to_owned),
            note: String::new(),
        }
    }

    fn actual_change(
        kind: ChangeKind,
        old_text: Option<&str>,
        new_text: Option<&str>,
        old_len: Option<usize>,
        new_len: Option<usize>,
    ) -> ActualChange {
        ActualChange {
            kind,
            old_text: old_text.map(str::to_owned),
            new_text: new_text.map(str::to_owned),
            old_comparable_len: old_len,
            new_comparable_len: new_len,
            resolvable: true,
        }
    }

    #[test]
    fn matching_is_one_to_one_and_tracks_kind_agreement() {
        let expected = vec![
            expected_change(
                "c1",
                ExpectedKind::Replacement,
                Some("ten days"),
                Some("twenty days"),
            ),
            expected_change("c2", ExpectedKind::Deletion, Some("obsolete section"), None),
        ];
        let actuals = vec![
            actual_change(
                ChangeKind::Replacement,
                Some("within ten days of receipt"),
                Some("within twenty days of receipt"),
                Some(26),
                Some(30),
            ),
            actual_change(
                ChangeKind::Insertion,
                Some("before the obsolete section"),
                Some("before"),
                Some(26),
                Some(7),
            ),
        ];
        let partial = compute_quality(Annotation::Partial, &expected, &actuals);
        assert_eq!(partial.expected_changes, 2);
        assert_eq!(partial.reported_changes, 2);
        assert_eq!(partial.recall, Some(1.0));
        assert_eq!(partial.precision, None);
        assert_eq!(partial.kind_accuracy, Some(0.5));
        assert_eq!(partial.unmatched_tiny_changes, 0);
    }

    #[test]
    fn whitespace_collapsing_allows_quotes_across_wrapped_blocks() {
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Replacement,
            Some("annual fee  of fifty dollars"),
            Some("annual fee of sixty dollars"),
        )];
        let actuals = vec![actual_change(
            ChangeKind::Replacement,
            Some("pay an annual\nfee  of fifty dollars to"),
            Some("pay an annual fee of sixty dollars to"),
            Some(36),
            Some(37),
        )];
        let quality = compute_quality(Annotation::Complete, &expected, &actuals);
        assert_eq!(quality.precision, Some(1.0));
        assert_eq!(quality.review_hunks_per_expected_change, Some(1.0));
    }

    #[test]
    fn tiny_unmatched_edits_are_counted_as_false_positive_candidates() {
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Replacement,
            Some("meaningful replaced sentence"),
            Some("other meaningful replaced sentence"),
        )];
        let actuals = vec![
            actual_change(
                ChangeKind::Replacement,
                Some("ab"),
                Some("cd"),
                Some(2),
                Some(2),
            ),
            actual_change(
                ChangeKind::Replacement,
                Some("ef"),
                Some("gh"),
                Some(2),
                Some(2),
            ),
            actual_change(
                ChangeKind::Replacement,
                Some("meaningful replaced sentence"),
                Some("other meaningful replaced sentence"),
                Some(28),
                Some(34),
            ),
        ];
        let quality = compute_quality(Annotation::Complete, &expected, &actuals);
        assert_eq!(quality.recall, Some(1.0));
        assert_eq!(quality.precision, Some(1.0 / 3.0));
        assert_eq!(quality.unmatched_tiny_changes, 2);
        assert_eq!(quality.reported_hunks_per_matched_change, Some(3.0));
        assert_eq!(quality.review_hunks_per_expected_change, Some(3.0));
    }

    #[test]
    fn long_one_sided_spans_are_not_tiny() {
        let change = actual_change(ChangeKind::Insertion, None, Some("ab"), None, Some(2));
        assert!(is_tiny(&change));
        let change = actual_change(
            ChangeKind::Insertion,
            None,
            Some("much longer inserted content"),
            None,
            Some(28),
        );
        assert!(!is_tiny(&change));
    }

    #[test]
    fn unresolvable_spans_never_count_as_suspicious_tiny_edits() {
        // A change whose spans failed text resolution carries no comparable
        // lengths; previously the 0-initialized maximum made such changes
        // look like two-token edits.
        let mut both_unresolved = actual_change(ChangeKind::Replacement, None, None, None, None);
        both_unresolved.resolvable = false;
        // A partially resolved change keeps one short length while still
        // being unresolvable overall.
        let mut partially_resolved =
            actual_change(ChangeKind::Replacement, Some("ab"), None, Some(2), None);
        partially_resolved.resolvable = false;
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Deletion,
            Some("gone text"),
            None,
        )];
        let quality = compute_quality(
            Annotation::Partial,
            &expected,
            &[partially_resolved, both_unresolved],
        );
        assert_eq!(quality.unmatched_tiny_changes, 0);
        assert_eq!(quality.unresolvable_reported_spans, 2);
        assert_eq!(quality.recall, Some(0.0));
    }

    fn record(status: PairRunStatus) -> PairRunReport {
        PairRunReport {
            pair_id: "p".to_owned(),
            set: PairSet::Dev.label(),
            role: PairRole::Standard.label(),
            document_type: "t".to_owned(),
            in_scope: true,
            status,
            provenance_verified: true,
            compared: false,
            extraction_complete: None,
            comparison_complete: None,
            extraction_issues: Vec::new(),
            coverage_old: None,
            coverage_new: None,
            coverage_comparison: None,
            unresolved_regions: None,
            unresolved_old_token_share: None,
            unresolved_new_token_share: None,
            reported_content_changes: None,
            formatting_only_changes: None,
            uncertain_changes: None,
            reported_changes_preview: Vec::new(),
            quality: None,
            quality_skipped_reason: None,
            resource_limit_failure: None,
            candidate_visits: None,
            candidate_visits_required: None,
            candidate_visits_required_exact: None,
            candidate_visits_required_ngram: None,
            candidate_visits_required_short_fallback: None,
            max_candidate_visits: None,
            candidate_visit_pressure: None,
            runtime_ms: 0,
            limit_scale_used: 1.0,
            failure: None,
        }
    }

    #[test]
    fn status_summary_reports_healthy_limit_and_failed_totals_accurately() {
        let reports = vec![
            record(PairRunStatus::Ok),
            record(PairRunStatus::Ok),
            record(PairRunStatus::Limit),
            record(PairRunStatus::Failed),
        ];
        assert!(reports[0].healthy());
        assert!(!reports[2].healthy());
        assert!(!reports[3].healthy());
        assert_eq!(
            summarize_reports(&reports),
            "2/4 revision pairs healthy; 1 stopped at resource limits; 1 failed"
        );
    }

    #[test]
    fn partial_annotations_skip_precision_but_keep_recall() {
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Insertion,
            None,
            Some("new paragraph"),
        )];
        let actuals = vec![
            actual_change(
                ChangeKind::Insertion,
                None,
                Some("a brand new paragraph"),
                None,
                Some(21),
            ),
            actual_change(
                ChangeKind::Deletion,
                Some("removed words"),
                None,
                Some(13),
                None,
            ),
        ];
        let partial = compute_quality(Annotation::Partial, &expected, &actuals);
        assert_eq!(partial.recall, Some(1.0));
        assert_eq!(partial.precision, None);
        assert_eq!(partial.kind_accuracy, Some(1.0));
        assert_eq!(partial.reported_hunks_per_matched_change, Some(2.0));
        let complete = compute_quality(Annotation::Complete, &expected, &actuals);
        assert_eq!(complete.precision, Some(0.5));
        assert_eq!(complete.review_hunks_per_expected_change, Some(2.0));
    }

    #[test]
    fn unresolvable_spans_are_excluded_from_matching_and_counted() {
        let expected = vec![expected_change(
            "c1",
            ExpectedKind::Deletion,
            Some("gone text"),
            None,
        )];
        let mut unresolvable =
            actual_change(ChangeKind::Deletion, Some("gone text"), None, Some(9), None);
        unresolvable.resolvable = false;
        let quality = compute_quality(
            Annotation::Partial,
            &expected,
            std::slice::from_ref(&unresolvable),
        );
        assert_eq!(quality.recall, Some(0.0));
        assert_eq!(quality.unresolvable_reported_spans, 1);
    }

    #[test]
    fn limit_scale_validation_rejects_lowering_and_nonfinite_values() {
        assert_eq!(validate_limit_scale(1.0).unwrap_or_default(), 1.0);
        assert_eq!(validate_limit_scale(16.0).unwrap_or_default(), 16.0);
        assert!(validate_limit_scale(0.999).is_err());
        assert!(validate_limit_scale(f64::NAN).is_err());
        assert!(validate_limit_scale(f64::INFINITY).is_err());
    }

    #[test]
    fn scaled_options_only_raise_budgets() {
        let baseline = PipelineOptions::default();
        let scaled = scaled_pipeline_options(4.0);
        assert!(scaled.max_ngram_token_elements >= baseline.max_ngram_token_elements);
        assert!(scaled.alignment.max_candidate_visits >= baseline.alignment.max_candidate_visits);
        assert!(scaled.alignment.max_dp_cells >= baseline.alignment.max_dp_cells);
        assert!(scaled.diff.max_tokens >= baseline.diff.max_tokens);
        assert!(scaled.diff.max_edit_distance >= baseline.diff.max_edit_distance);
        assert_eq!(
            scaled_pipeline_options(1.0).alignment.max_candidate_visits,
            baseline.alignment.max_candidate_visits
        );
    }

    #[test]
    fn collapse_whitespace_normalizes_line_breaks_and_runs() {
        assert_eq!(collapse_whitespace("a\n b   c\t d"), "a b c d");
        assert_eq!(collapse_whitespace("   "), "");
    }

    fn glyph_document(text: &str) -> Document<Glyph> {
        let glyphs = text
            .chars()
            .enumerate()
            .filter(|(_, character)| *character != ' ')
            .map(|(index, character)| Glyph {
                id: GlyphId(index as u64 + 1),
                text: DecodedText::Mapped(character.to_string()),
                raw_code: character.to_string().into_bytes(),
                page: PageId(0),
                bbox: Rect {
                    min: Vec2 {
                        x: index as f64 * 6.0,
                        y: 100.0,
                    },
                    max: Vec2 {
                        x: index as f64 * 6.0 + 5.0,
                        y: 110.0,
                    },
                },
                baseline: Vec2 {
                    x: index as f64 * 6.0,
                    y: 100.0,
                },
                direction: Vec2 { x: 1.0, y: 0.0 },
                font_id: FontId(1),
                font_size: 10.0,
                render_order: u32::try_from(index).expect("fixture glyph index fits in u32"),
                render_mode: TextRenderMode::Fill,
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: 1,
                        generation: 0,
                    },
                    operator_index: u32::try_from(index).expect("fixture glyph index fits in u32"),
                },
            })
            .collect();
        Document::new(glyphs)
    }

    #[test]
    fn compare_outcomes_with_metrics_records_candidate_visits_on_success() {
        let old =
            ExtractionOutcome::complete(glyph_document("Stable old paragraph remains visible"));
        let new =
            ExtractionOutcome::complete(glyph_document("Stable new paragraph remains visible"));

        let ComparisonWithMetrics {
            outcome,
            metrics,
            pressure,
        } = compare_outcomes_with_metrics(old, new, PipelineOptions::default())
            .expect("comparison succeeds");

        assert!(!outcome.comparison.changes.is_empty());
        let visits = metrics.candidate_visits.expect("candidate visits recorded");
        assert!(visits > 0, "non-anchor old blocks must be charged");
        assert_eq!(
            metrics.candidate_visits_required,
            Some(visits),
            "attempted charge must equal the required sum on success"
        );
        let (exact, ngram, short_fallback) = (
            metrics
                .candidate_visits_required_exact
                .expect("inverted index reports a breakdown"),
            metrics
                .candidate_visits_required_ngram
                .expect("inverted index reports a breakdown"),
            metrics
                .candidate_visits_required_short_fallback
                .expect("inverted index reports a breakdown"),
        );
        assert_eq!(
            exact + ngram + short_fallback,
            visits,
            "required components must sum to the required total"
        );
        assert_eq!(
            metrics.max_candidate_visits,
            Some(AlignmentOptions::default().max_candidate_visits)
        );
        let pressure = pressure.expect("pressure recorded for complete extraction");
        assert_eq!(
            pressure.max_candidate_visits,
            AlignmentOptions::default().max_candidate_visits
        );
    }

    #[test]
    fn compare_outcomes_with_metrics_keeps_attempted_charge_on_candidate_limit() {
        let old =
            ExtractionOutcome::complete(glyph_document("Stable old paragraph remains visible"));
        let new =
            ExtractionOutcome::complete(glyph_document("Stable new paragraph remains visible"));

        let ComparisonWithMetrics {
            outcome: _,
            metrics,
            pressure: _,
        } = compare_outcomes_with_metrics(old.clone(), new.clone(), PipelineOptions::default())
            .expect("baseline comparison succeeds");
        let charge = metrics.candidate_visits.expect("baseline charge recorded");
        assert!(charge > 1, "fixture must charge at least two visits");

        let options = PipelineOptions {
            alignment: AlignmentOptions {
                max_candidate_visits: charge - 1,
                ..AlignmentOptions::default()
            },
            ..PipelineOptions::default()
        };
        let error = compare_outcomes_with_metrics(old, new, options)
            .expect_err("candidate limit must fail");
        match error {
            RevisionRunError::Limit {
                message,
                metrics,
                pressure,
            } => {
                assert!(message.contains("alignment candidate visits"));
                assert_eq!(metrics.candidate_visits, Some(charge));
                assert_eq!(
                    metrics.candidate_visits_required,
                    Some(charge),
                    "the full required sum completes when no later estimate errors"
                );
                let (exact, ngram, short_fallback) = (
                    metrics
                        .candidate_visits_required_exact
                        .expect("inverted index reports a breakdown"),
                    metrics
                        .candidate_visits_required_ngram
                        .expect("inverted index reports a breakdown"),
                    metrics
                        .candidate_visits_required_short_fallback
                        .expect("inverted index reports a breakdown"),
                );
                assert_eq!(
                    exact + ngram + short_fallback,
                    charge,
                    "required components must sum to the required total"
                );
                assert_eq!(metrics.max_candidate_visits, Some(charge - 1));
                let pressure = pressure.expect("pressure recorded for complete extraction");
                assert_eq!(pressure.max_candidate_visits, charge - 1);
            }
            other => panic!("expected Limit, got {other:?}"),
        }
    }

    #[test]
    fn compare_outcomes_with_metrics_returns_none_for_pre_alignment_limit() {
        let old =
            ExtractionOutcome::complete(glyph_document("Stable old paragraph remains visible"));
        let new =
            ExtractionOutcome::complete(glyph_document("Stable new paragraph remains visible"));
        let options = PipelineOptions {
            max_ngram_token_elements: 1,
            ..PipelineOptions::default()
        };

        let error = compare_outcomes_with_metrics(old, new, options)
            .expect_err("ngram budget must fail before alignment");
        match error {
            RevisionRunError::Limit {
                metrics, pressure, ..
            } => {
                assert_eq!(metrics.candidate_visits, None);
                assert_eq!(metrics.candidate_visits_required, None);
                assert_eq!(metrics.candidate_visits_required_exact, None);
                assert_eq!(metrics.candidate_visits_required_ngram, None);
                assert_eq!(metrics.candidate_visits_required_short_fallback, None);
                assert_eq!(metrics.max_candidate_visits, None);
                assert_eq!(pressure, None);
            }
            other => panic!("expected Limit, got {other:?}"),
        }
    }

    #[test]
    fn compare_outcomes_with_metrics_skips_pressure_for_incomplete_extraction() {
        let incomplete = ExtractionOutcome::new(
            glyph_document("Stable old paragraph remains visible"),
            vec![
                ExtractionIssue::new(
                    ExtractionIssueKind::Unresolved,
                    ExtractionScope::Document,
                    "document evidence is incomplete",
                )
                .expect("valid issue"),
            ],
        )
        .expect("valid outcome");
        let complete =
            ExtractionOutcome::complete(glyph_document("Stable new paragraph remains visible"));

        let ComparisonWithMetrics {
            outcome: _,
            metrics,
            pressure,
        } = compare_outcomes_with_metrics(incomplete, complete, PipelineOptions::default())
            .expect("incomplete comparison is not an error");

        assert_eq!(metrics.candidate_visits, None);
        assert_eq!(metrics.candidate_visits_required, None);
        assert_eq!(metrics.candidate_visits_required_exact, None);
        assert_eq!(metrics.candidate_visits_required_ngram, None);
        assert_eq!(metrics.candidate_visits_required_short_fallback, None);
        assert_eq!(pressure, None);
    }

    #[test]
    fn pair_report_json_includes_candidate_visit_fields() {
        let mut report = record(PairRunStatus::Ok);
        report.candidate_visits = Some(42);
        report.candidate_visits_required = Some(84);
        report.candidate_visits_required_exact = Some(20);
        report.candidate_visits_required_ngram = Some(40);
        report.candidate_visits_required_short_fallback = Some(24);
        report.max_candidate_visits = Some(1_000_000);
        report.candidate_visit_pressure = Some(CandidateVisitPressure {
            estimated_visits_p50: 1,
            estimated_visits_p95: 2,
            estimated_visits_max: 3,
            estimated_visits_upper_bound_total: 9,
            max_candidate_visits: 8,
            estimated_visits_upper_bound_exceeds_limit: true,
            ngram_posting_visits_total: 9,
            dominant_ngram_visits: 9,
            dominant_ngram_df: 3,
            shared_ngram_count: 1,
            top_10_ngram_visits: 9,
            ngrams_for_50_percent_visits: 1,
            ngrams_for_90_percent_visits: 1,
            shared_ngram_df_p50: 3,
            shared_ngram_df_p95: 3,
            shared_ngram_df_max: 3,
        });

        let json = serde_json::to_value(&report).expect("report serializes");
        assert_eq!(json["candidate_visits"], 42);
        assert_eq!(json["candidate_visits_required"], 84);
        assert_eq!(json["candidate_visits_required_exact"], 20);
        assert_eq!(json["candidate_visits_required_ngram"], 40);
        assert_eq!(json["candidate_visits_required_short_fallback"], 24);
        assert_eq!(json["max_candidate_visits"], 1_000_000);
        assert_eq!(
            json["candidate_visit_pressure"]["estimated_visits_upper_bound_total"],
            9
        );
        assert_eq!(json["candidate_visit_pressure"]["dominant_ngram_df"], 3);
        assert_eq!(json["candidate_visit_pressure"]["shared_ngram_count"], 1);
        assert_eq!(
            json["candidate_visit_pressure"]["ngrams_for_90_percent_visits"],
            1
        );
    }

    #[test]
    fn validate_visit_metrics_accepts_complete_absent_and_unavailable_required() {
        let complete = VisitMetrics {
            candidate_visits: Some(42),
            candidate_visits_required: Some(84),
            candidate_visits_required_exact: Some(20),
            candidate_visits_required_ngram: Some(40),
            candidate_visits_required_short_fallback: Some(24),
            max_candidate_visits: Some(1_000_000),
        };
        assert_eq!(validate_visit_metrics(complete), Ok(complete));
        let generic = VisitMetrics {
            candidate_visits_required_exact: None,
            candidate_visits_required_ngram: None,
            candidate_visits_required_short_fallback: None,
            ..complete
        };
        assert_eq!(validate_visit_metrics(generic), Ok(generic));
        let unavailable = VisitMetrics {
            candidate_visits_required: None,
            candidate_visits_required_exact: None,
            candidate_visits_required_ngram: None,
            candidate_visits_required_short_fallback: None,
            ..complete
        };
        assert_eq!(validate_visit_metrics(unavailable), Ok(unavailable));
        assert_eq!(
            validate_visit_metrics(VisitMetrics::default()),
            Ok(VisitMetrics::default())
        );
    }

    #[test]
    fn validate_visit_metrics_rejects_every_partial_direction() {
        for metrics in [
            VisitMetrics {
                candidate_visits: Some(42),
                ..VisitMetrics::default()
            },
            VisitMetrics {
                candidate_visits_required: Some(84),
                ..VisitMetrics::default()
            },
            VisitMetrics {
                max_candidate_visits: Some(1_000_000),
                ..VisitMetrics::default()
            },
            VisitMetrics {
                candidate_visits: Some(42),
                candidate_visits_required: Some(84),
                ..VisitMetrics::default()
            },
            VisitMetrics {
                candidate_visits_required: Some(84),
                max_candidate_visits: Some(1_000_000),
                ..VisitMetrics::default()
            },
            VisitMetrics {
                candidate_visits: Some(42),
                candidate_visits_required: Some(84),
                candidate_visits_required_exact: Some(20),
                candidate_visits_required_ngram: Some(40),
                candidate_visits_required_short_fallback: None,
                max_candidate_visits: Some(1_000_000),
            },
            VisitMetrics {
                candidate_visits: Some(42),
                candidate_visits_required: None,
                candidate_visits_required_exact: Some(20),
                candidate_visits_required_ngram: Some(40),
                candidate_visits_required_short_fallback: Some(24),
                max_candidate_visits: Some(1_000_000),
            },
        ] {
            let error = validate_visit_metrics(metrics)
                .expect_err("partial metrics must be a contract violation");
            assert!(error.contains("alignment metrics contract violation"));
        }
    }

    #[test]
    fn validate_visit_metrics_rejects_component_sum_mismatch() {
        let metrics = VisitMetrics {
            candidate_visits: Some(42),
            candidate_visits_required: Some(84),
            candidate_visits_required_exact: Some(20),
            candidate_visits_required_ngram: Some(40),
            candidate_visits_required_short_fallback: Some(23),
            max_candidate_visits: Some(1_000_000),
        };
        let error = validate_visit_metrics(metrics)
            .expect_err("component sum mismatch must be a contract violation");
        assert!(error.contains("do not sum to"));
    }

    #[test]
    fn resolve_pressure_rejects_alignment_without_a_pressure_attempt() {
        let error = resolve_pressure(Some(42), None)
            .expect_err("alignment reached without a pressure attempt must fail");
        match error {
            RevisionRunError::Other(stage, message) => {
                assert_eq!(stage, "candidate visit pressure contract violation");
                assert!(message.contains("without a pressure attempt"));
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn publish_new_file_refuses_to_overwrite_existing_file_and_handles_temp_collisions_deterministically()
     {
        let mut base_path = std::env::temp_dir();
        let unique_id = format!(
            "pdfbench-publish-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        base_path.push(&unique_id);
        fs::create_dir_all(&base_path).expect("create test dir");

        let dest = base_path.join("output.json");
        let bytes = b"hello\n";

        // Pre-create the exact first candidate temp file for a local counter
        let local_counter = AtomicU64::new(42);
        let pid = std::process::id();
        let collision_temp = base_path.join(format!(".pdfbench-artifact-{pid}-42"));
        fs::write(&collision_temp, b"pre-existing foreign temp file")
            .expect("write collision temp");

        // Success on new file despite exact pre-existing temp file
        publish_new_file_with_counter(&dest, bytes, &local_counter).expect("publish succeeds");
        assert_eq!(fs::read(&dest).expect("read dest"), bytes);

        // Pre-existing collision temp file was not modified or deleted
        assert_eq!(
            fs::read(&collision_temp).expect("read collision temp"),
            b"pre-existing foreign temp file"
        );
        let _ = fs::remove_file(&collision_temp);

        // Owned temp file (.pdfbench-artifact-{pid}-43) was cleaned up after publish
        let entries = fs::read_dir(&base_path)
            .expect("read dir")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec!["output.json"]);

        // Refuses to overwrite existing file
        let error = publish_new_file(&dest, b"overwrite\n").expect_err("must refuse overwrite");
        assert!(
            error
                .to_string()
                .contains("destination path already exists")
        );
        assert_eq!(fs::read(&dest).expect("dest unchanged"), bytes);

        // Refuses non-existent parent directory
        let invalid_parent = base_path.join("nonexistent").join("output.json");
        let error = publish_new_file(&invalid_parent, bytes).expect_err("must fail missing dir");
        assert!(
            error
                .to_string()
                .contains("destination directory does not exist")
        );

        let _ = fs::remove_dir_all(&base_path);
    }

    #[test]
    fn publish_new_file_handles_long_destination_names_without_name_max_overflow() {
        let mut base_path = std::env::temp_dir();
        let unique_id = format!(
            "pdfbench-long-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        base_path.push(&unique_id);
        fs::create_dir_all(&base_path).expect("create test dir");

        // Long destination name (240 chars)
        let long_name = format!("{}.json", "a".repeat(235));
        let dest = base_path.join(long_name);
        let bytes = b"long destination test bytes\n";

        publish_new_file(&dest, bytes).expect("publish with long name succeeds");
        assert_eq!(fs::read(&dest).expect("read long dest"), bytes);

        let _ = fs::remove_dir_all(&base_path);
    }

    #[test]
    fn publish_new_file_exhaustion_reports_truthful_publication_error() {
        let mut base_path = std::env::temp_dir();
        let unique_id = format!(
            "pdfbench-exhaust-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        base_path.push(&unique_id);
        fs::create_dir_all(&base_path).expect("create test dir");

        let pid = std::process::id();
        let local_counter = AtomicU64::new(1000);

        // Pre-create all 64 candidate temp files
        for i in 0..MAX_TEMP_CREATE_ATTEMPTS {
            let seq = 1000 + i as u64;
            let path = base_path.join(format!(".pdfbench-artifact-{pid}-{seq}"));
            fs::write(&path, b"existing").expect("write temp");
        }

        let dest = base_path.join("output.json");
        let error = publish_new_file_with_counter(&dest, b"bytes", &local_counter)
            .expect_err("must exhaust attempts");
        assert!(error.to_string().contains("exhausted 64 attempts"));
        assert!(!dest.exists(), "destination must not be created");

        let _ = fs::remove_dir_all(&base_path);
    }

    #[test]
    fn normalize_output_destination_resolves_aliases_and_rejects_missing_parents() {
        let mut base_path = std::env::temp_dir();
        let unique_id = format!(
            "pdfbench-norm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        base_path.push(&unique_id);
        fs::create_dir_all(base_path.join("real_dir")).expect("create real dir");

        let file_direct = base_path.join("real_dir").join("report.json");
        let file_relative = base_path.join("real_dir").join(".").join("report.json");
        let norm_direct = normalize_output_destination(&file_direct).expect("normalize direct");
        let norm_relative =
            normalize_output_destination(&file_relative).expect("normalize relative");
        assert_eq!(norm_direct, norm_relative);

        // Symlink parent alias if symlink creation succeeds
        let symlink_dir = base_path.join("symlink_dir");
        #[cfg(unix)]
        if std::os::unix::fs::symlink(base_path.join("real_dir"), &symlink_dir).is_ok() {
            let file_symlink = symlink_dir.join("report.json");
            let norm_symlink =
                normalize_output_destination(&file_symlink).expect("normalize symlink");
            assert_eq!(norm_direct, norm_symlink);
        }

        // Missing parent directory is rejected
        let missing = base_path.join("missing_parent").join("report.json");
        let error = normalize_output_destination(&missing).expect_err("must reject missing parent");
        assert!(
            error
                .to_string()
                .contains("destination directory does not exist")
        );

        let _ = fs::remove_dir_all(&base_path);
    }

    #[test]
    fn summary_json_exact_key_set_and_nested_schema_equality() {
        let reports = vec![
            PairRunReport {
                pair_id: "ok-pair".to_owned(),
                set: "dev",
                role: "standard",
                document_type: "single-column".to_owned(),
                in_scope: true,
                status: PairRunStatus::Ok,
                provenance_verified: true,
                compared: true,
                extraction_complete: Some(true),
                comparison_complete: Some(false),
                extraction_issues: Vec::new(),
                coverage_old: Some(0.5),
                coverage_new: Some(0.6),
                coverage_comparison: Some(0.5),
                unresolved_regions: Some(0),
                unresolved_old_token_share: Some(0.0),
                unresolved_new_token_share: Some(0.0),
                reported_content_changes: Some(3),
                formatting_only_changes: Some(0),
                uncertain_changes: Some(0),
                reported_changes_preview: Vec::new(),
                quality: Some(QualityMetrics {
                    annotation: Annotation::Partial,
                    expected_changes: 2,
                    reported_changes: 3,
                    recall: Some(1.0),
                    precision: None,
                    kind_accuracy: Some(1.0),
                    reported_hunks_per_matched_change: Some(1.5),
                    review_hunks_per_expected_change: None,
                    unmatched_tiny_changes: 0,
                    unresolvable_reported_spans: 0,
                }),
                quality_skipped_reason: None,
                resource_limit_failure: None,
                candidate_visits: None,
                candidate_visits_required: None,
                candidate_visits_required_exact: None,
                candidate_visits_required_ngram: None,
                candidate_visits_required_short_fallback: None,
                max_candidate_visits: None,
                candidate_visit_pressure: None,
                runtime_ms: 50,
                limit_scale_used: 1.0,
                failure: None,
            },
            PairRunReport {
                pair_id: "limit-pair".to_owned(),
                set: "dev",
                role: "standard",
                document_type: "single-column".to_owned(),
                in_scope: true,
                status: PairRunStatus::Limit,
                provenance_verified: true,
                compared: true,
                extraction_complete: Some(true),
                comparison_complete: None,
                extraction_issues: Vec::new(),
                coverage_old: None,
                coverage_new: None,
                coverage_comparison: None,
                unresolved_regions: None,
                unresolved_old_token_share: None,
                unresolved_new_token_share: None,
                reported_content_changes: None,
                formatting_only_changes: None,
                uncertain_changes: None,
                reported_changes_preview: Vec::new(),
                quality: None,
                quality_skipped_reason: Some(QUALITY_SKIP_RESOURCE_LIMIT.to_owned()),
                resource_limit_failure: Some(
                    "alignment candidate visit budget exceeded".to_owned(),
                ),
                candidate_visits: None,
                candidate_visits_required: None,
                candidate_visits_required_exact: None,
                candidate_visits_required_ngram: None,
                candidate_visits_required_short_fallback: None,
                max_candidate_visits: None,
                candidate_visit_pressure: None,
                runtime_ms: 100,
                limit_scale_used: 1.0,
                failure: None,
            },
            PairRunReport {
                pair_id: "fail-pair".to_owned(),
                set: "holdout",
                role: "standard",
                document_type: "single-column".to_owned(),
                in_scope: true,
                status: PairRunStatus::Failed,
                provenance_verified: true,
                compared: true,
                extraction_complete: Some(false),
                comparison_complete: None,
                extraction_issues: vec![IssueLine {
                    side: "old",
                    kind: "unsupported",
                    scope: "document".to_owned(),
                    description: "unsupported content stream operator".to_owned(),
                }],
                coverage_old: None,
                coverage_new: None,
                coverage_comparison: None,
                unresolved_regions: None,
                unresolved_old_token_share: None,
                unresolved_new_token_share: None,
                reported_content_changes: None,
                formatting_only_changes: None,
                uncertain_changes: None,
                reported_changes_preview: Vec::new(),
                quality: None,
                quality_skipped_reason: Some(QUALITY_SKIP_INCOMPLETE_EXTRACTION.to_owned()),
                resource_limit_failure: None,
                candidate_visits: None,
                candidate_visits_required: None,
                candidate_visits_required_exact: None,
                candidate_visits_required_ngram: None,
                candidate_visits_required_short_fallback: None,
                max_candidate_visits: None,
                candidate_visit_pressure: None,
                runtime_ms: 20,
                limit_scale_used: 1.0,
                failure: Some("/path/to/doc.pdf: extraction failed".to_owned()),
            },
        ];

        let summary = RevisionSummaryReport::from_reports(&reports);
        let value = serde_json::to_value(&summary).expect("serialize value");

        // Top-level schema key-set equality
        let top_keys = value
            .as_object()
            .expect("top object")
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let expected_top_keys = HashSet::from(["schema_version".to_owned(), "records".to_owned()]);
        assert_eq!(top_keys, expected_top_keys);

        let records = value["records"].as_array().expect("records array");
        assert_eq!(records.len(), 3);

        // Record key-set equality
        let expected_record_keys = HashSet::from([
            "pair_id".to_owned(),
            "set".to_owned(),
            "role".to_owned(),
            "in_scope".to_owned(),
            "status".to_owned(),
            "provenance_verified".to_owned(),
            "compared".to_owned(),
            "extraction_complete".to_owned(),
            "comparison_complete".to_owned(),
            "limit_scale_used".to_owned(),
            "resource_limit_failure".to_owned(),
            "coverage_old".to_owned(),
            "coverage_new".to_owned(),
            "coverage_comparison".to_owned(),
            "unresolved_regions".to_owned(),
            "unresolved_old_token_share".to_owned(),
            "unresolved_new_token_share".to_owned(),
            "reported_content_changes".to_owned(),
            "reported_formatting_changes".to_owned(),
            "reported_uncertain_changes".to_owned(),
            "quality".to_owned(),
            "quality_skipped_reason".to_owned(),
        ]);

        for rec in records {
            let keys = rec
                .as_object()
                .expect("record object")
                .keys()
                .cloned()
                .collect::<HashSet<_>>();
            assert_eq!(keys, expected_record_keys);
            assert!(!keys.contains("failure"), "failure must be excluded");
            assert!(!keys.contains("runtime_ms"), "runtime_ms must be excluded");
        }

        // Quality key-set equality
        let expected_quality_keys = HashSet::from([
            "annotation".to_owned(),
            "expected_changes".to_owned(),
            "reported_changes".to_owned(),
            "recall".to_owned(),
            "precision".to_owned(),
            "kind_accuracy".to_owned(),
            "reported_hunks_per_matched_change".to_owned(),
            "review_hunks_per_expected_change".to_owned(),
            "unmatched_tiny_changes".to_owned(),
            "unresolvable_reported_spans".to_owned(),
        ]);
        let quality_keys = records[0]["quality"]
            .as_object()
            .expect("quality object")
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        assert_eq!(quality_keys, expected_quality_keys);

        // Check truthfulness of values
        let ok_rec = &records[0];
        assert_eq!(ok_rec["pair_id"], "ok-pair");
        assert_eq!(ok_rec["status"], "ok");
        assert_eq!(ok_rec["reported_formatting_changes"], 0);
        assert_eq!(ok_rec["reported_uncertain_changes"], 0);
        assert_eq!(ok_rec["quality"]["unmatched_tiny_changes"], 0);
        assert_eq!(ok_rec["quality"]["precision"], serde_json::Value::Null);
        assert_eq!(ok_rec["quality"]["recall"], 1.0);
        assert_eq!(ok_rec["resource_limit_failure"], serde_json::Value::Null);

        let limit_rec = &records[1];
        assert_eq!(limit_rec["pair_id"], "limit-pair");
        assert_eq!(limit_rec["status"], "limit");
        assert_eq!(limit_rec["coverage_old"], serde_json::Value::Null);
        assert_eq!(limit_rec["quality"], serde_json::Value::Null);
        assert_eq!(
            limit_rec["resource_limit_failure"],
            "alignment candidate visit budget exceeded"
        );
        assert_eq!(
            limit_rec["reported_uncertain_changes"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn full_json_schema_regression_does_not_gain_uncertain_changes_field() {
        let report = PairRunReport {
            pair_id: "regression-check".to_owned(),
            set: "dev",
            role: "standard",
            document_type: "single-column".to_owned(),
            in_scope: true,
            status: PairRunStatus::Ok,
            provenance_verified: true,
            compared: true,
            extraction_complete: Some(true),
            comparison_complete: Some(true),
            extraction_issues: Vec::new(),
            coverage_old: Some(1.0),
            coverage_new: Some(1.0),
            coverage_comparison: Some(1.0),
            unresolved_regions: Some(0),
            unresolved_old_token_share: Some(0.0),
            unresolved_new_token_share: Some(0.0),
            reported_content_changes: Some(0),
            formatting_only_changes: Some(0),
            uncertain_changes: Some(0),
            reported_changes_preview: Vec::new(),
            quality: None,
            quality_skipped_reason: None,
            resource_limit_failure: None,
            candidate_visits: None,
            candidate_visits_required: None,
            candidate_visits_required_exact: None,
            candidate_visits_required_ngram: None,
            candidate_visits_required_short_fallback: None,
            max_candidate_visits: None,
            candidate_visit_pressure: None,
            runtime_ms: 10,
            limit_scale_used: 1.0,
            failure: None,
        };

        let full_bytes = serde_json::to_vec_pretty(&[report]).expect("serialize full");
        let full_val: serde_json::Value = serde_json::from_slice(&full_bytes).expect("parse full");
        let full_obj = full_val[0].as_object().expect("first report");

        assert!(
            !full_obj.contains_key("uncertain_changes"),
            "full JSON schema must not gain uncertain_changes"
        );
    }

    #[test]
    fn summary_json_is_deterministic_and_excludes_forbidden_keys_and_values() {
        let base = PairRunReport {
            pair_id: "deterministic-pair".to_owned(),
            set: "dev",
            role: "standard",
            document_type: "single-column".to_owned(),
            in_scope: true,
            status: PairRunStatus::Ok,
            provenance_verified: true,
            compared: true,
            extraction_complete: Some(true),
            comparison_complete: Some(false),
            extraction_issues: vec![IssueLine {
                side: "old",
                kind: "unsupported",
                scope: "page".to_owned(),
                description: "Injected /tmp/path/document.pdf extraction issue".to_owned(),
            }],
            coverage_old: Some(0.4),
            coverage_new: Some(0.5),
            coverage_comparison: Some(0.4),
            unresolved_regions: Some(1),
            unresolved_old_token_share: Some(0.02),
            unresolved_new_token_share: Some(0.03),
            reported_content_changes: Some(5),
            formatting_only_changes: Some(0),
            uncertain_changes: Some(0),
            reported_changes_preview: vec![ReportedChangeText {
                kind: "replacement",
                old_text: Some("Sensitive text /home/user/cache/doc.pdf".to_owned()),
                new_text: Some("Sensitive text replacement".to_owned()),
            }],
            quality: None,
            quality_skipped_reason: None,
            resource_limit_failure: None,
            candidate_visits: Some(10),
            candidate_visits_required: Some(20),
            candidate_visits_required_exact: Some(5),
            candidate_visits_required_ngram: Some(10),
            candidate_visits_required_short_fallback: Some(5),
            max_candidate_visits: Some(500),
            candidate_visit_pressure: None,
            runtime_ms: 100,
            limit_scale_used: 1.0,
            failure: Some("Failure with /home/hayato/cache/old.pdf".to_owned()),
        };

        let mut report_a = base.clone();
        report_a.runtime_ms = 42;
        report_a.candidate_visits = Some(999);

        let mut report_b = base;
        report_b.runtime_ms = 888888;
        report_b.candidate_visits = Some(12345);

        let summary_a = RevisionSummaryReport::from_reports(&[report_a]);
        let summary_b = RevisionSummaryReport::from_reports(&[report_b]);

        let bytes_a = serde_json::to_vec_pretty(&summary_a).expect("serialize A");
        let bytes_b = serde_json::to_vec_pretty(&summary_b).expect("serialize B");

        assert_eq!(
            bytes_a, bytes_b,
            "summaries must be byte-for-byte deterministic"
        );

        let json_str = String::from_utf8(bytes_a).expect("utf8 string");
        let forbidden = [
            "/home/",
            "/tmp/",
            "Sensitive text",
            "runtime_ms",
            "\"failure\":",
            "candidate_visits",
            "extraction_issues",
            "candidate_visit_pressure",
            "reported_changes_preview",
        ];
        for term in forbidden {
            assert!(
                !json_str.contains(term),
                "summary JSON must not contain forbidden term {term:?}"
            );
        }
    }
}
