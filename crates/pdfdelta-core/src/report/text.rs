//! Presentation-only contextual unified-diff projection of an exact
//! `Comparison`. The renderer never mutates the underlying changes, spans,
//! or JSON semantics; it only regroups and decorates what the exact diff
//! already produced.

use std::fmt::Write;

use crate::{
    Error, Result,
    diff::{
        ChangeEvent, ChangeKind, ChangeTag, ChangedRegionProof, Comparison, Confidence, TextSpan,
    },
    layout::BlockId,
    model::FontProgramHash,
    normalize::{BlockText, ComparableToken},
    source::ExtractionScope,
};

use super::{
    ExtractionStatus, ReportSummary, ResolvedGroup, SideIndex, TextReportOptions,
    assessment_reason, assumption, change_kind, change_tag, confidence as confidence_name,
    issue_kind_name, lowercase_hex, percentage, relation_outcome, search_completeness, side_name,
    yes_no,
};

/// Maximum unchanged tokens between adjacent edits to coalesce into a single hunk.
const COALESCE_MAX_EQUAL_TOKENS: usize = 16;

/// Number of context tokens displayed around changed regions in unified diffs.
const CONTEXT_WINDOW_TOKENS: usize = 32;

const CODE_FILE_HEADER: &str = "\x1b[1m";
const CODE_HUNK_HEADER: &str = "\x1b[1;36m";
const CODE_MINUS: &str = "\x1b[31m";
const CODE_MINUS_EMPHASIS: &str = "\x1b[1;31m";
const CODE_PLUS: &str = "\x1b[32m";
const CODE_PLUS_EMPHASIS: &str = "\x1b[1;32m";
const CODE_WARNING: &str = "\x1b[33m";
const CODE_MOVE: &str = "\x1b[35m";
const RESET: &str = "\x1b[0m";

