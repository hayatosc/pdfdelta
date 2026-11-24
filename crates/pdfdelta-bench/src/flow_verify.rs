//! Verifies the flow engine against fixtures whose true edit is known.
//!
//! Generated cases come from the same canonical mutation matrix as
//! `pdfbench verify`; each expected canonical text span is mapped to the exact
//! changed text so the flow script can be checked against truth instead of a
//! bag-of-tokens heuristic. Vendored external pairs cover line-wrap and
//! page-break invariance, a number replacement, a paragraph insertion and
//! deletion, a vertical-text insertion, and a vertical-text page move. The
//! checks are intentionally exact: an invariance case must report no content
//! change, and a content
//! change must cover exactly the mutated text on the changed side and nothing
//! on the other side.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use pdfdelta_core::{
    model::{Document, Glyph},
    pdf::{LopdfParser, ParseLimits},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
    span::ChangeKind,
};
use pdfdelta_flow::{FlowChange, FlowToken, flow_diff, flow_tokens_pair};

use crate::{
    BenchError, Result,
    cases::built_in_cases,
    mutation::{ExpectedCanonicalSpan, ExpectedSemanticChange, RenderPlan},
    renderers::{RenderLimits, RendererKind},
};

/// One flow-verification outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowVerifyRecord {
    /// Fixture identity, including the renderer or external pair name.
    pub case: String,
    /// Expected truth, such as `replace 10 -> 20` or `no content change`.
    pub expected: String,
    /// What the flow script actually reported.
    pub got: String,
    /// Whether the script matched the expected truth exactly.
    pub passed: bool,
    /// Extra evidence for a failure.
    pub detail: String,
}

/// Aggregate flow-verification result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowVerifySummary {
    /// One record per fixture and renderer cell.
    pub records: Vec<FlowVerifyRecord>,
    /// Number of passing records.
    pub passed: usize,
}

impl FlowVerifySummary {
    /// Total number of evaluated records.
    #[must_use]
    pub fn total(&self) -> usize {
        self.records.len()
    }

    /// Whether every record passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.passed == self.total()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Truth {
    NoContentChange,
    Change {
        kind: ChangeKind,
        old_text: String,
        new_text: String,
    },
}

impl Truth {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::NoContentChange => "no content change".to_owned(),
            Self::Change {
                kind,
                old_text,
                new_text,
            } => format!(
                "{} old={:?} new={:?}",
                kind_label(*kind),
                shorten(old_text),
                shorten(new_text)
            ),
        }
    }
}

fn kind_label(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Replacement => "replace",
        ChangeKind::Insertion => "insert",
        ChangeKind::Deletion => "delete",
        ChangeKind::Move => "move",
    }
}

fn shorten(text: &str) -> String {
    const LIMIT: usize = 60;
    if text.chars().count() <= LIMIT {
        return text.to_owned();
    }
    let mut shortened = text.chars().take(LIMIT).collect::<String>();
    shortened.push_str("...");
    shortened
}

/// Runs the flow engine on every generated and vendored fixture with known
/// truth.
///
/// # Errors
///
/// Returns an error when the built-in matrix cannot be constructed, when an
/// external fixture cannot be read, or when output limits reject an input.
pub fn verify_flow_fixtures() -> Result<FlowVerifySummary> {
    let mut records = Vec::new();
    for case in built_in_cases()? {
        let truth = truth_for_expectation(
            case.plan().expectation().changes(),
            case.plan().old(),
            case.plan().new_plan(),
        )?;
        for renderer in RendererKind::all() {
            let name = format!("{} renderer={}", case.name(), renderer.name());
            records.push(evaluate_generated_case(
                &name,
                case.plan().old(),
                case.plan().new_plan(),
                renderer,
                &truth,
            ));
        }
    }
    for external in external_cases()? {
        records.push(evaluate_external_case(&external));
    }
    let passed = records.iter().filter(|record| record.passed).count();
    Ok(FlowVerifySummary { records, passed })
}

