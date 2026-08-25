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
    sync::Arc,
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

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
        reported_changes_preview: Vec::new(),
        quality: None,
        quality_skipped_reason: None,
        resource_limit_failure: None,
        candidate_visits: None,
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
            Ok((outcome, candidate_visits, max_candidate_visits, pressure)) => {
                record.candidate_visits = candidate_visits;
                record.max_candidate_visits = max_candidate_visits;
                record.candidate_visit_pressure = pressure;
                outcome
            }
            Err(RevisionRunError::Read(reason)) => {
                record.failure = Some(reason);
                return finish(record, started);
            }
            Err(RevisionRunError::Limit {
                message,
                candidate_visits,
                max_candidate_visits,
                pressure,
            }) => {
                record.resource_limit_failure = Some(message);
                record.candidate_visits = candidate_visits;
                record.max_candidate_visits = max_candidate_visits;
                record.candidate_visit_pressure = pressure.map(|pressure| *pressure);
                record.quality_skipped_reason =
                    Some("comparison stopped at a resource limit".to_owned());
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
            record.quality_skipped_reason =
                Some("extraction was incomplete so reported diffs are suppressed".to_owned());
        }
        (None, _) if pair.expected_file.is_some() => {}
        (None, _) => {
            record.quality_skipped_reason =
                Some("no expected annotations are recorded for this pair".to_owned());
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
        candidate_visits: Option<usize>,
        max_candidate_visits: Option<usize>,
        pressure: Option<Box<CandidateVisitPressure>>,
    },
    Other(&'static str, String),
}

type RevisionOutcome = pdfdelta_core::pipeline::ComparisonOutcome;

/// Comparison outcome plus the alignment charge and supplementary pressure.
type ComparisonWithMetrics = (
    RevisionOutcome,
    Option<usize>,
    Option<usize>,
    Option<CandidateVisitPressure>,
);

/// Validates one alignment metrics pair: both fields set or both absent
/// pass through; a partial pair is a contract violation and is reported as
/// an error rather than silently treated as a measurement.
fn validate_visit_pair(
    candidate_visits: Option<usize>,
    max_candidate_visits: Option<usize>,
) -> std::result::Result<(Option<usize>, Option<usize>), String> {
    match (candidate_visits, max_candidate_visits) {
        (Some(_), Some(_)) | (None, None) => Ok((candidate_visits, max_candidate_visits)),
        _ => Err(format!(
            "alignment metrics contract violation: candidate_visits={candidate_visits:?}, max_candidate_visits={max_candidate_visits:?}"
        )),
    }
}