pub(super) fn render(
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    comparison: &Comparison,
    extraction: &ExtractionStatus,
    summary: &ReportSummary,
    options: &TextReportOptions<'_>,
) -> Result<String> {
    let old = SideIndex::new(old_blocks)?;
    let new = SideIndex::new(new_blocks)?;
    let painter = Painter {
        enabled: options.color,
    };

    let mut output = String::new();
    writeln!(
        output,
        "content changes: {} · proven changed regions: {} · formatting-only: {} · uncertain: {} · unresolved regions: {} · coverage {} · established changes: {} · tentative candidates: {} · difference: {} · completeness: {}",
        summary.content_changes,
        summary.proven_changed_regions,
        summary.formatting_only_changes,
        summary.uncertain_changes,
        summary.unresolved_regions,
        percentage(summary.comparison_coverage),
        summary.established_changes,
        summary.tentative_candidates,
        summary.difference_status.as_str(),
        completeness(summary.comparison_complete),
    )
    .map_err(|error| Error::Report(error.to_string()))?;
    writeln!(
        output,
        "comparison scope: supported text={}, images {}",
        if summary.comparison_scope.supported_text {
            "yes"
        } else {
            "no"
        },
        if summary.comparison_scope.images_compared {
            "compared"
        } else {
            "not compared"
        },
    )
    .map_err(|error| Error::Report(error.to_string()))?;
    if let Some(assessment) = &comparison.assessment {
        for unit in &assessment.review_units {
            let Some(bounds) = unit.changed_count else {
                continue;
            };
            if bounds.upper == 0 {
                continue;
            }
            writeln!(output, "review unit (relation={}): {}..={} changed source tokens under literal-minimal alignment",
                unit.relation, bounds.lower, bounds.upper)
                .map_err(|error| Error::Report(error.to_string()))?;
            if let Some(residual) = unit.unresolved_changed_count {
                writeln!(output, "  unresolved remainder: {}..={} changed source tokens; positions may remain uncertain",
                    residual.lower, residual.upper)
                    .map_err(|error| Error::Report(error.to_string()))?;
            }
        }
    }
    if !extraction.old_complete || !extraction.new_complete || !extraction.issues.is_empty() {
        writeln!(
            output,
            "{}",
            painter.paint(
                CODE_WARNING,
                &format!(
                    "extraction incomplete: old={}, new={}",
                    yes_no(summary.old_extraction_complete),
                    yes_no(summary.new_extraction_complete),
                ),
            )
        )
        .map_err(|error| Error::Report(error.to_string()))?;
        for issue in &extraction.issues {
            let scope = match issue.scope {
                ExtractionScope::Document => "scope=document".to_owned(),
                ExtractionScope::Page(page) => format!("scope=page, page={}", (page.0 as u64) + 1),
                ExtractionScope::PageGap { retained_before } => {
                    format!("scope=page-gap, retained-pages-before={retained_before}")
                }
                ExtractionScope::GlyphGap { retained_before } => {
                    format!("scope=glyph-gap, retained-glyphs-before={retained_before}")
                }
            };
            writeln!(
                output,
                "{}",
                painter.paint(
                    CODE_WARNING,
                    &format!(
                        "! extraction issue (side={}, kind={}, {}): {}",
                        side_name(issue.side),
                        issue_kind_name(issue.kind),
                        scope,
                        issue.description,
                    ),
                )
            )
            .map_err(|error| Error::Report(error.to_string()))?;
        }
    }

    writeln!(output).map_err(|error| Error::Report(error.to_string()))?;
    for (label, marker) in [(options.old_label, "--- "), (options.new_label, "+++ ")] {
        writeln!(
            output,
            "{}",
            painter.paint(CODE_FILE_HEADER, &format!("{marker}{label}"))
        )
        .map_err(|error| Error::Report(error.to_string()))?;
    }

    for cluster in cluster_changes(comparison) {
        writeln!(output).map_err(|error| Error::Report(error.to_string()))?;
        let mut body = Vec::new();
        let mut pages = Vec::new();
        append_cluster_body(&cluster, &old, &new, &mut pages, &mut body, &painter)?;
        pages.sort_unstable();
        pages.dedup();
        writeln!(
            output,
            "{}",
            painter.paint(CODE_HUNK_HEADER, &cluster.header(&pages))
        )
        .map_err(|error| Error::Report(error.to_string()))?;
        for line in body {
            writeln!(output, "{line}").map_err(|error| Error::Report(error.to_string()))?;
        }
    }

    for region in &comparison.proven_changed_regions {
        writeln!(output).map_err(|error| Error::Report(error.to_string()))?;
        let mut pages = Vec::new();
        let mut side_notes = Vec::new();
        for span in region.old_span.iter() {
            let window = resolve_window(&old, span)?;
            pages.extend_from_slice(&window.pages);
            side_notes.push(("old", window.render_marked_region()));
        }
        for span in region.new_span.iter() {
            let window = resolve_window(&new, span)?;
            pages.extend_from_slice(&window.pages);
            side_notes.push(("new", window.render_marked_region()));
        }
        pages.sort_unstable();
        pages.dedup();
        writeln!(
            output,
            "{}",
            painter.paint(
                CODE_HUNK_HEADER,
                &format!("@@ {} · PROVEN CONTENT DIFFERENCE @@", format_pages(&pages)),
            )
        )
        .map_err(|error| Error::Report(error.to_string()))?;
        let proof = match region.proof {
            ChangedRegionProof::ExactTokenMultisetMismatch => "exact token multiset mismatch",
            ChangedRegionProof::OneSidedNonEmptyRange => "one-sided non-empty range",
        };
        writeln!(
            output,
            "{}",
            painter.paint(
                CODE_WARNING,
                &format!(
                    "! content differs ({proof}; confidence: {})",
                    confidence_name(region.confidence)
                ),
            )
        )
        .map_err(|error| Error::Report(error.to_string()))?;
        for (side, text) in side_notes {
            writeln!(
                output,
                "{}",
                painter.paint(CODE_WARNING, &format!("! {side}: {text}"))
            )
            .map_err(|error| Error::Report(error.to_string()))?;
        }
    }

    for candidate in &comparison.change_candidates {
        let assessment = comparison
            .assessment
            .as_ref()
            .and_then(|assessment| assessment.relations.get(candidate.relation))
            .ok_or_else(|| {
                Error::InvalidConfiguration(
                    "candidate refers to a missing assessment relation".to_owned(),
                )
            })?;
        writeln!(output).map_err(|error| Error::Report(error.to_string()))?;
        let mut pages = Vec::new();
        let mut side_notes = Vec::new();
        for span in candidate
            .change
            .occurrences
            .iter()
            .filter_map(|occurrence| occurrence.old_span.as_ref())
        {
            let window = resolve_window(&old, span)?;
            pages.extend_from_slice(&window.pages);
            side_notes.push(("old", window.render_marked_region()));
        }
        for span in candidate
            .change
            .occurrences
            .iter()
            .filter_map(|occurrence| occurrence.new_span.as_ref())
        {
            let window = resolve_window(&new, span)?;
            pages.extend_from_slice(&window.pages);
            side_notes.push(("new", window.render_marked_region()));
        }
        pages.sort_unstable();
        pages.dedup();
        writeln!(
            output,
            "{}",
            painter.paint(
                CODE_HUNK_HEADER,
                &format!(
                    "@@ {} · TENTATIVE · candidate group {} · relation {} @@",
                    format_pages(&pages),
                    candidate.alternative_group + 1,
                    candidate.relation,
                ),
            )
        )
        .map_err(|error| Error::Report(error.to_string()))?;
        let reasons = assessment
            .reasons
            .iter()
            .copied()
            .map(assessment_reason)
            .collect::<Vec<_>>()
            .join(", ");
        let assumptions = assessment
            .assumptions
            .iter()
            .copied()
            .map(assumption)
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            output,
            "{}",
            painter.paint(
                CODE_WARNING,
                &format!(
                    "? possible {} (reasons: {}; assumptions: {}; outcome: {}; search: {}; confidence: {})",
                    change_kind(candidate.change.kind),
                    reasons,
                    assumptions,
                    relation_outcome(assessment.outcome),
                    search_completeness(assessment.search),
                    confidence_name(candidate.change.confidence),
                ),
            ),
        )
        .map_err(|error| Error::Report(error.to_string()))?;
        for (side, text) in side_notes {
            writeln!(
                output,
                "{}",
                painter.paint(CODE_WARNING, &format!("? {side}: {text}")),
            )
            .map_err(|error| Error::Report(error.to_string()))?;
        }
    }

    for region in &comparison.unresolved_regions {
        writeln!(output).map_err(|error| Error::Report(error.to_string()))?;
        let mut pages = Vec::new();
        let mut side_notes = Vec::new();
        for span in region.old_span.iter() {
            let window = resolve_window(&old, span)?;
            pages.extend_from_slice(&window.pages);
            side_notes.push(("old", window.render_marked_region()));
        }
        for span in region.new_span.iter() {
            let window = resolve_window(&new, span)?;
            pages.extend_from_slice(&window.pages);
            side_notes.push(("new", window.render_marked_region()));
        }
        pages.sort_unstable();
        pages.dedup();

        writeln!(
            output,
            "{}",
            painter.paint(
                CODE_HUNK_HEADER,
                &format!("@@ {} · UNRESOLVED @@", format_pages(&pages)),
            )
        )
        .map_err(|error| Error::Report(error.to_string()))?;
        let evidence = super::json::evidence_label_list(&region.evidence);
        let reason = if evidence.is_empty() {
            "could not safely align this region".to_owned()
        } else {
            format!("could not safely align this region (evidence: {evidence})")
        };
        writeln!(
            output,
            "{}",
            painter.paint(CODE_WARNING, &format!("? {reason}"))
        )
        .map_err(|error| Error::Report(error.to_string()))?;
        for (side, text) in side_notes {
            writeln!(
                output,
                "{}",
                painter.paint(CODE_WARNING, &format!("? {side}: {text}"))
            )
            .map_err(|error| Error::Report(error.to_string()))?;
        }
    }

    Ok(output)
}

