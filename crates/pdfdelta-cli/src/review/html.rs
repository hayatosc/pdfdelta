//! HTML presentation of existing results. No matching, normalization or ownership
//! decisions are made here; source outlines and excerpts are review context.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
};

use pdfdelta_core::{
    document::{
        ChangedToken, DocumentView, DocumentViewComparison, FieldValue, GraphNode,
        InterpretationStatus, KeyedElementOperationKind, LocalViewComparison, NodeContent, NodeId,
        SourceRef, TypedOperation,
    },
    model::{PageId, Rect, Vec2},
};
use serde::Serialize;

use super::Input;

const MAX_SOURCE_VISITS: usize = 20_000_000;

fn charge(remaining: &mut usize, count: usize) -> io::Result<()> {
    *remaining = remaining
        .checked_sub(count)
        .ok_or_else(|| io::Error::other("static review source-reference budget exhausted"))?;
    Ok(())
}

fn escaped(out: &mut dyn Write, value: &str) -> io::Result<()> {
    Escaped(out).write_all(value.as_bytes())
}

struct Escaped<'a>(&'a mut dyn Write);

impl Write for Escaped<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut start = 0;
        for (index, byte) in bytes.iter().enumerate() {
            let replacement: &[u8] = match byte {
                b'&' => b"&amp;",
                b'<' => b"&lt;",
                b'>' => b"&gt;",
                b'\"' => b"&quot;",
                b'\'' => b"&#39;",
                _ => continue,
            };
            self.0.write_all(&bytes[start..index])?;
            self.0.write_all(replacement)?;
            start = index + 1;
        }
        self.0.write_all(&bytes[start..])?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn json(out: &mut dyn Write, title: &str, value: &impl Serialize) -> io::Result<()> {
    write!(out, "<details><summary>")?;
    escaped(out, title)?;
    write!(out, "</summary><pre tabindex=\"0\"><code>")?;
    serde_json::to_writer_pretty(Escaped(out), value).map_err(io::Error::other)?;
    write!(out, "</code></pre></details>")
}