/// Extracts the alignment candidate visit charge from the diagnostics.
/// Returns `(None, None)` when alignment was never reached or recorded no
/// charge; a partial pair is a contract violation and is reported as an
/// error.
fn alignment_visit_metrics(
    diagnostics: &PipelineDiagnostics,
) -> std::result::Result<(Option<usize>, Option<usize>), String> {
    let Some(record) = diagnostics
        .records()
        .iter()
        .find(|record| record.phase == PipelinePhase::Alignment)
    else {
        return Ok((None, None));
    };
    validate_visit_pair(
        record.metrics.candidate_visits,
        record.metrics.max_candidate_visits,
    )
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

/// Compares two extracted outcomes and returns the alignment candidate
/// visit charge and the supplementary all-old-block pressure alongside the
/// outcome; a candidate limit stop carries the attempted charge and the
/// pressure in the error. A metrics contract violation takes priority over
/// the core result. Once alignment is reached, a pressure measurement
/// failure is a benchmark instrumentation failure, not missing
/// supplementary information: it takes priority over the core Ok/Limit
/// result and surfaces as `Failed`.
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
    let (candidate_visits, max_candidate_visits) =
        alignment_visit_metrics(&diagnostics).map_err(|message| {
            RevisionRunError::Other("alignment metrics contract violation", message)
        })?;
    let pressure = resolve_pressure(candidate_visits, pressure_result)?;
    let outcome = result.map_err(|error| match error {
        pdfdelta_core::Error::LimitExceeded { .. } => RevisionRunError::Limit {
            message: error.to_string(),
            candidate_visits,
            max_candidate_visits,
            pressure: pressure.map(Box::new),
        },
        other => RevisionRunError::Other("comparison failed", other.to_string()),
    })?;
    Ok((outcome, candidate_visits, max_candidate_visits, pressure))
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

pub fn write_reports_json(path: &Path, reports: &[PairRunReport]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            BenchError::InvalidInput(format!(
                "cannot create revision benchmark JSON output {}: {error}",
                path.display()
            ))
        })?;
    let bytes = serde_json::to_vec_pretty(reports).map_err(|error| {
        BenchError::InvalidInput(format!("cannot serialize revision JSON report: {error}"))
    })?;
    file.write_all(&bytes).map_err(|error| {
        BenchError::InvalidInput(format!("cannot write revision JSON report: {error}"))
    })
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
            reported_changes_preview: Vec::new(),
            quality: None,
            quality_skipped_reason: None,
            resource_limit_failure: None,
            candidate_visits: None,
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

        let (outcome, candidate_visits, max_candidate_visits, pressure) =
            compare_outcomes_with_metrics(old, new, PipelineOptions::default())
                .expect("comparison succeeds");

        assert!(!outcome.comparison.changes.is_empty());
        let visits = candidate_visits.expect("candidate visits recorded");
        assert!(visits > 0, "non-anchor old blocks must be charged");
        assert_eq!(
            max_candidate_visits,
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

        let (_, charge, _, _) =
            compare_outcomes_with_metrics(old.clone(), new.clone(), PipelineOptions::default())
                .expect("baseline comparison succeeds");
        let charge = charge.expect("baseline charge recorded");
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
                candidate_visits,
                max_candidate_visits,
                pressure,
            } => {
                assert!(message.contains("alignment candidate visits"));
                assert_eq!(candidate_visits, Some(charge));
                assert_eq!(max_candidate_visits, Some(charge - 1));
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
                candidate_visits,
                max_candidate_visits,
                pressure,
                ..
            } => {
                assert_eq!(candidate_visits, None);
                assert_eq!(max_candidate_visits, None);
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

        let (_, candidate_visits, _, pressure) =
            compare_outcomes_with_metrics(incomplete, complete, PipelineOptions::default())
                .expect("incomplete comparison is not an error");

        assert_eq!(candidate_visits, None);
        assert_eq!(pressure, None);
    }

    #[test]
    fn pair_report_json_includes_candidate_visit_fields() {
        let mut report = record(PairRunStatus::Ok);
        report.candidate_visits = Some(42);
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
        });

        let json = serde_json::to_value(&report).expect("report serializes");
        assert_eq!(json["candidate_visits"], 42);
        assert_eq!(json["max_candidate_visits"], 1_000_000);
        assert_eq!(
            json["candidate_visit_pressure"]["estimated_visits_upper_bound_total"],
            9
        );
        assert_eq!(json["candidate_visit_pressure"]["dominant_ngram_df"], 3);
    }

    #[test]
    fn validate_visit_pair_accepts_complete_and_absent_pairs() {
        assert_eq!(
            validate_visit_pair(Some(42), Some(1_000_000)),
            Ok((Some(42), Some(1_000_000)))
        );
        assert_eq!(validate_visit_pair(None, None), Ok((None, None)));
    }

    #[test]
    fn validate_visit_pair_rejects_both_partial_directions() {
        for (candidate_visits, max_candidate_visits) in [(Some(42), None), (None, Some(1_000_000))]
        {
            let error = validate_visit_pair(candidate_visits, max_candidate_visits)
                .expect_err("partial pair must be a contract violation");
            assert!(error.contains("alignment metrics contract violation"));
        }
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
}