/// Renders one presentation hunk body, merging nearby ranges only within a
/// shared block group and separator coordinate system.
fn append_cluster_body(
    cluster: &Cluster<'_>,
    old: &SideIndex<'_>,
    new: &SideIndex<'_>,
    pages: &mut Vec<u32>,
    body: &mut Vec<String>,
    painter: &Painter,
) -> Result<()> {
    let first = cluster.changes[0];
    if first.kind == ChangeKind::Move {
        let move_change = cluster.changes[0];
        let mut old_pages = Vec::new();
        let mut new_pages = Vec::new();
        for span in old_spans(move_change) {
            old_pages.extend(resolve_window(old, span)?.pages);
        }
        for span in new_spans(move_change) {
            new_pages.extend(resolve_window(new, span)?.pages);
        }
        old_pages.sort_unstable();
        old_pages.dedup();
        new_pages.sort_unstable();
        new_pages.dedup();
        let marker = if old_pages == new_pages {
            format!("~ moved within {}", format_pages(&old_pages))
        } else {
            format!(
                "~ moved from {} to {}",
                format_pages(&old_pages),
                format_pages(&new_pages),
            )
        };
        pages.extend_from_slice(&old_pages);
        pages.extend_from_slice(&new_pages);
        body.push(painter.paint(CODE_MOVE, &marker));
        for occurrence in &move_change.occurrences {
            append_marked_spans(
                occurrence.old_span.as_ref(),
                old,
                MINUS_STYLE,
                pages,
                body,
                painter,
            )?;
            append_marked_spans(
                occurrence.new_span.as_ref(),
                new,
                PLUS_STYLE,
                pages,
                body,
                painter,
            )?;
        }
        return Ok(());
    }

    let old_spans = cluster
        .changes
        .iter()
        .flat_map(|change| old_spans(change))
        .collect::<Vec<_>>();
    let new_spans = cluster
        .changes
        .iter()
        .flat_map(|change| new_spans(change))
        .collect::<Vec<_>>();
    append_merged_runs(&old_spans, old, MINUS_STYLE, pages, body, painter)?;
    append_merged_runs(&new_spans, new, PLUS_STYLE, pages, body, painter)?;
    Ok(())
}