fn reasons(out: &mut dyn Write, values: &[String]) -> io::Result<()> {
    if !values.is_empty() {
        write!(out, "<ul>")?;
        for value in values {
            write!(out, "<li>")?;
            escaped(out, value)?;
            write!(out, "</li>")?;
        }
        write!(out, "</ul>")?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Category {
    A,
    B,
    C,
    Formatting,
    Unresolved,
}

impl Category {
    fn label(self) -> (&'static str, &'static str) {
        match self {
            Self::A => ("a", "A — strict change"),
            Self::B => ("b", "B — corresponding-range content change"),
            Self::C => ("c", "C — inferred comparison"),
            Self::Formatting => ("f", "F — formatting-only difference"),
            Self::Unresolved => ("u", "Unresolved comparison"),
        }
    }
}

fn category(pair: &LocalViewComparison) -> Category {
    if pair.interpretation == InterpretationStatus::Inferred {
        return Category::C;
    }
    if !pair.compared || pair.operation.is_none() {
        return Category::Unresolved;
    }
    if matches!(
        pair.operation,
        Some(
            TypedOperation::TextChanged { .. }
                | TypedOperation::ValueChanged {
                    old: FieldValue::Text(_),
                    new: FieldValue::Text(_)
                }
        )
    ) && !pair
        .text_mask
        .as_ref()
        .is_some_and(|mask| !mask.old.is_empty() || !mask.new.is_empty())
    {
        Category::B
    } else {
        Category::A
    }
}

type Location = (Option<PageId>, Option<Rect>);

struct Side<'a> {
    name: &'static str,
    view: DocumentView<'a>,
    nodes: BTreeMap<NodeId, &'a GraphNode>,
    locations: BTreeMap<SourceRef, Location>,
    widget_locations: BTreeMap<SourceRef, Vec<Location>>,
    previews: BTreeMap<PageId, Vec<&'a pdfdelta_core::document::RenderedEvidence>>,
    /// Bundle artifact holding this side's complete source index.
    sources_name: &'static str,
}

fn bounds(points: impl IntoIterator<Item = Vec2>) -> Option<Rect> {
    let mut points = points.into_iter();
    let first = points.next()?;
    Some(points.fold(
        Rect {
            min: first,
            max: first,
        },
        |mut rect, point| {
            rect.min.x = rect.min.x.min(point.x);
            rect.min.y = rect.min.y.min(point.y);
            rect.max.x = rect.max.x.max(point.x);
            rect.max.y = rect.max.y.max(point.y);
            rect
        },
    ))
}

impl<'a> Side<'a> {
    fn new(name: &'static str, view: DocumentView<'a>, remaining: &mut usize) -> io::Result<Self> {
        let store = view.evidence;
        charge(
            remaining,
            view.graph.nodes.len()
                + store.native.items().len()
                + store.native.vector_lines().len()
                + store.rendered.len()
                + store.structured.len(),
        )?;
        let mut locations = BTreeMap::new();
        let mut widget_locations = BTreeMap::new();
        let mut previews: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for glyph in store.native.items() {
            locations.insert(
                SourceRef::Native { glyph: glyph.id },
                (Some(glyph.page), Some(glyph.bbox)),
            );
        }
        for line in store.native.vector_lines() {
            locations.insert(
                SourceRef::NativeVector { line: line.id },
                (Some(line.page), bounds([line.from, line.to])),
            );
        }
        for region in &store.rendered {
            charge(remaining, region.polygon.len())?;
            locations.insert(
                SourceRef::Rendered { region: region.id },
                (Some(region.page), bounds(region.polygon.iter().copied())),
            );
            if region.composited_page {
                previews.entry(region.page).or_default().push(region);
            }
        }
        for element in &store.structured {
            locations.insert(
                SourceRef::Structured {
                    element: element.id,
                },
                (element.page, element.bounds),
            );
            if let pdfdelta_core::document::StructuredValue::FormField { widgets, .. } =
                &element.value
                && !widgets.is_empty()
            {
                charge(remaining, widgets.len())?;
                widget_locations.insert(
                    SourceRef::Structured {
                        element: element.id,
                    },
                    widgets
                        .iter()
                        .map(|widget| (widget.page, widget.bounds))
                        .collect(),
                );
            }
        }
        Ok(Self {
            name,
            view,
            nodes: view
                .graph
                .nodes
                .iter()
                .map(|node| (node.id, node))
                .collect(),
            locations,
            widget_locations,
            previews,
            sources_name: "sources.json.gz",
        })
    }

    fn sources(&self, ids: &[NodeId], remaining: &mut usize) -> io::Result<Vec<SourceRef>> {
        let mut sources = BTreeSet::new();
        charge(remaining, ids.len())?;
        for id in ids {
            let node = self
                .nodes
                .get(id)
                .ok_or_else(|| io::Error::other("review node is absent from the retained graph"))?;
            charge(remaining, node.sources.len())?;
            sources.extend(node.sources.iter().copied());
        }
        Ok(sources.into_iter().collect())
    }

    fn locators(
        &self,
        out: &mut dyn Write,
        sources: &[SourceRef],
        remaining: &mut usize,
    ) -> io::Result<()> {
        charge(remaining, sources.len())?;
        let mut pages: BTreeMap<PageId, Option<Rect>> = BTreeMap::new();
        let mut unlocated = 0usize;
        let mut unknown_bounds = 0usize;
        for source in sources {
            let locations = self
                .widget_locations
                .get(source)
                .map(Vec::as_slice)
                .or_else(|| self.locations.get(source).map(std::slice::from_ref))
                .unwrap_or(&[]);
            charge(remaining, locations.len())?;
            if locations.is_empty() {
                unlocated += 1;
            }
            for location in locations {
                match location {
                    (Some(page), rect) => {
                        let extent = pages.entry(*page).or_default();
                        if let Some(rect) = rect {
                            *extent = Some(match extent {
                                Some(previous) => {
                                    bounds([previous.min, previous.max, rect.min, rect.max])
                                        .expect("nonempty source bounds")
                                }
                                None => *rect,
                            });
                        } else {
                            unknown_bounds += 1;
                        }
                    }
                    _ => unlocated += 1,
                }
            }
        }
        for (page, rect) in pages {
            let number = u64::from(page.0) + 1;
            write!(
                out,
                "<p class=\"locator\"><a href=\"{}.pdf#page={number}\">Open {} PDF, page {number}</a>",
                self.name, self.name
            )?;
            if let Some(rect) = rect {
                write!(
                    out,
                    " · source extent ({:.3}, {:.3})–({:.3}, {:.3}) PDF units",
                    rect.min.x, rect.min.y, rect.max.x, rect.max.y
                )?;
            } else {
                write!(out, " · exact region unavailable")?;
            }
            write!(out, "</p>")?;
            for region in self.previews.get(&page).into_iter().flatten() {
                write!(
                    out,
                    "<details><summary>Show {} page {number} preview</summary><figure><img src=\"{}-region-{}.png\" width=\"{}\" height=\"{}\" loading=\"lazy\" alt=\"{} PDF page {number}; full-page source context\"><figcaption>Retained page rendering, region {}. This preview is context, not a changed mask. Render profile and warnings are in {}.</figcaption></figure></details>",
                    self.name,
                    self.name,
                    region.id,
                    region.raster.width,
                    region.raster.height,
                    self.name,
                    region.id,
                    self.sources_name
                )?;
            }
        }
        if unlocated > 0 {
            write!(
                out,
                "<p>{unlocated} source locations have no retained page locator; inspect their object/evidence IDs in {}.",
                self.sources_name
            )?;
        }
        if unknown_bounds > 0 {
            write!(
                out,
                "<p>{unknown_bounds} source locations lack exact bounds; the listed extents cover only located geometry.</p>"
            )?;
        }
        json(
            out,
            "Complete source references (context, not a changed mask)",
            &sources,
        )
    }

    fn excerpt(
        &self,
        out: &mut dyn Write,
        ids: &[NodeId],
        mask: &[ChangedToken],
        conditional: bool,
    ) -> io::Result<()> {
        let positions: BTreeSet<_> = mask.iter().map(|token| token.position).collect();
        write!(out, "<blockquote class=\"excerpt\">")?;
        let mut position = 0;
        for id in ids {
            let node = self
                .nodes
                .get(id)
                .ok_or_else(|| io::Error::other("missing excerpt node"))?;
            match &node.content {
                NodeContent::Text { view } => {
                    for token in &view.tokens {
                        scalar(
                            out,
                            token.as_scalar(),
                            positions.contains(&position),
                            conditional,
                        )?;
                        position += 1;
                    }
                }
                NodeContent::Value {
                    value: FieldValue::Text(text),
                } => {
                    for ch in text.chars() {
                        scalar(out, Some(ch), positions.contains(&position), conditional)?;
                        position += 1;
                    }
                }
                NodeContent::Value { value } => {
                    serde_json::to_writer(Escaped(out), value).map_err(io::Error::other)?;
                }
                NodeContent::Visual { .. } => write!(
                    out,
                    "Visual region; open its original page or preview below."
                )?,
                _ => write!(out, "No text excerpt is available for this node.")?,
            }
        }
        if ids.is_empty() {
            write!(out, "No counterpart node is asserted on this side.")?;
        }
        write!(out, "</blockquote>")
    }
}

fn scalar(
    out: &mut dyn Write,
    ch: Option<char>,
    marked: bool,
    conditional: bool,
) -> io::Result<()> {
    if marked {
        write!(
            out,
            "<mark{}>",
            if conditional {
                " class=\"conditional\""
            } else {
                ""
            }
        )?;
    }
    match ch {
        None => write!(out, "[unmapped token]")?,
        Some(ch)
            if (ch.is_control() && ch != '\n' && ch != '\t')
                || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{ffffd}' | '\u{100000}'..='\u{10fffd}') =>
        {
            escaped(out, &ch.escape_unicode().to_string())?;
        }
        Some(ch) => escaped(out, ch.encode_utf8(&mut [0; 4]))?,
    }
    if marked {
        write!(out, "</mark>")?;
    }
    Ok(())
}

fn header(out: &mut dyn Write, id: &str, pointer: &str, category: Category) -> io::Result<()> {
    let (class, label) = category.label();
    write!(
        out,
        "<article id=\"{id}\"><h3 class=\"category category-{class}\">{label}</h3><p class=\"locator\">Evidence ID: <a href=\"#{id}\">{id}</a> · comparison.json pointer: <code>{pointer}</code></p>"
    )
}

fn pair(
    out: &mut dyn Write,
    result: &LocalViewComparison,
    old: &Side<'_>,
    new: &Side<'_>,
    non_owning: bool,
    remaining: &mut usize,
) -> io::Result<()> {
    let conditional = non_owning || result.interpretation == InterpretationStatus::Inferred;
    write!(
        out,
        "<p class=\"legend\">{} Unmarked text is context. Glyph references and token positions are different units.</p>",
        if conditional {
            "Dashed underlining shows a conditional local mask; it adds no strict ownership."
        } else {
            "Solid underlining shows only the exact source-backed token positions retained by the comparison; other changed positions may remain unresolved."
        }
    )?;
    write!(out, "<div class=\"sides\">")?;
    let text_mask = result.text_mask.as_ref();
    let old_mask = text_mask.map_or(&[][..], |mask| mask.old.as_slice());
    let new_mask = text_mask.map_or(&[][..], |mask| mask.new.as_slice());
    for (side, ids, mask) in [(old, &result.old, old_mask), (new, &result.new, new_mask)] {
        write!(
            out,
            "<section><h4>{}</h4>",
            if side.name == "old" { "Old" } else { "New" }
        )?;
        side.excerpt(out, ids, mask, conditional)?;
        let sources = side.sources(ids, remaining)?;
        side.locators(out, &sources, remaining)?;
        write!(out, "</section>")?;
    }
    write!(out, "</div>")?;
    reasons(out, &result.unresolved)?;
    json(
        out,
        "Local operation, exact masks and interpretation",
        result,
    )
}

pub(super) fn write(
    out: &mut dyn Write,
    comparison: &DocumentViewComparison,
    images: Option<&crate::evidence_compare::ImageReport>,
    complete: bool,
    flow_review: &super::FlowReview<'_>,
    old_input: &Input<'_>,
    new_input: &Input<'_>,
) -> io::Result<()> {
    let mut remaining = MAX_SOURCE_VISITS;
    let mut old = Side::new("old", old_input.view, &mut remaining)?;
    let mut new = Side::new("new", new_input.view, &mut remaining)?;
    if flow_review.flow.is_some() {
        old.sources_name = "sources.json.gz";
        new.sources_name = "sources.json.gz";
    }
    write!(
        out,
        "<!DOCTYPE html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; base-uri 'none'; form-action 'none'\"><title>PDF source review</title><style>{}</style><body><a href=\"#changes\">Skip to changes</a><header><h1>PDF source review</h1><p>Comparison: <strong>{}</strong>. Review context does not alter strict coverage or the comparison exit code.</p><p>Old: ",
        include_str!("style.css"),
        if complete { "complete" } else { "incomplete" }
    )?;
    escaped(out, &old_input.name.display().to_string())?;
    write!(out, "<br>New: ")?;
    escaped(out, &new_input.name.display().to_string())?;
    write!(
        out,
        "</p><nav><ul><li><a href=\"#changes\">All reported changes</a></li><li><a href=\"#uncertainty\">Search and acquisition</a></li><li><a href=\"#sources\">Original files and evidence</a></li></ul></nav></header><main><section id=\"changes\"><h2>Changes and comparison proposals</h2><p>A establishes the shown change or source positions under accepted correspondence. B reports content of a corresponding range without assigning every internal edit. C retains an inferred counterpart. These categories are not added together as strict recall.</p><p>Full excerpts retain whitespace. Control and private-use characters are shown as Unicode escapes so mapping differences remain visible; a literal representation change need not change the visible words in the PDF.</p>"
    )?;
    let mut displayed = 0usize;
    if let Some(images) = images {
        for (index, change) in images.comparison.changes.iter().enumerate() {
            displayed += 1;
            header(
                out,
                &format!("image-change-{index}"),
                &format!("/image_diff/comparison/changes/{index}"),
                Category::C,
            )?;
            write!(
                out,
                "<p>Image {:?}. Decoded pixel hashes are compared; replacement correspondence is inferred from placement. No OCR or pixel mask is used.</p><div class=\"sides\">",
                change.kind
            )?;
            for (side, inventory, selected) in [
                ("old", &images.old, change.old),
                ("new", &images.new, change.new),
            ] {
                write!(out, "<section><h4>{side}</h4>")?;
                if let Some(i) = selected {
                    let image = &inventory.images[i];
                    write!(
                        out,
                        "<p><a href=\"{side}.pdf#page={}\">Page {}, image {}</a> ({} × {} pixels)</p>",
                        image.page.0 + 1,
                        image.page.0 + 1,
                        image.occurrence + 1,
                        image.width,
                        image.height
                    )?;
                    json(out, "Image hash and placement", image)?;
                } else {
                    write!(out, "<p>Absent from the acquired image inventory.</p>")?;
                }
                write!(out, "</section>")?;
            }
            write!(out, "</div></article>")?;
        }
        json(
            out,
            "Image inventory, unchanged counts and unresolved occurrences",
            images,
        )?;
    }
    for (scope_index, scope) in comparison.scopes.iter().enumerate() {
        for (index, result) in scope.result.comparisons.iter().enumerate() {
            if result.operation.is_none() && result.compared && result.unresolved.is_empty() {
                continue;
            }
            displayed += 1;
            header(
                out,
                &format!("s{scope_index}-p{index}"),
                &format!("/comparison/scopes/{scope_index}/result/comparisons/{index}"),
                category(result),
            )?;
            pair(out, result, &old, &new, false, &mut remaining)?;
            write!(
                out,
                "<p><a href=\"#scope-{scope_index}\">Parent and search dependencies</a></p></article>"
            )?;
        }
        for (index, review) in scope.result.text_scope_reviews.iter().enumerate() {
            displayed += 1;
            header(
                out,
                &format!("s{scope_index}-r{index}"),
                &format!("/comparison/scopes/{scope_index}/result/text_scope_reviews/{index}"),
                if review.comparison.interpretation == InterpretationStatus::Inferred {
                    Category::C
                } else {
                    Category::B
                },
            )?;
            write!(out, "<p>Non-owning range review. Convention: <code>")?;
            escaped(out, &review.convention)?;
            write!(
                out,
                "</code>. Interior candidate enumeration: {}.</p>",
                match review.candidate_search_exhaustive {
                    Some(true) => "complete",
                    Some(false) => "incomplete",
                    None => "unknown",
                }
            )?;
            pair(out, &review.comparison, &old, &new, true, &mut remaining)?;
            json(
                out,
                "Boundary proposals and complete retained range references",
                review,
            )?;
            write!(
                out,
                "<p><a href=\"#scope-{scope_index}\">Parent and search dependencies</a></p></article>"
            )?;
        }
    }
    for (index, relation) in comparison
        .relations
        .iter()
        .enumerate()
        .filter(|(_, relation)| relation.changed())
    {
        displayed += 1;
        header(
            out,
            &format!("relation-{index}"),
            &format!("/comparison/relations/{index}"),
            if relation.interpretation == InterpretationStatus::Inferred {
                Category::C
            } else {
                Category::A
            },
        )?;
        write!(
            out,
            "<p>Relationship {:?}: old present {}, new present {}. This is a relationship claim, not a character mask.</p>",
            relation.kind, relation.old_present, relation.new_present
        )?;
        for (side, endpoints, sources) in [
            (&old, &relation.old, &relation.old_sources),
            (&new, &relation.new, &relation.new_sources),
        ] {
            write!(out, "<h4>{}</h4>", side.name)?;
            let ids: Vec<_> = endpoints.iter().flatten().copied().collect();
            let mut context = side.sources(&ids, &mut remaining)?;
            context.extend(sources);
            side.locators(out, &context, &mut remaining)?;
        }
        json(out, "Relation evidence and parent premises", relation)?;
        write!(out, "</article>")?;
    }
    if let Some(keys) = &comparison.key_presence {
        for (scope, scoped) in keys.scoped.iter().enumerate() {
            for (index, operation) in scoped.operations.iter().enumerate() {
                displayed += 1;
                header(
                    out,
                    &format!("key-{scope}-{index}"),
                    &format!("/comparison/key_presence/scoped/operations/{index}"),
                    Category::A,
                )?;
                write!(
                    out,
                    "<p>Native key membership: {:?}. Only identity evidence is owned; this does not assert that all displayed content was inserted or deleted.</p>",
                    operation.kind
                )?;
                let side = if operation.kind == KeyedElementOperationKind::Removed {
                    &old
                } else {
                    &new
                };
                side.excerpt(out, &[operation.node], &[], false)?;
                side.locators(out, &operation.review_sources, &mut remaining)?;
                json(out, "Membership operation and identity source", operation)?;
                write!(out, "</article>")?;
            }
        }
    }
    if displayed == 0 {
        write!(
            out,
            "<p class=\"empty\">No change or local comparison proposal was reported. Incomplete acquisition or search can still prevent a conclusion.</p>"
        )?;
    }
    write!(out, "</section>")?;
    if flow_review.flow.is_some() || flow_review.verdicts.is_some() {
        flow_section(
            out,
            flow_review.flow,
            flow_review.verdicts,
            &old,
            &new,
            &mut remaining,
        )?;
    }
    write!(
        out,
        "<section id=\"uncertainty\"><h2>Search, scope and acquisition dependencies</h2>"
    )?;
    for (index, scope) in comparison.scopes.iter().enumerate() {
        write!(
            out,
            "<section id=\"scope-{index}\"><h3>Scope {index}</h3><p>Parent {:?}; interpretation {:?}; candidate enumeration {}.</p>",
            scope.parent,
            scope.interpretation,
            if scope.result.candidates.exhaustive {
                "complete"
            } else {
                "incomplete"
            }
        )?;
        reasons(out, &scope.result.unresolved)?;
        json(
            out,
            "Extraction dependencies",
            &scope.result.extraction_dependencies,
        )?;
        json(
            out,
            "Counterpart decisions and unresolved histories",
            &scope.result.counterpart_decisions,
        )?;
        write!(
            out,
            "<p>All candidates, optimization states and accepted boundary indexes: <a href=\"comparison.json\">comparison.json</a>, pointer <code>/comparison/scopes/{index}/result</code>.</p></section>"
        )?;
    }
    reasons(out, &comparison.relation_unresolved)?;
    json(
        out,
        "Raw and scoped key presence, including unresolved renames or movement",
        &comparison.key_presence,
    )?;
    for side in [&old, &new] {
        write!(out, "<h3>{} acquisition</h3>", side.name)?;
        json(
            out,
            "Acquisition issues and gaps",
            &side.view.evidence.issues,
        )?;
    }
    let sources = "sources.json.gz";
    let compressed = " (gzip-compressed, lossless)";
    write!(
        out,
        "</section><section id=\"sources\"><h2>Original files and evidence</h2><ul><li><a href=\"old.pdf\">Old source PDF</a></li><li><a href=\"new.pdf\">New source PDF</a></li><li><a href=\"comparison.json\">Complete comparison JSON</a></li><li><a href=\"{sources}\">Source IDs, glyph geometry, raw codes, provenance, graph, profiles and warnings{compressed}</a></li><li><a href=\"manifest.json\">Artifact hashes and byte counts</a></li></ul><p>PDF page links use one-based page indexes; printed page labels may differ. Preview pixels come from the retained renderer profile, not a new comparison. Missing previews do not imply an empty page. The full PDFs remain available. Exported normalization metadata must be rebuilt from original evidence before reuse as comparison evidence.</p></section></main></body></html>"
    )
}

/// Flow text evidence and the per-channel verdicts that selected the exit.
///
/// Flow text changes have no local comparison entry in
/// `comparison.scopes`. They link to the report's own
/// `flow_text.changes` array, and every displayed token range names the source
/// pages and retained token geometry it came from.
fn flow_section(
    out: &mut dyn Write,
    flow: Option<&crate::flow_cmd::FlowComparison>,
    verdicts: Option<&[crate::flow_exit::ChannelVerdict]>,
    old: &Side<'_>,
    new: &Side<'_>,
    remaining: &mut usize,
) -> io::Result<()> {
    write!(
        out,
        "<section id=\"flow-text\"><h2>Flow text evidence and channel verdicts</h2>\
         <p>The flow engine supplies the text channel. Flow categories are A for a delete, insert, or high-confidence replace with exact token ranges; B for a replace whose span is coarse, so the range is corresponding but its interior edit is not localized; C for a move or a change carrying an ambiguity signal; and F for a formatting-only difference on matched tokens. Flow has no separate unresolved text-comparison category: text evidence that was not examined or cannot be trusted is listed with its own reason record below.</p>"
    )?;
    if let Some(verdicts) = verdicts {
        write!(out, "<h3>Channel verdicts</h3><ul class=\"verdicts\">")?;
        for (index, verdict) in verdicts.iter().enumerate() {
            write!(
                out,
                "<li id=\"verdict-{index}\"><strong>{}</strong>: detected {}, broken {}, unexamined {}. JSON pointer <code>/channel_verdicts/{index}</code>",
                verdict.channel.label(),
                verdict.detected,
                verdict.broken,
                verdict.unexamined
            )?;
            if verdict.channel == crate::flow_exit::ChannelClass::Text {
                let issues = crate::flow_review::issues_json(
                    flow.expect("a text verdict always has a flow comparison"),
                );
                if issues.is_empty() {
                    write!(
                        out,
                        " · no text extraction issue was retained; status in <code>/flow_text/summary</code>"
                    )?;
                } else {
                    write!(out, " · reason records: ")?;
                    for (issue, _) in issues.iter().enumerate() {
                        if issue > 0 {
                            write!(out, ", ")?;
                        }
                        write!(
                            out,
                            "<a href=\"comparison.json\">/flow_text/issues/{issue}</a>"
                        )?;
                    }
                }
            } else {
                write!(
                    out,
                    " · typed reasons are in the scope and acquisition sections below and under <code>/comparison</code>"
                )?;
            }
            write!(out, "</li>")?;
        }
        write!(out, "</ul>")?;
    }
    let Some(flow) = flow else {
        return write!(out, "</section>");
    };
    let issues = crate::flow_review::issues_json(flow);
    if !issues.is_empty() {
        write!(out, "<h3>Text extraction issues</h3><ul>")?;
        for (index, issue) in issues.iter().enumerate() {
            write!(
                out,
                "<li><a href=\"comparison.json\">/flow_text/issues/{index}</a> · side {} · {} · {}",
                issue["side"].as_str().unwrap_or("unknown"),
                issue["kind"].as_str().unwrap_or("unknown"),
                if issue["unexamined"].as_bool().unwrap_or(false) {
                    "unexamined"
                } else {
                    "broken"
                }
            )?;
            if let Some(page) = issue["page"].as_u64() {
                write!(out, " · page {page}")?;
            }
            write!(out, " · ")?;
            escaped(out, issue["message"].as_str().unwrap_or(""))?;
            write!(out, "</li>")?;
        }
        write!(out, "</ul>")?;
    }
    let order = crate::flow_review::order_json(flow);
    let mut uncertain = Vec::new();
    for side_name in ["old", "new"] {
        for (index, entry) in order[side_name]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            if entry["uncertain"].as_bool().unwrap_or(false) {
                uncertain.push((side_name, index, entry));
            }
        }
    }
    if !uncertain.is_empty() {
        write!(out, "<h3>Reading-order evidence</h3><ul>")?;
        for (side, index, entry) in uncertain {
            write!(
                out,
                "<li><a href=\"comparison.json\">/flow_text/order/{side}/{index}</a> · {side} page {} · layout and paint order disagree (agreement {:.3}, coherence {:.3})</li>",
                entry["page"].as_u64().unwrap_or(0),
                entry["agreement"].as_f64().unwrap_or(0.0),
                entry["coherence"].as_f64().unwrap_or(0.0)
            )?;
        }
        write!(out, "</ul>")?;
    }
    let mut report_index = 0usize;
    for (index, change) in flow.diff.changes.iter().enumerate() {
        use pdfdelta_diff::Change;
        if matches!(
            change,
            Change::LayoutOnly { .. } | Change::ReadingOrder { .. }
        ) {
            continue;
        }
        let assessment = flow.assessments.get(index);
        let category = flow_change_category(change, assessment);
        header(
            out,
            &format!("flow-change-{report_index}"),
            &format!("/flow_text/changes/{report_index}"),
            category,
        )?;
        let (kind, old_range, new_range) = match change {
            Change::Delete { old } => ("delete", old.clone(), 0..0),
            Change::Insert { new } => ("insert", 0..0, new.clone()),
            Change::Replace { old, new } => ("replace", old.clone(), new.clone()),
            Change::Move { old, new } => ("move", old.clone(), new.clone()),
            Change::ReadingOrder { old, new } => ("reading-order", old.clone(), new.clone()),
            Change::LayoutOnly { old, new } => ("layout-only", old.clone(), new.clone()),
        };
        write!(
            out,
            "<p>{} tokens old, {} tokens new",
            old_range.len(),
            new_range.len()
        )?;
        if let Some(assessment) = assessment {
            write!(out, " · confidence {}", assessment.confidence.label())?;
            if !assessment.ambiguity.is_empty() {
                write!(
                    out,
                    " · ambiguity {}",
                    assessment
                        .ambiguity
                        .iter()
                        .map(|reason| reason.label())
                        .collect::<Vec<_>>()
                        .join(", ")
                )?;
            }
        }
        write!(out, " · kind {kind}</p><div class=\"sides\">")?;
        flow_side(
            out,
            "old",
            &flow.old_side.stream,
            &old_range,
            old,
            remaining,
        )?;
        flow_side(
            out,
            "new",
            &flow.new_side.stream,
            &new_range,
            new,
            remaining,
        )?;
        write!(out, "</div></article>")?;
        report_index += 1;
    }
    for (index, formatting) in flow.formatting.iter().enumerate() {
        header(
            out,
            &format!("flow-formatting-{index}"),
            &format!("/flow_text/formatting_only_changes/{index}"),
            Category::Formatting,
        )?;
        write!(
            out,
            "<p>Matched tokens differ in {}. No content change is asserted.</p><div class=\"sides\">",
            formatting
                .reasons
                .iter()
                .map(|reason| reason.label())
                .collect::<Vec<_>>()
                .join(", ")
        )?;
        flow_side(
            out,
            "old",
            &flow.old_side.stream,
            &formatting.old,
            old,
            remaining,
        )?;
        flow_side(
            out,
            "new",
            &flow.new_side.stream,
            &formatting.new,
            new,
            remaining,
        )?;
        write!(out, "</div></article>")?;
    }
    if report_index == 0 && flow.formatting.is_empty() {
        write!(
            out,
            "<p class=\"empty\">Flow reported no content or formatting change.</p>"
        )?;
    }
    write!(
        out,
        "<p>Flow's summary is at <code>/flow_text/summary</code> and its exact token ranges are in <code>/flow_text/changes</code>. Per-glyph provenance is available from a <code>--full-json</code> run or an agent review bundle, which retain token sources.</p></section>"
    )
}

