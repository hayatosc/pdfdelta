//! Presentation-only contextual unified-diff projection of an exact
//! `Comparison`. The renderer never mutates the underlying changes, spans,
//! or JSON semantics; it only regroups and decorates what the exact diff
//! already produced.

use std::fmt::Write;

use crate::{
    Error, Result,
    diff::{Change, ChangeKind, ChangeTag, Comparison, Confidence, TextSpan},
    layout::BlockId,
    model::FontProgramHash,
    normalize::{BlockText, ComparableToken},
    source::ExtractionScope,
};

use super::{
    ExtractionStatus, ReportSummary, ResolvedGroup, SideIndex, TextReportOptions,
    confidence as confidence_name, issue_kind_name, lowercase_hex, percentage, side_name, yes_no,
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
        "content changes: {} · formatting-only: {} · uncertain: {} · unresolved regions: {} · coverage {}",
        summary.content_changes,
        summary.formatting_only_changes,
        summary.uncertain_changes,
        summary.unresolved_regions,
        percentage(summary.comparison_coverage),
    )
    .map_err(|error| Error::Report(error.to_string()))?;
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

/// Renders one presentation hunk body. All old-side spans of the cluster
/// share one block group and all new-side spans share one block group (the
/// clustering guarantees it), so their edited ranges merge into contiguous
/// runs and render as adjacent `-` / `+` lines instead of one window per
/// scalar-level exact change.
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
        let old_pages = move_change
            .old_span
            .as_ref()
            .map(|span| resolve_window(old, span))
            .transpose()?
            .map(|window| window.pages)
            .unwrap_or_default();
        let new_pages = move_change
            .new_span
            .as_ref()
            .map(|span| resolve_window(new, span))
            .transpose()?
            .map(|window| window.pages)
            .unwrap_or_default();
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
        append_marked_spans(
            move_change.old_span.as_ref(),
            old,
            MINUS_STYLE,
            pages,
            body,
            painter,
        )?;
        append_marked_spans(
            move_change.new_span.as_ref(),
            new,
            PLUS_STYLE,
            pages,
            body,
            painter,
        )?;
        return Ok(());
    }

    let old_spans = cluster
        .changes
        .iter()
        .filter_map(|change| change.old_span.as_ref())
        .collect::<Vec<_>>();
    let new_spans = cluster
        .changes
        .iter()
        .filter_map(|change| change.new_span.as_ref())
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
    let Some(template) = spans.first() else {
        return Ok(());
    };
    // Every span in the cluster shares the same block list and separator on
    // this side, so one resolution serves all runs — but every participating
    // span must pass the same fail-loud bounds contract before merging, not
    // just the template, or a later out-of-range span would silently clamp.
    let group = index.resolve_group(&template.blocks, template.separator)?;
    for span in spans {
        validate_span_against_group(span, &group)?;
    }
    for (start, end) in merged_edited_ranges(spans.iter().copied()) {
        let window = bounded_window(&group, start, end);
        pages.extend_from_slice(&window.pages);
        body.push(window.render_marked(style, painter));
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

struct Cluster<'a> {
    changes: Vec<&'a Change>,
    last_old: Option<&'a TextSpan>,
    last_new: Option<&'a TextSpan>,
    confidence: Confidence,
    tags: Vec<ChangeTag>,
}

impl<'a> Cluster<'a> {
    fn starting(change: &'a Change) -> Self {
        Self {
            changes: vec![change],
            last_old: change.old_span.as_ref(),
            last_new: change.new_span.as_ref(),
            confidence: change.confidence,
            tags: change.tags.clone(),
        }
    }

    fn can_absorb(&self, next: &Change) -> bool {
        if next.kind == ChangeKind::Move
            || self
                .changes
                .first()
                .is_some_and(|first| first.kind == ChangeKind::Move)
        {
            // A move is a standalone relocation record; its marker line must
            // stay attached to exactly one -/+ pair.
            return false;
        }
        if next.confidence != self.confidence || next.tags != self.tags {
            return false;
        }
        side_close(self.last_old, next.old_span.as_ref())
            && side_close(self.last_new, next.new_span.as_ref())
    }

    fn absorb(&mut self, next: &'a Change) {
        if next.old_span.is_some() {
            self.last_old = next.old_span.as_ref();
        }
        if next.new_span.as_ref().is_some() {
            self.last_new = next.new_span.as_ref();
        }
        self.changes.push(next);
    }

    fn header(&self, pages: &[u32]) -> String {
        let old_blocks = self
            .changes
            .iter()
            .find_map(|change| change.old_span.as_ref());
        let new_blocks = self
            .changes
            .iter()
            .find_map(|change| change.new_span.as_ref());
        let mut parts = vec![format_pages(pages)];
        match (old_blocks, new_blocks) {
            (Some(old), Some(new)) => parts.push(format!(
                "old {} -> new {}",
                blocks_label(&old.blocks),
                blocks_label(&new.blocks),
            )),
            (Some(old), None) => parts.push(format!("old {}", blocks_label(&old.blocks))),
            (None, Some(new)) => parts.push(format!("new {}", blocks_label(&new.blocks))),
            (None, None) => {}
        }
        parts.push(format!("confidence: {}", confidence_name(self.confidence),));
        format!("@@ {} @@", parts.join(" · "))
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