fn append_merged_runs(
    spans: &[&TextSpan],
    index: &SideIndex<'_>,
    style: LineStyle,
    pages: &mut Vec<u32>,
    body: &mut Vec<String>,
    painter: &Painter,
) -> Result<()> {
    let mut compatible_groups: Vec<Vec<&TextSpan>> = Vec::new();
    for span in spans {
        if let Some(group) = compatible_groups
            .iter_mut()
            .find(|group| group[0].blocks == span.blocks && group[0].separator == span.separator)
        {
            group.push(*span);
        } else {
            compatible_groups.push(vec![*span]);
        }
    }
    for spans in compatible_groups {
        let template = spans[0];
        let group = index.resolve_group(&template.blocks, template.separator)?;
        for span in &spans {
            validate_span_against_group(span, &group)?;
        }
        for (start, end) in merged_edited_ranges(spans.into_iter()) {
            let window = bounded_window(&group, start, end);
            pages.extend_from_slice(&window.pages);
            body.push(window.render_marked(style, painter));
        }
    }
    Ok(())
}

fn append_marked_spans(
    span: Option<&TextSpan>,
    index: &SideIndex<'_>,
    style: LineStyle,
    pages: &mut Vec<u32>,
    body: &mut Vec<String>,
    painter: &Painter,
) -> Result<()> {
    let Some(span) = span else {
        return Ok(());
    };
    let window = resolve_window(index, span)?;
    pages.extend_from_slice(&window.pages);
    body.push(window.render_marked(style, painter));
    Ok(())
}