fn flow_change_category(
    change: &pdfdelta_diff::Change,
    assessment: Option<&pdfdelta_diff::ChangeAssessment>,
) -> Category {
    use pdfdelta_diff::{Change, Confidence};
    if matches!(change, Change::LayoutOnly { .. }) {
        return Category::Unresolved;
    }
    let coarse = assessment.is_some_and(|assessment| {
        assessment
            .ambiguity
            .contains(&pdfdelta_diff::AmbiguityReason::CoarseSpan)
    });
    if coarse {
        return Category::B;
    }
    if matches!(change, Change::Move { .. }) {
        return Category::C;
    }
    match assessment.map(|assessment| assessment.confidence) {
        Some(Confidence::High) => Category::A,
        Some(Confidence::Medium | Confidence::Low) => Category::C,
        None => Category::A,
    }
}

/// One side's flow token range, its source pages, and retained previews.
fn flow_side(
    out: &mut dyn Write,
    name: &'static str,
    stream: &pdfdelta_diff::TokenStream,
    range: &std::ops::Range<usize>,
    side: &Side<'_>,
    remaining: &mut usize,
) -> io::Result<()> {
    charge(remaining, range.len())?;
    write!(out, "<section><h4>{name}</h4>")?;
    if range.is_empty() {
        write!(
            out,
            "<blockquote class=\"excerpt\">No tokens on this side.</blockquote>"
        )?;
        return write!(out, "</section>");
    }
    write!(out, "<blockquote class=\"excerpt\">")?;
    let tokens = &stream.tokens[range.clone()];
    let head = tokens.len().min(40);
    let tail = tokens.len().saturating_sub(80).min(40);
    for (index, token) in tokens[..head].iter().enumerate() {
        if index > 0 {
            write!(out, " ")?;
        }
        escaped(out, &token.text)?;
    }
    if tokens.len() > head + tail {
        write!(out, " … [{} tokens elided] … ", tokens.len() - head - tail)?;
    }
    for (index, token) in tokens[tokens.len() - tail..].iter().enumerate() {
        if index > 0 {
            write!(out, " ")?;
        }
        escaped(out, &token.text)?;
    }
    write!(out, "</blockquote>")?;
    let mut pages: std::collections::BTreeMap<PageId, Option<Rect>> =
        std::collections::BTreeMap::new();
    for token in tokens {
        let entry = pages.entry(token.page).or_insert(None);
        if let Some(rect) = token.bounds {
            *entry = Some(match *entry {
                Some(previous) => bounds([previous.min, previous.max, rect.min, rect.max])
                    .expect("nonempty token bounds"),
                None => rect,
            });
        }
    }
    for (page, rect) in pages {
        let number = u64::from(page.0) + 1;
        write!(
            out,
            "<p class=\"locator\"><a href=\"{name}.pdf#page={number}\">Open {name} PDF, page {number}</a>"
        )?;
        if let Some(rect) = rect {
            write!(
                out,
                " · token extent ({:.3}, {:.3})–({:.3}, {:.3}) PDF units",
                rect.min.x, rect.min.y, rect.max.x, rect.max.y
            )?;
        } else {
            write!(out, " · exact token geometry unavailable")?;
        }
        write!(out, "</p>")?;
        for region in side.previews.get(&page).into_iter().flatten() {
            write!(
                out,
                "<details><summary>Show {name} page {number} preview</summary><figure><img src=\"{name}-region-{}.png\" width=\"{}\" height=\"{}\" loading=\"lazy\" alt=\"{name} PDF page {number}; full-page source context\"><figcaption>Retained page rendering, region {}. This preview is context, not a changed mask.</figcaption></figure></details>",
                region.id, region.raster.width, region.raster.height, region.id
            )?;
        }
    }
    write!(out, "</section>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malicious_markup_and_private_use_remain_literal_text() {
        let mut out = Vec::new();
        escaped(&mut out, "</style><script x='\"'>&日本語").expect("escaped text");
        assert_eq!(
            String::from_utf8(out).expect("UTF-8"),
            "&lt;/style&gt;&lt;script x=&#39;&quot;&#39;&gt;&amp;日本語"
        );
        let mut out = Vec::new();
        scalar(&mut out, Some('\u{f0a3}'), true, true).expect("conditional scalar");
        assert_eq!(
            String::from_utf8(out).expect("UTF-8"),
            "<mark class=\"conditional\">\\u{f0a3}</mark>"
        );
    }
}
