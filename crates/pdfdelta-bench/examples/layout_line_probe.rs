//! Prints reconstructed native lines and their glyph geometry for one PDF.
//!
//! Diagnostic only: it reports raw evidence and layout grouping, and certifies
//! no reading order, text inventory, or comparison result.

use std::{env, fs::File, io::Read, sync::Arc};

use pdfdelta_core::{
    layout::{
        RegionOptions, partition_regions_with_vector_lines, reconstruct_blocks, reconstruct_lines,
    },
    model::{DecodedText, Rect},
    normalize::{NormalizationKind, normalize_blocks},
    pdf::{LopdfParser, ParseLimits, PdfParser},
    pipeline::PipelineOptions,
    source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor},
};

type ProbeResult = Result<(), Box<dyn std::error::Error>>;

fn main() -> ProbeResult {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let Some(path) = args.first() else {
        return Err(
            "usage: layout_line_probe INPUT.pdf [--page N] [--search TEXT] [--bbox x0,y0,x1,y1]"
                .into(),
        );
    };
    let mut page: Option<u32> = None;
    let mut search: Option<String> = None;
    let mut bbox: Option<Rect> = None;
    let mut uncertain_only = false;
    let mut source_bounded_only = false;
    let mut tokens_only = false;
    let mut compare_with: Option<String> = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--page" => {
                page = Some(args.get(index + 1).ok_or("--page needs a value")?.parse()?);
                index += 2;
            }
            "--search" => {
                search = Some(args.get(index + 1).ok_or("--search needs a value")?.clone());
                index += 2;
            }
            "--uncertain" => {
                uncertain_only = true;
                index += 1;
            }
            "--source-bounded" => {
                source_bounded_only = true;
                index += 1;
            }
            "--tokens" => {
                tokens_only = true;
                index += 1;
            }
            "--compare" => {
                compare_with = Some(
                    args.get(index + 1)
                        .ok_or("--compare needs a second PDF")?
                        .clone(),
                );
                index += 2;
            }
            "--bbox" => {
                let value = args.get(index + 1).ok_or("--bbox needs a value")?;
                let parts = value
                    .split(',')
                    .map(str::parse::<f64>)
                    .collect::<Result<Vec<_>, _>>()?;
                if parts.len() != 4 {
                    return Err("--bbox expects x0,y0,x1,y1".into());
                }
                bbox = Some(Rect {
                    min: pdfdelta_core::model::Vec2 {
                        x: parts[0],
                        y: parts[1],
                    },
                    max: pdfdelta_core::model::Vec2 {
                        x: parts[2],
                        y: parts[3],
                    },
                });
                index += 2;
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }

    let limits = ParseLimits::default();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limits.max_input_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_input_bytes {
        return Err("PDF input exceeds the default parser limit".into());
    }
    let pdf = LopdfParser.parse(Arc::from(bytes), limits)?;
    let extraction =
        ContentStreamGlyphExtractor.extract_outcome(pdf.as_ref(), ExtractionLimits::default())?;
    if let Some(second) = compare_with {
        let mut other = Vec::new();
        File::open(&second)?
            .take(limits.max_input_bytes as u64 + 1)
            .read_to_end(&mut other)?;
        if other.len() > limits.max_input_bytes {
            return Err("second PDF input exceeds the default parser limit".into());
        }
        let other_pdf = LopdfParser.parse(Arc::from(other), limits)?;
        let other_extraction = ContentStreamGlyphExtractor
            .extract_outcome(other_pdf.as_ref(), ExtractionLimits::default())?;
        let comparison = pdfdelta_core::pipeline::compare_extraction_outcomes(
            pdfdelta_core::source::ExtractionOutcome::complete(extraction.document().clone()),
            pdfdelta_core::source::ExtractionOutcome::complete(other_extraction.document().clone()),
            PipelineOptions::default(),
        )?;
        println!(
            "{}",
            serde_json::json!({
                "kind": "comparison",
                "old": path,
                "new": second,
                "changes": comparison.comparison.changes.len(),
                "candidates": comparison.comparison.change_candidates.len(),
                "proven_regions": comparison.comparison.proven_changed_regions.len(),
                "unresolved": comparison.comparison.unresolved_regions.len(),
                "complete": comparison.comparison.change_candidates.is_empty()
                    && comparison.comparison.proven_changed_regions.is_empty()
                    && comparison.comparison.unresolved_regions.is_empty(),
                "old_coverage": {
                    "resolved": comparison.comparison.old_coverage.resolved_tokens,
                    "total": comparison.comparison.old_coverage.total_tokens,
                    "ratio": comparison.comparison.old_coverage.ratio,
                },
                "new_coverage": {
                    "resolved": comparison.comparison.new_coverage.resolved_tokens,
                    "total": comparison.comparison.new_coverage.total_tokens,
                    "ratio": comparison.comparison.new_coverage.ratio,
                },
            })
        );
        return Ok(());
    }
    let options = PipelineOptions::default();
    let lines = reconstruct_lines(extraction.document(), options.line)?;
    let blocks = reconstruct_blocks(extraction.document(), &lines, options.block)?;
    let normalized = normalize_blocks(extraction.document(), &lines, &blocks)?;
    if tokens_only {
        for block in &normalized {
            println!(
                "{}",
                serde_json::json!({
                    "kind": "tokens",
                    "block": block.block.0,
                    "tokens": block.canonical.comparable_tokens()
                        .map(|tokens| tokens.iter().map(|token| format!("{token:?}")).collect::<Vec<_>>())
                        .unwrap_or_default(),
                    "count": block
                        .canonical
                        .comparable_tokens()
                        .map_or(0, |tokens| tokens.len()),
                })
            );
        }
        return Ok(());
    }
    if source_bounded_only {
        for block in &normalized {
            let failure = source_bounded_failure(block);
            println!(
                "{}",
                serde_json::json!({
                    "kind": "source_bounded",
                    "block": block.block.0,
                    "role": format!("{:?}", block.role),
                    "text": block.canonical.text,
                    "failure": failure,
                    "source_bounded": failure.is_none(),
                    "canonical_scalars": block.canonical.text.chars().count(),
                    "source_map_entries": block.canonical.source_map.len(),
                    "line_breaks": block.line_breaks.as_ref().map(Vec::len),
                    "page_breaks": block.page_breaks.as_ref().map(Vec::len),
                    "pages": block.pages.len(),
                    "font_signatures": block.font_size_signatures.as_ref().map(Vec::len),
                    "position_signatures": block.position_signatures.as_ref().map(Vec::len),
                    "issues": block.issues.len(),
                    "normalization_events": block.normalization_events.iter().map(|event| format!("{:?}", event.kind)).collect::<Vec<_>>(),
                })
            );
        }
        return Ok(());
    }
    let mut uncertain_lines = std::collections::HashSet::new();
    let mut trusted_lines = std::collections::HashSet::new();
    let mut trusted_intervals = Vec::<(u32, u32, u64)>::new();
    if uncertain_only {
        for page_id in 0..pdf.pages()?.len() {
            let page = pdfdelta_core::model::PageId(page_id as u32);
            let page_lines = lines
                .iter()
                .filter(|line| line.page == page)
                .cloned()
                .collect::<Vec<_>>();
            let page_vector_lines = extraction
                .document()
                .vector_lines()
                .iter()
                .filter(|line| line.page == page)
                .copied()
                .collect::<Vec<_>>();
            let graph = partition_regions_with_vector_lines(
                page,
                &page_lines,
                &page_vector_lines,
                RegionOptions::default(),
            )?;
            let by_id = page_lines
                .iter()
                .map(|line| (line.id, line))
                .collect::<std::collections::HashMap<_, _>>();
            for region in &graph.regions {
                let supported = region
                    .line_ids
                    .iter()
                    .copied()
                    .filter(|line_id| {
                        by_id
                            .get(line_id)
                            .is_some_and(|line| line_is_supported(line))
                    })
                    .collect::<Vec<_>>();
                let trusted = longest_render_monotone_subsequence(&supported, &by_id);
                let trusted_set = trusted
                    .iter()
                    .copied()
                    .collect::<std::collections::HashSet<_>>();
                for line_id in &region.line_ids {
                    if !trusted_set.contains(line_id) {
                        uncertain_lines.insert(*line_id);
                    }
                }
                for line_id in &trusted {
                    trusted_lines.insert(*line_id);
                    let line = by_id[line_id];
                    trusted_intervals.push((
                        *line.render_order.start(),
                        *line.render_order.end(),
                        line.id.0,
                    ));
                }
                println!(
                    "{}",
                    serde_json::json!({
                        "kind": "region",
                        "page": page.0,
                        "region": region.id.0,
                        "bbox": [region.bbox.min.x, region.bbox.min.y, region.bbox.max.x, region.bbox.max.y],
                        "lines": region.line_ids.len(),
                        "supported": supported.len(),
                        "trusted": trusted.len(),
                        "line_order": region.line_ids.iter().map(|id| id.0).collect::<Vec<_>>(),
                    })
                );
            }
        }
    }
    let glyph_by_id = extraction
        .document()
        .items()
        .iter()
        .map(|glyph| (glyph.id, glyph))
        .collect::<std::collections::HashMap<_, _>>();

    for line in &lines {
        if page.is_some_and(|page| line.page.0 != page) {
            continue;
        }

        let glyphs =
            line.glyphs
                .iter()
                .map(|id| {
                    glyph_by_id.get(id).copied().ok_or_else(|| {
                        format!("line {} references missing glyph {}", line.id.0, id.0)
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
        let text = glyphs
            .iter()
            .map(|glyph| match &glyph.text {
                DecodedText::Mapped(value) => value.clone(),
                DecodedText::Unmapped { .. } => "\u{fffd}".to_owned(),
            })
            .collect::<String>();
        if search
            .as_ref()
            .is_some_and(|needle| !text.contains(needle.as_str()))
        {
            continue;
        }
        if bbox.is_some_and(|region| !intersects(&region, &line.bbox)) {
            continue;
        }
        let block = blocks
            .iter()
            .position(|block| block.lines.contains(&line.id));
        let issue = block.and_then(|index| normalized.get(index));
        let mut glyph_rows = glyphs
            .iter()
            .map(|glyph| {
                serde_json::json!({
                    "id": glyph.id.0,
                    "text": match &glyph.text {
                        DecodedText::Mapped(value) => value.clone(),
                        DecodedText::Unmapped { .. } => "\u{fffd}".to_owned(),
                    },
                    "bbox": [glyph.bbox.min.x, glyph.bbox.min.y, glyph.bbox.max.x, glyph.bbox.max.y],
                    "baseline": [glyph.baseline.x, glyph.baseline.y],
                    "font_size": glyph.font_size,
                    "font_id": glyph.font_id.0,
                    "render_order": glyph.render_order,
                    "render_mode": format!("{:?}", glyph.render_mode),
                    "operator_index": glyph.provenance.operator_index,
                    "content_stream": format!("{:?}", glyph.provenance.content_stream),
                })
            })
            .collect::<Vec<_>>();
        glyph_rows.sort_by_key(|row| row["render_order"].as_u64().unwrap_or(0));
        println!(
            "{}",
            serde_json::json!({
                "kind": "line",
                "line": line.id.0,
                "page": line.page.0,
                "supported": line_is_supported(line),
                "trusted": trusted_lines.contains(&line.id),
                "uncertain": uncertain_lines.contains(&line.id),
                "text": text,
                "bbox": [line.bbox.min.x, line.bbox.min.y, line.bbox.max.x, line.bbox.max.y],
                "baseline": [line.baseline.x, line.baseline.y],
                "render_order": [line.render_order.start(), line.render_order.end()],
                "block": block,
                "block_issues": issue.map(|block| format!("{:?}", block.issues)),
                "glyphs": glyph_rows,
            })
        );
    }
    Ok(())
}

fn intersects(left: &Rect, right: &Rect) -> bool {
    left.min.x <= right.max.x
        && right.min.x <= left.max.x
        && left.min.y <= right.max.y
        && right.min.y <= left.max.y
}

fn line_is_supported(line: &pdfdelta_core::layout::Line) -> bool {
    let finite = [
        line.bbox.min.x,
        line.bbox.min.y,
        line.bbox.max.x,
        line.bbox.max.y,
        line.baseline.x,
        line.baseline.y,
        line.direction.x,
        line.direction.y,
    ]
    .iter()
    .all(|value| value.is_finite());
    if !finite {
        return false;
    }
    let length_squared = line.direction.x * line.direction.x + line.direction.y * line.direction.y;
    if length_squared <= f64::EPSILON {
        return false;
    }
    let length = length_squared.sqrt();
    let normalized = pdfdelta_core::model::Vec2 {
        x: line.direction.x / length,
        y: line.direction.y / length,
    };
    normalized.y.abs() <= 1.0e-6
        && normalized.x > 0.0
        && matches!(
            line.text_direction,
            pdfdelta_core::layout::LineTextDirection::LeftToRight
                | pdfdelta_core::layout::LineTextDirection::Neutral
        )
}

fn longest_render_monotone_subsequence(
    line_ids: &[pdfdelta_core::layout::LineId],
    lines: &std::collections::HashMap<pdfdelta_core::layout::LineId, &pdfdelta_core::layout::Line>,
) -> Vec<pdfdelta_core::layout::LineId> {
    if line_ids.is_empty() {
        return Vec::new();
    }
    let mut predecessors = vec![None; line_ids.len()];
    let mut tails = Vec::<(u32, usize)>::new();
    for (current, line_id) in line_ids.iter().enumerate() {
        let line = lines[line_id];
        let start = *line.render_order.start();
        let end = *line.render_order.end();
        let tail_position = tails.partition_point(|(tail_end, _)| *tail_end < start);
        if tail_position > 0 {
            predecessors[current] = Some(tails[tail_position - 1].1);
        }
        if tail_position == tails.len() {
            tails.push((end, current));
        } else if end < tails[tail_position].0 {
            tails[tail_position] = (end, current);
        }
    }
    let mut cursor = tails
        .last()
        .map(|(_, index)| *index)
        .expect("a non-empty line sequence has a longest subsequence");
    let mut trusted = Vec::with_capacity(tails.len());
    loop {
        trusted.push(line_ids[cursor]);
        let Some(previous) = predecessors[cursor] else {
            break;
        };
        cursor = previous;
    }
    trusted.reverse();
    trusted
}

fn source_bounded_failure(block: &pdfdelta_core::normalize::BlockText) -> Option<&'static str> {
    if !block.issues.is_empty() {
        return Some("normalization issue");
    }
    if block
        .normalization_events
        .iter()
        .any(|event| event.kind != NormalizationKind::WhitespaceCollapse)
    {
        return Some("non-whitespace normalization");
    }
    if !block.canonical.unmapped.is_empty() {
        return Some("unmapped canonical token");
    }
    if !block.line_breaks.as_ref().is_some_and(Vec::is_empty) {
        return Some("line breaks");
    }
    if !block.page_breaks.as_ref().is_some_and(Vec::is_empty) {
        return Some("page breaks");
    }
    if block.pages.len() != 1 {
        return Some("multiple pages");
    }
    if block.font_size_signatures.is_none() {
        return Some("missing font-size signatures");
    }
    if block.position_signatures.is_none() {
        return Some("missing position signatures");
    }
    let scalar_count = block.canonical.text.chars().count();
    let mut sources = std::collections::HashSet::new();
    let mut next_start = 0usize;
    for entry in &block.canonical.source_map {
        if entry.output_range.start != next_start {
            return Some("source map gap");
        }
        if entry
            .output_range
            .end
            .saturating_sub(entry.output_range.start)
            != 1
        {
            return Some("source map range is not one scalar");
        }
        if entry.output_range.end > scalar_count {
            return Some("source map range past canonical text");
        }
        if entry.source.atoms.is_empty() {
            return Some("empty source atoms");
        }
        if entry.source.atoms.iter().any(|atom| !sources.insert(atom)) {
            return Some("duplicate source atom");
        }
        next_start = entry.output_range.end;
    }
    if next_start != scalar_count {
        return Some("source map does not cover canonical text");
    }
    None
}