/// Unions nearby edited comparable-token ranges into contiguous hunk spans.
fn merged_edited_ranges<'a>(spans: impl Iterator<Item = &'a TextSpan>) -> Vec<(usize, usize)> {
    let mut spans = spans.collect::<Vec<_>>();
    spans.sort_unstable_by_key(|span| (span.comparable_range.start, span.comparable_range.end));
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for span in spans {
        match merged.last_mut() {
            Some((_, last_end))
                if span.comparable_range.start <= *last_end + COALESCE_MAX_EQUAL_TOKENS =>
            {
                *last_end = (*last_end).max(span.comparable_range.end);
            }
            _ => merged.push((span.comparable_range.start, span.comparable_range.end)),
        }
    }
    merged
}

fn old_spans(change: &ChangeEvent) -> impl DoubleEndedIterator<Item = &TextSpan> {
    change
        .occurrences
        .iter()
        .filter_map(|occurrence| occurrence.old_span.as_ref())
}

fn new_spans(change: &ChangeEvent) -> impl DoubleEndedIterator<Item = &TextSpan> {
    change
        .occurrences
        .iter()
        .filter_map(|occurrence| occurrence.new_span.as_ref())
}

struct Cluster<'a> {
    changes: Vec<&'a ChangeEvent>,
    last_old: Option<&'a TextSpan>,
    last_new: Option<&'a TextSpan>,
    confidence: Confidence,
    tags: Vec<ChangeTag>,
}

impl<'a> Cluster<'a> {
    fn starting(change: &'a ChangeEvent) -> Self {
        Self {
            changes: vec![change],
            last_old: old_spans(change).next_back(),
            last_new: new_spans(change).next_back(),
            confidence: change.confidence,
            tags: change.tags.clone(),
        }
    }

    fn can_absorb(&self, next: &ChangeEvent) -> bool {
        if next.kind == ChangeKind::Move
            || next.occurrences.len() != 1
            || self
                .changes
                .first()
                .is_some_and(|first| first.kind == ChangeKind::Move || first.occurrences.len() != 1)
        {
            // Moves and grouped semantic occurrences are standalone records;
            // only ordinary single-occurrence edits use proximity clustering.
            return false;
        }
        if next.confidence != self.confidence || next.tags != self.tags {
            return false;
        }
        side_close(self.last_old, old_spans(next).next())
            && side_close(self.last_new, new_spans(next).next())
    }

    fn absorb(&mut self, next: &'a ChangeEvent) {
        if let Some(span) = old_spans(next).next_back() {
            self.last_old = Some(span);
        }
        if let Some(span) = new_spans(next).next_back() {
            self.last_new = Some(span);
        }
        self.changes.push(next);
    }

    fn header(&self, pages: &[u32]) -> String {
        let old_blocks =
            unique_block_groups(self.changes.iter().flat_map(|change| old_spans(change)));
        let new_blocks =
            unique_block_groups(self.changes.iter().flat_map(|change| new_spans(change)));
        let old_blocks = block_groups_label(&old_blocks);
        let new_blocks = block_groups_label(&new_blocks);
        let mut parts = vec![format_pages(pages)];
        match (old_blocks.as_deref(), new_blocks.as_deref()) {
            (Some(old), Some(new)) => parts.push(format!("old {old} -> new {new}")),
            (Some(old), None) => parts.push(format!("old {old}")),
            (None, Some(new)) => parts.push(format!("new {new}")),
            (None, None) => {}
        }
        parts.push(format!("confidence: {}", confidence_name(self.confidence),));
        if !self.tags.is_empty() {
            let tags = self
                .tags
                .iter()
                .copied()
                .map(change_tag)
                .collect::<Vec<_>>()
                .join(",");
            parts.push(format!("tags: {tags}"));
        }
        format!("@@ {} @@", parts.join(" · "))
    }
}

fn unique_block_groups<'a>(spans: impl Iterator<Item = &'a TextSpan>) -> Vec<&'a [BlockId]> {
    let mut groups = Vec::new();
    for span in spans {
        let blocks = span.blocks.as_slice();
        if !groups.contains(&blocks) {
            groups.push(blocks);
        }
    }
    groups
}