pub(crate) fn plan_text(plan: &RenderPlan) -> String {
    plan.pages()
        .iter()
        .flatten()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn span_text(plan: &RenderPlan, span: &ExpectedCanonicalSpan) -> String {
    plan_text(plan)
        .chars()
        .skip(span.start())
        .take(span.end() - span.start())
        .collect()
}

pub(crate) fn truth_for_expectation(
    changes: &[ExpectedSemanticChange],
    old_plan: &RenderPlan,
    new_plan: &RenderPlan,
) -> Result<Truth> {
    match changes {
        [] => Ok(Truth::NoContentChange),
        [change] => {
            let old_text = change
                .old_spans()
                .first()
                .map(|span| span_text(old_plan, span))
                .unwrap_or_default();
            let new_text = change
                .new_spans()
                .first()
                .map(|span| span_text(new_plan, span))
                .unwrap_or_default();
            Ok(Truth::Change {
                kind: change.kind(),
                old_text,
                new_text,
            })
        }
        _ => Err(BenchError::InvalidInput(
            "flow verification supports zero or one expected change per case".to_owned(),
        )),
    }
}

fn evaluate_generated_case(
    name: &str,
    old_plan: &RenderPlan,
    new_plan: &RenderPlan,
    renderer: RendererKind,
    truth: &Truth,
) -> FlowVerifyRecord {
    let result = (|| {
        let limits = RenderLimits::default();
        let old_pdf = renderer.render(old_plan, limits)?;
        let new_pdf = renderer.render(new_plan, limits)?;
        let (old_document, new_document) = extract_pair(&old_pdf, &new_pdf)?;
        compare_documents(name, &old_document, &new_document, truth.clone())
    })();
    match result {
        Ok(record) => record,
        Err(error) => failure(name, truth, error.to_string()),
    }
}

#[derive(Clone, Debug)]
struct ExternalCase {
    name: &'static str,
    old: PathBuf,
    new: PathBuf,
    truth: Truth,
}

fn external_cases() -> Result<Vec<ExternalCase>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/external");
    let paragraph = "運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。";
    let cases = vec![
        ExternalCase {
            name: "external/case1-japanese-typst line-wrap-only",
            old: root.join("case1-japanese-typst/old.pdf"),
            new: root.join("case1-japanese-typst/new.pdf"),
            truth: Truth::NoContentChange,
        },
        ExternalCase {
            name: "external/case2-japanese-typst page-break-only",
            old: root.join("case2-japanese-typst/old.pdf"),
            new: root.join("case2-japanese-typst/new.pdf"),
            truth: Truth::NoContentChange,
        },
        ExternalCase {
            name: "external/case3-typst number-replace",
            old: root.join("case3-typst/old.pdf"),
            new: root.join("case3-typst/new.pdf"),
            truth: Truth::Change {
                kind: ChangeKind::Replacement,
                old_text: "10".to_owned(),
                new_text: "20".to_owned(),
            },
        },
        ExternalCase {
            name: "external/case4-japanese-typst paragraph-insert",
            old: root.join("case4-case5-japanese-typst/old.pdf"),
            new: root.join("case4-case5-japanese-typst/new.pdf"),
            truth: Truth::Change {
                kind: ChangeKind::Insertion,
                old_text: String::new(),
                new_text: paragraph.to_owned(),
            },
        },
        ExternalCase {
            name: "external/case5-japanese-typst paragraph-delete",
            old: root.join("case4-case5-japanese-typst/new.pdf"),
            new: root.join("case4-case5-japanese-typst/old.pdf"),
            truth: Truth::Change {
                kind: ChangeKind::Deletion,
                old_text: paragraph.to_owned(),
                new_text: String::new(),
            },
        },
        ExternalCase {
            name: "external/japanese-typst number-replace",
            old: root.join("japanese-typst/old.pdf"),
            new: root.join("japanese-typst/new.pdf"),
            truth: Truth::Change {
                kind: ChangeKind::Replacement,
                old_text: "10".to_owned(),
                new_text: "20".to_owned(),
            },
        },
        ExternalCase {
            name: "external/vertical-tectonic glyph-insert",
            old: root.join("vertical-tectonic/vertical-old.pdf"),
            new: root.join("vertical-tectonic/vertical-changed.pdf"),
            truth: Truth::Change {
                kind: ChangeKind::Insertion,
                old_text: String::new(),
                new_text: "二".to_owned(),
            },
        },
        ExternalCase {
            name: "external/vertical-tectonic page-move",
            old: root.join("vertical-tectonic/vertical-old.pdf"),
            new: root.join("vertical-tectonic/vertical-moved.pdf"),
            truth: Truth::NoContentChange,
        },
    ];
    for case in &cases {
        for path in [&case.old, &case.new] {
            if !path.is_file() {
                return Err(BenchError::InvalidInput(format!(
                    "external flow fixture is missing: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(cases)
}

fn evaluate_external_case(case: &ExternalCase) -> FlowVerifyRecord {
    let result = (|| {
        let old_pdf = read_bounded_pdf(&case.old)?;
        let new_pdf = read_bounded_pdf(&case.new)?;
        let (old_document, new_document) = extract_pair(&old_pdf, &new_pdf)?;
        compare_documents(case.name, &old_document, &new_document, case.truth.clone())
    })();
    match result {
        Ok(record) => record,
        Err(error) => failure(case.name, &case.truth, error.to_string()),
    }
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

pub(crate) fn extract_pair(
    old_pdf: &[u8],
    new_pdf: &[u8],
) -> Result<(Document<Glyph>, Document<Glyph>)> {
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let parse_limits = ParseLimits::default();
    let extraction_limits = ExtractionLimits::default();
    let old = source
        .extract_outcome(Arc::from(old_pdf), parse_limits, extraction_limits)
        .map_err(|error| BenchError::core("old extraction", error))?;
    let new = source
        .extract_outcome(Arc::from(new_pdf), parse_limits, extraction_limits)
        .map_err(|error| BenchError::core("new extraction", error))?;
    for (side, outcome) in [("old", &old), ("new", &new)] {
        if !outcome.is_complete() {
            return Err(BenchError::InvalidInput(format!(
                "{side} extraction is incomplete: {}",
                outcome
                    .issues()
                    .first()
                    .map_or("unknown issue", |issue| issue.description())
            )));
        }
    }
    Ok((old.document().clone(), new.document().clone()))
}

fn compare_documents(
    name: &str,
    old_document: &Document<Glyph>,
    new_document: &Document<Glyph>,
    truth: Truth,
) -> Result<FlowVerifyRecord> {
    let pair = flow_tokens_pair(old_document, new_document)
        .map_err(|error| BenchError::core("flow tokenization", error))?;
    let diff = flow_diff(&pair.old.tokens, &pair.new.tokens)
        .map_err(|error| BenchError::InvalidInput(format!("flow verification failed: {error}")))?;
    let actual = summarize_changes(&diff.changes, &pair.old.tokens, &pair.new.tokens);
    let expected = truth.label();
    let (passed, detail) = check_truth(&truth, &actual);
    Ok(FlowVerifyRecord {
        case: name.to_owned(),
        expected,
        got: actual.label(),
        passed,
        detail,
    })
}

#[derive(Debug, Default)]
pub(crate) struct ActualChanges {
    pub(crate) content: Vec<(String, String, String)>,
    pub(crate) moves: Vec<(String, String)>,
    pub(crate) layout_only: usize,
}

impl ActualChanges {
    pub(crate) fn label(&self) -> String {
        let mut parts = Vec::new();
        for (kind, _, _) in &self.content {
            if !parts.iter().any(|part| part == kind) {
                parts.push(kind.clone());
            }
        }
        if !self.moves.is_empty() {
            parts.push("move".to_owned());
        }
        if self.content.is_empty() && self.moves.is_empty() {
            if self.layout_only > 0 {
                return format!("layout-only x{}", self.layout_only);
            }
            return "none".to_owned();
        }
        if self.layout_only > 0 {
            parts.push(format!("layout-only x{}", self.layout_only));
        }
        parts.join(",")
    }

    pub(crate) fn old_text(&self) -> String {
        self.content
            .iter()
            .filter(|(kind, _, _)| kind == "replace" || kind == "delete")
            .map(|(_, old, _)| old.as_str())
            .collect()
    }

    pub(crate) fn new_text(&self) -> String {
        self.content
            .iter()
            .filter(|(kind, _, _)| kind == "replace" || kind == "insert")
            .map(|(_, _, new)| new.as_str())
            .collect()
    }
}

pub(crate) fn summarize_changes(
    changes: &[FlowChange],
    old: &[FlowToken],
    new: &[FlowToken],
) -> ActualChanges {
    let mut actual = ActualChanges::default();
    for change in changes {
        match change {
            FlowChange::Delete { old: range } => actual.content.push((
                "delete".to_owned(),
                token_text(&old[range.clone()]),
                String::new(),
            )),
            FlowChange::Insert { new: range } => actual.content.push((
                "insert".to_owned(),
                String::new(),
                token_text(&new[range.clone()]),
            )),
            FlowChange::Replace {
                old: old_range,
                new: new_range,
            } => actual.content.push((
                "replace".to_owned(),
                token_text(&old[old_range.clone()]),
                token_text(&new[new_range.clone()]),
            )),
            FlowChange::Move {
                old: old_range,
                new: new_range,
            } => actual.moves.push((
                token_text(&old[old_range.clone()]),
                token_text(&new[new_range.clone()]),
            )),
            FlowChange::LayoutOnly { .. } => actual.layout_only += 1,
        }
    }
    actual
}

pub(crate) fn token_text(tokens: &[FlowToken]) -> String {
    tokens.iter().map(|token| token.text.as_str()).collect()
}

pub(crate) fn check_truth(truth: &Truth, actual: &ActualChanges) -> (bool, String) {
    match truth {
        Truth::NoContentChange => {
            let passed = actual.content.is_empty();
            let detail = if passed {
                String::new()
            } else {
                format!(
                    "content changes: {}",
                    actual
                        .content
                        .iter()
                        .map(|(kind, old, new)| format!("{kind} {old:?} -> {new:?}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            };
            (passed, detail)
        }
        Truth::Change {
            kind: ChangeKind::Move,
            old_text,
            new_text,
        } => {
            let mut failures = Vec::new();
            if !actual.content.is_empty() {
                failures.push(format!(
                    "expected no content changes, got {} ({})",
                    actual.label(),
                    actual
                        .content
                        .iter()
                        .map(|(kind, old, new)| format!("{kind} {old:?} -> {new:?}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }
            if actual.moves.len() != 1 {
                failures.push(format!("expected one move, got {}", actual.moves.len()));
            }
            let moved_old = actual.moves.first().map_or("", |entry| entry.0.as_str());
            let moved_new = actual.moves.first().map_or("", |entry| entry.1.as_str());
            if normalize_text(moved_old) != normalize_text(old_text) {
                failures.push(format!(
                    "moved old text differs: expected {:?}, got {:?}",
                    shorten(old_text),
                    shorten(moved_old)
                ));
            }
            if normalize_text(moved_new) != normalize_text(new_text) {
                failures.push(format!(
                    "moved new text differs: expected {:?}, got {:?}",
                    shorten(new_text),
                    shorten(moved_new)
                ));
            }
            (failures.is_empty(), failures.join("; "))
        }
        Truth::Change {
            kind,
            old_text,
            new_text,
        } => {
            let expected_kind = kind_label(*kind);
            let single = actual.content.len() == 1 && actual.moves.is_empty();
            let (actual_kind, _, _) = actual
                .content
                .first()
                .map_or(("none", &String::new(), &String::new()), |entry| {
                    (entry.0.as_str(), &entry.1, &entry.2)
                });
            let (old_matches, new_matches) =
                token_boundary_match(&actual.old_text(), &actual.new_text(), old_text, new_text);
            let mut failures = Vec::new();
            if !single {
                failures.push(format!(
                    "expected one change, got {} content change(s) and {} move(s)",
                    actual.content.len(),
                    actual.moves.len()
                ));
            }
            if actual_kind != expected_kind {
                failures.push(format!("expected kind {expected_kind}, got {actual_kind}"));
            }
            if !old_matches {
                failures.push(format!(
                    "old text differs: expected {:?}, got {:?}",
                    shorten(old_text),
                    shorten(&actual.old_text())
                ));
            }
            if !new_matches {
                failures.push(format!(
                    "new text differs: expected {:?}, got {:?}",
                    shorten(new_text),
                    shorten(&actual.new_text())
                ));
            }
            (failures.is_empty(), failures.join("; "))
        }
    }
}

pub(crate) fn normalize_text(text: &str) -> String {
    text.chars()
        .filter(|scalar| !scalar.is_whitespace())
        .collect()
}

/// Compares changed token text with a canonical character span.
///
/// A mutation can change one character inside a token: replacing `1` with `2`
/// in `10` is a one-scalar change in the canonical source, but the flow token
/// is `10` and becomes `20`. The expected span is therefore allowed to be a
/// substring of the changed token text when the remaining characters are
/// identical on both sides; those characters did not change.
fn token_boundary_match(
    actual_old: &str,
    actual_new: &str,
    expected_old: &str,
    expected_new: &str,
) -> (bool, bool) {
    let actual_old = normalize_text(actual_old);
    let actual_new = normalize_text(actual_new);
    let expected_old = normalize_text(expected_old);
    let expected_new = normalize_text(expected_new);
    if actual_old == expected_old && actual_new == expected_new {
        return (true, true);
    }
    let Some(old_position) = actual_old.find(&expected_old) else {
        return (false, false);
    };
    let Some(new_position) = actual_new.find(&expected_new) else {
        return (false, false);
    };
    let old_residual = remove_at(&actual_old, old_position, expected_old.len());
    let new_residual = remove_at(&actual_new, new_position, expected_new.len());
    (old_residual == new_residual, old_residual == new_residual)
}

fn remove_at(text: &str, start: usize, length: usize) -> String {
    let mut residual = String::with_capacity(text.len().saturating_sub(length));
    residual.push_str(&text[..start]);
    residual.push_str(&text[start + length..]);
    residual
}

fn failure(name: &str, truth: &Truth, detail: String) -> FlowVerifyRecord {
    FlowVerifyRecord {
        case: name.to_owned(),
        expected: truth.label(),
        got: "error".to_owned(),
        passed: false,
        detail,
    }
}

/// Formats one verification record as a single table row.
#[must_use]
pub fn format_record(record: &FlowVerifyRecord) -> String {
    format!(
        "{} case={} expected={} got={} detail={}",
        if record.passed { "PASS" } else { "FAIL" },
        record.case,
        record.expected,
        record.got,
        record.detail
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_cases_have_existing_pdfs() {
        let cases = external_cases().expect("external fixtures resolve");
        assert!(cases.len() >= 8);
    }

    #[test]
    fn known_fixtures_stay_above_the_order_coherence_threshold() {
        let mut minimum = f64::INFINITY;
        for case in built_in_cases().expect("cases build") {
            for renderer in RendererKind::all() {
                let limits = RenderLimits::default();
                let old_pdf = renderer
                    .render(case.plan().old(), limits)
                    .expect("old render");
                let new_pdf = renderer
                    .render(case.plan().new_plan(), limits)
                    .expect("new render");
                let (old, new) = extract_pair(&old_pdf, &new_pdf).expect("extract");
                let pair = flow_tokens_pair(&old, &new).expect("order selection");
                for page in pair.old_pages.iter().chain(&pair.new_pages) {
                    minimum = minimum.min(page.coherence);
                }
            }
        }
        assert!(
            minimum >= pdfdelta_flow::ORDER_COHERENCE_MIN,
            "minimum generated fixture coherence {minimum}"
        );
    }

    #[test]
    fn generated_truth_is_zero_or_one_change() {
        let cases = built_in_cases().expect("cases build");
        for case in cases {
            let truth = truth_for_expectation(
                case.plan().expectation().changes(),
                case.plan().old(),
                case.plan().new_plan(),
            );
            assert!(truth.is_ok(), "{}: {truth:?}", case.name());
        }
    }
}