fn block_groups_label(groups: &[&[BlockId]]) -> Option<String> {
    match groups {
        [] => None,
        [blocks] => Some(blocks_label(blocks)),
        groups => Some(format!(
            "groups [{}]",
            groups
                .iter()
                .map(|blocks| blocks_label(blocks))
                .collect::<Vec<_>>()
                .join("; ")
        )),
    }
}

fn side_close(previous: Option<&TextSpan>, next: Option<&TextSpan>) -> bool {
    match (previous, next) {
        (_, None) | (None, _) => true,
        (Some(previous), Some(next)) => {
            // Separator equality keeps the two ranges in one coordinate
            // system: the same blocks concatenate differently under Space
            // versus Concatenate, so their token indexes are not comparable.
            previous.blocks == next.blocks
                && previous.separator == next.separator
                && next.comparable_range.start >= previous.comparable_range.end
                && next.comparable_range.start - previous.comparable_range.end
                    <= COALESCE_MAX_EQUAL_TOKENS
        }
    }
}

fn cluster_changes(comparison: &Comparison) -> Vec<Cluster<'_>> {
    let mut clusters: Vec<Cluster<'_>> = Vec::new();
    for change in &comparison.changes {
        if let Some(active) = clusters.last_mut()
            && active.can_absorb(change)
        {
            active.absorb(change);
            continue;
        }
        clusters.push(Cluster::starting(change));
    }
    clusters
}

fn blocks_label(blocks: &[BlockId]) -> String {
    let ids = blocks
        .iter()
        .map(|block| block.0.to_string())
        .collect::<Vec<_>>()
        .join(",");
    if blocks.len() == 1 {
        format!("block {ids}")
    } else {
        format!("blocks {ids}")
    }
}

/// Renders one-based, compressed page locations such as `page 3`,
/// `pages 1-3`, or `pages 1-2,5`.
fn format_pages(pages: &[u32]) -> String {
    if pages.is_empty() {
        return "unknown location".to_owned();
    }
    let mut runs = Vec::new();
    let mut start = pages[0];
    let mut end = pages[0];
    for page in &pages[1..] {
        if (*page as u64) == (end as u64) + 1 {
            end = *page;
        } else {
            runs.push((start, end));
            start = *page;
            end = *page;
        }
    }
    runs.push((start, end));
    let joined = runs
        .iter()
        .map(|(start, end)| {
            let s = (*start as u64) + 1;
            let e = (*end as u64) + 1;
            if start == end {
                s.to_string()
            } else {
                format!("{s}-{e}")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    if runs.len() == 1 && runs[0].0 == runs[0].1 {
        format!("page {joined}")
    } else {
        format!("pages {joined}")
    }
}

struct SideWindow {
    pages: Vec<u32>,
    pre: String,
    changed: String,
    post: String,
    truncated_left: bool,
    truncated_right: bool,
}

/// Marker, base color, and emphasis color for one kind of diff line.
#[derive(Clone, Copy)]
struct LineStyle {
    marker: char,
    base: &'static str,
    emphasis: &'static str,
}

const MINUS_STYLE: LineStyle = LineStyle {
    marker: '-',
    base: CODE_MINUS,
    emphasis: CODE_MINUS_EMPHASIS,
};

const PLUS_STYLE: LineStyle = LineStyle {
    marker: '+',
    base: CODE_PLUS,
    emphasis: CODE_PLUS_EMPHASIS,
};

impl SideWindow {
    fn render_marked(&self, style: LineStyle, painter: &Painter) -> String {
        let mut head = String::new();
        head.push(style.marker);
        head.push(' ');
        if self.truncated_left {
            head.push_str("... ");
        }
        head.push_str(&self.pre);
        let mut tail = self.post.clone();
        if self.truncated_right {
            tail.push_str(" ...");
        }
        let mut line = painter.paint(style.base, &head);
        line.push_str(&painter.paint(style.emphasis, &self.changed));
        line.push_str(&painter.paint(style.base, &tail));
        line
    }

    /// Unresolved regions carry no marker semantics, so the window renders as
    /// one informational string instead of -/+ lines.
    fn render_marked_region(&self) -> String {
        let mut text = String::new();
        if self.truncated_left {
            text.push_str("... ");
        }
        text.push_str(&self.pre);
        text.push_str(&self.changed);
        text.push_str(&self.post);
        if self.truncated_right {
            text.push_str(" ...");
        }
        text
    }
}

/// Resolves the bounded context window around one edited span. The group is
/// the containing normalized block(s), so a tiny edit still shows enough
/// surrounding words to identify what changed. The span is validated against
/// the group evidence exactly like the JSON path, so malformed ranges fail
/// loudly in both report modes instead of being silently clamped here.
fn resolve_window(index: &SideIndex<'_>, span: &TextSpan) -> Result<SideWindow> {
    let group = index.resolve_group(&span.blocks, span.separator)?;
    validate_span_against_group(span, &group)?;
    Ok(bounded_window(
        &group,
        span.comparable_range.start,
        span.comparable_range.end,
    ))
}

/// Cuts the bounded context window around one already validated edited range
/// of a resolved group.
fn bounded_window(group: &ResolvedGroup, start: usize, end: usize) -> SideWindow {
    let total = group.tokens.len();
    let window_start = start.saturating_sub(CONTEXT_WINDOW_TOKENS);
    let window_end = end.saturating_add(CONTEXT_WINDOW_TOKENS).min(total);
    SideWindow {
        pages: group.pages.clone(),
        pre: render_region(group, window_start, start),
        changed: render_region(group, start, end),
        post: render_region(group, end, window_end),
        truncated_left: window_start > 0,
        truncated_right: window_end < total,
    }
}

/// Shared bounds contract with `SideIndex::resolve`: the exact message and
/// checks must stay identical so text and JSON modes reject the same spans.
fn validate_span_against_group(span: &TextSpan, group: &ResolvedGroup) -> Result<()> {
    let scalar_count = group
        .tokens
        .iter()
        .filter(|token| token.is_scalar())
        .count();
    if span.canonical_range.start > span.canonical_range.end
        || span.comparable_range.start > span.comparable_range.end
        || span.comparable_range.end > group.tokens.len()
        || span.canonical_range.end > scalar_count
    {
        return Err(Error::InvalidConfiguration(
            "text span range exceeds the normalized block evidence".to_owned(),
        ));
    }
    Ok(())
}

/// Renders `[start, end)` of the group's comparable tokens, emitting stable
/// placeholders for unmapped glyphs at their exact positions so the changed
/// segment identifies pure-unmapped and mixed edits just like mapped ones.
fn render_region(group: &ResolvedGroup, start: usize, end: usize) -> String {
    let total = group.tokens.len();
    let end = end.min(total);
    let start = start.min(end);
    let mut rendered = String::new();
    for token in &group.tokens[start..end] {
        match token {
            ComparableToken::Scalar(scalar) => rendered.push(*scalar),
            ComparableToken::Unmapped {
                font_hash,
                glyph_id,
            } => {
                rendered.push_str(&unmapped_placeholder(font_hash, *glyph_id));
            }
        }
    }
    rendered
}

// Abbreviates the font hash to its first four bytes for human-readable display.
fn unmapped_placeholder(font_hash: &FontProgramHash, glyph_id: u16) -> String {
    let prefix = &font_hash.0[..font_hash.0.len().min(4)];
    let hash = lowercase_hex(prefix);
    format!("<unmapped:{glyph_id}:{hash}>")
}

fn completeness(value: bool) -> &'static str {
    if value { "complete" } else { "incomplete" }
}

struct Painter {
    enabled: bool,
}

impl Painter {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.enabled && !text.is_empty() {
            format!("{code}{text}{RESET}")
        } else {
            text.to_owned()
        }
    }
}
