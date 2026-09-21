use std::collections::{HashMap, HashSet};

use crate::{
    Error, Result,
    alignment::BlockSeparator,
    diff::TextSpan,
    layout::BlockId,
    model::{GlyphEvidence, GlyphId, PageId, Rect},
    normalize::{BlockText, ScalarRange, TextSource, TextSourceAtom},
    pdf::ObjectRef,
};

/// Default resource limits for projecting one report span to source evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanSourceProjectionLimits {
    /// Maximum comparable tokens accumulated across the span's blocks.
    pub max_comparable_tokens: usize,
    /// Maximum items in each evidence index or intermediate/output collection.
    pub max_evidence_items: usize,
}

impl Default for SpanSourceProjectionLimits {
    fn default() -> Self {
        Self {
            max_comparable_tokens: 5_100_000,
            max_evidence_items: 20_400_000,
        }
    }
}

/// Backend-neutral source evidence selected by a [`TextSpan`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpanSourceEvidence {
    Glyph {
        glyph_id: GlyphId,
        page: PageId,
        bbox: Rect,
        content_stream: ObjectRef,
        operator_index: u32,
    },
    SyntheticSpace {
        preceding_glyph_id: GlyphId,
        following_glyph_id: GlyphId,
    },
    LineBreak {
        preceding_glyph_id: GlyphId,
        following_glyph_id: GlyphId,
    },
    BlockSeparatorSpace,
}

/// Projects a normalized text span to its ordered, deduplicated source evidence.
///
/// This validates duplicate block and glyph ids even when the duplicate item is
/// outside the selected span. Use [`project_span_sources_with_limits`] when a
/// caller needs limits lower than the library defaults.
///
/// # Errors
///
/// Returns an error for duplicate or missing evidence, non-finite glyph
/// geometry, inconsistent canonical and comparable ranges, malformed
/// normalization events, or a resource limit.
pub fn project_span_sources(
    blocks: &[BlockText],
    glyph_evidence: &[GlyphEvidence],
    span: &TextSpan,
) -> Result<Vec<SpanSourceEvidence>> {
    project_span_sources_with_limits(
        blocks,
        glyph_evidence,
        span,
        SpanSourceProjectionLimits::default(),
    )
}

/// Projects a normalized text span with explicit resource limits.
///
/// # Errors
///
/// Returns the same errors as [`project_span_sources`], including
/// [`Error::LimitExceeded`] when any configured bound is exceeded.
pub fn project_span_sources_with_limits(
    blocks: &[BlockText],
    glyph_evidence: &[GlyphEvidence],
    span: &TextSpan,
    limits: SpanSourceProjectionLimits,
) -> Result<Vec<SpanSourceEvidence>> {
    if limits.max_comparable_tokens == 0 || limits.max_evidence_items == 0 {
        return Err(Error::InvalidConfiguration(
            "span source projection limits must be greater than zero".to_owned(),
        ));
    }
    let projector = SpanSourceProjector::new(blocks, glyph_evidence, limits)?;
    projector.project(span)
}

/// Zero-width normalization-event ownership policy.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SpanEventPolicy {
    /// Generic spans select zero-width events strictly inside the span.
    Interior,
    /// Resolution partitions select zero-width events by the comparable-token
    /// owner: the first token at the event's canonical position (or the
    /// preceding token for a terminal event). The span that contains that
    /// token owns the event, so unmapped-only slices and adjacent entries
    /// neither lose nor duplicate it.
    ResolutionRange,
}

/// Reusable, validated source-evidence index for one document side.
///
/// Build this once when projecting multiple spans from the same normalized
/// document. [`project_span_sources`] is the convenience path for one span.
pub struct SpanSourceProjector<'a> {
    blocks: HashMap<BlockId, &'a BlockText>,
    glyphs: HashMap<GlyphId, &'a GlyphEvidence>,
    limits: SpanSourceProjectionLimits,
}

impl<'a> SpanSourceProjector<'a> {
    /// Builds indexes after validating all block and glyph ids and glyph
    /// geometry.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate ids, non-finite geometry, invalid limits,
    /// or allocation and configured resource limits.
    pub fn new(
        blocks: &'a [BlockText],
        glyph_evidence: &'a [GlyphEvidence],
        limits: SpanSourceProjectionLimits,
    ) -> Result<Self> {
        if limits.max_comparable_tokens == 0 || limits.max_evidence_items == 0 {
            return Err(Error::InvalidConfiguration(
                "span source projection limits must be greater than zero".to_owned(),
            ));
        }
        check_limit(
            "span source blocks",
            blocks.len(),
            limits.max_evidence_items,
        )?;
        check_limit(
            "span source glyph evidence",
            glyph_evidence.len(),
            limits.max_evidence_items,
        )?;
        for glyph in glyph_evidence {
            for (field, value) in [
                ("bbox.min.x", glyph.bbox.min.x),
                ("bbox.min.y", glyph.bbox.min.y),
                ("bbox.max.x", glyph.bbox.max.x),
                ("bbox.max.y", glyph.bbox.max.y),
            ] {
                if !value.is_finite() {
                    return Err(Error::InvalidConfiguration(format!(
                        "glyph {} has non-finite {field}: {value}",
                        glyph.id.0
                    )));
                }
            }
        }

        let mut indexed_blocks = HashMap::new();
        reserve(
            &mut indexed_blocks,
            blocks.len(),
            "span source blocks",
            limits,
        )?;
        for block in blocks {
            if indexed_blocks.insert(block.block, block).is_some() {
                return Err(Error::InvalidConfiguration(format!(
                    "duplicate block id {}",
                    block.block.0
                )));
            }
        }

        let mut glyphs = HashMap::new();
        reserve(
            &mut glyphs,
            glyph_evidence.len(),
            "span source glyph evidence",
            limits,
        )?;
        for glyph in glyph_evidence {
            if glyphs.insert(glyph.id, glyph).is_some() {
                return Err(Error::Report(format!(
                    "duplicate glyph evidence id {}",
                    glyph.id.0
                )));
            }
        }
        Ok(Self {
            blocks: indexed_blocks,
            glyphs,
            limits,
        })
    }

    /// Projects one span to ordered, deduplicated source evidence.
    ///
    /// # Errors
    ///
    /// Returns an error for missing or inconsistent evidence, malformed
    /// ranges, or a configured resource limit.
    pub fn project(&self, span: &TextSpan) -> Result<Vec<SpanSourceEvidence>> {
        self.project_with_policy(span, SpanEventPolicy::Interior)
    }

    /// Projects one resolution-partition span.
    ///
    /// Zero-width normalization events are selected by their comparable-token
    /// owner: the first token at the event's grouped canonical position (or the
    /// preceding token for a terminal event). The span that contains that
    /// token owns the event, which covers unmapped-only slices and boundary
    /// positions between adjacent entries without duplicating or dropping it.
    /// A comparable-empty span keeps the canonical point/interior rule, and
    /// tokens absent from the block stay with the entry ending at the event.
    /// Every other selection rule matches [`Self::project`].
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::project`].
    pub(crate) fn project_resolution_range(
        &self,
        span: &TextSpan,
    ) -> Result<Vec<SpanSourceEvidence>> {
        self.project_with_policy(span, SpanEventPolicy::ResolutionRange)
    }

    fn project_with_policy(
        &self,
        span: &TextSpan,
        policy: SpanEventPolicy,
    ) -> Result<Vec<SpanSourceEvidence>> {
        if span
            .separator
            .is_some_and(|separator| !separator.valid_for(span.blocks.len()))
        {
            return Err(Error::InvalidConfiguration(
                "separator pattern does not cover its block group".to_owned(),
            ));
        }
        if span.comparable_range.start > span.comparable_range.end
            || span.canonical_range.start > span.canonical_range.end
        {
            return Err(Error::InvalidConfiguration(
                "text span ranges must be ordered".to_owned(),
            ));
        }
        check_limit(
            "span source blocks",
            span.blocks.len(),
            self.limits.max_evidence_items,
        )?;

        let mut token_capacity = 0_usize;
        let mut atom_work = 0_usize;
        let mut event_capacity = 0_usize;
        let mut previous_token = None;
        for (position, block_id) in span.blocks.iter().copied().enumerate() {
            let block = self.block(block_id)?;
            let counts = preflight_block(block)?;
            let (first_token, last_token) = block_boundary_tokens(block);
            let inserts_separator = position > 0
                && separator_inserts_space_tokens(
                    span.separator
                        .unwrap_or(BlockSeparator::Concatenate)
                        .at(position - 1),
                    previous_token,
                    first_token,
                );
            token_capacity = token_capacity
                .checked_add(counts.token_count)
                .and_then(|count| count.checked_add(usize::from(inserts_separator)))
                .ok_or_else(|| token_limit(self.limits))?;
            if inserts_separator {
                previous_token = Some(ProjectedComparableToken::Scalar(' '));
            }
            if let Some(last_token) = last_token {
                previous_token = Some(last_token);
            }
            atom_work = atom_work
                .checked_add(counts.atom_work)
                .ok_or_else(|| evidence_limit(self.limits))?;
            event_capacity = event_capacity
                .checked_add(block.normalization_events.len())
                .ok_or_else(|| evidence_limit(self.limits))?;
        }
        check_limit(
            "span source comparable tokens",
            token_capacity,
            self.limits.max_comparable_tokens,
        )?;
        check_limit(
            "span source atom traversal",
            atom_work,
            self.limits.max_evidence_items,
        )?;
        check_limit(
            "span source normalization events",
            event_capacity,
            self.limits.max_evidence_items,
        )?;

        let mut tokens = Vec::new();
        let mut event_sources = Vec::new();
        try_reserve_vec(
            &mut tokens,
            token_capacity,
            "span source comparable tokens",
            self.limits.max_comparable_tokens,
        )?;
        try_reserve_vec(
            &mut event_sources,
            event_capacity,
            "span source normalization events",
            self.limits.max_evidence_items,
        )?;
        let mut canonical_offset = 0;
        let mut previous_token = None;
        for (position, block_id) in span.blocks.iter().copied().enumerate() {
            let block = self.block(block_id)?;
            let (first_token, last_token) = block_boundary_tokens(block);
            // Decide the separator before appending so tokens are pushed in
            // order: inserting into the middle would shift every following
            // token for every block boundary.
            let inserts_separator = position > 0
                && separator_inserts_space_tokens(
                    span.separator
                        .unwrap_or(BlockSeparator::Concatenate)
                        .at(position - 1),
                    previous_token,
                    first_token,
                );
            if inserts_separator {
                tokens.push(ProjectedToken {
                    token: ProjectedComparableToken::Scalar(' '),
                    canonical_position: canonical_offset,
                    source: ProjectedTokenSource::BlockSeparatorSpace,
                });
                canonical_offset += 1;
            }
            let token_base = tokens.len();
            append_block_tokens(&mut tokens, block, canonical_offset);
            previous_token = last_token.or(if inserts_separator {
                Some(ProjectedComparableToken::Scalar(' '))
            } else {
                previous_token
            });
            let block_scalar_count = block.canonical.text.chars().count();
            let block_start = canonical_offset;
            canonical_offset += block_scalar_count;
            for (sequence, event) in block.normalization_events.iter().enumerate() {
                let event_range = ScalarRange {
                    start: block_start + event.canonical_range.start,
                    end: block_start + event.canonical_range.end,
                };
                let selected = match policy {
                    SpanEventPolicy::Interior => {
                        normalization_event_selected(event_range, span.canonical_range)
                    }
                    SpanEventPolicy::ResolutionRange => {
                        resolution_event_selected(event_range, span, &tokens, token_base)?
                    }
                };
                if selected {
                    event_sources.push(PositionedSource {
                        canonical_position: event_range.start,
                        tie_break: if event_range.start == event_range.end {
                            0
                        } else {
                            2
                        },
                        sequence,
                        order: event_sources.len(),
                        source: ProjectedTokenSource::Text(&event.source),
                    });
                }
            }
        }

        if span.comparable_range.end > tokens.len() || span.canonical_range.end > canonical_offset {
            return Err(Error::InvalidConfiguration(
                "text span range exceeds the normalized block evidence".to_owned(),
            ));
        }
        let canonical_start = tokens[..span.comparable_range.start]
            .iter()
            .filter(|token| token.token.is_scalar())
            .count();
        let canonical_end = tokens[..span.comparable_range.end]
            .iter()
            .filter(|token| token.token.is_scalar())
            .count();
        if canonical_start != span.canonical_range.start
            || canonical_end != span.canonical_range.end
        {
            return Err(Error::InvalidConfiguration(
                "text span canonical and comparable ranges select different scalar evidence"
                    .to_owned(),
            ));
        }

        let selected_count = span.comparable_range.end - span.comparable_range.start;
        let positioned_count = selected_count
            .checked_add(event_sources.len())
            .ok_or_else(|| evidence_limit(self.limits))?;
        check_limit(
            "span source positioned evidence",
            positioned_count,
            self.limits.max_evidence_items,
        )?;
        let mut positioned = Vec::new();
        try_reserve_vec(
            &mut positioned,
            positioned_count,
            "span source positioned evidence",
            self.limits.max_evidence_items,
        )?;
        positioned.extend(
            tokens[span.comparable_range.start..span.comparable_range.end]
                .iter()
                .enumerate()
                .map(|(sequence, token)| PositionedSource {
                    canonical_position: token.canonical_position,
                    tie_break: 1,
                    sequence,
                    order: sequence,
                    source: token.source,
                }),
        );
        for (index, source) in event_sources.iter_mut().enumerate() {
            source.order = selected_count + index;
        }
        positioned.extend(event_sources);
        positioned.sort_unstable_by_key(|source| {
            (
                source.canonical_position,
                source.tie_break,
                source.sequence,
                source.order,
            )
        });

        let mut output = Vec::new();
        let mut seen_atoms = HashSet::new();
        let mut seen_glyphs = HashSet::new();
        for positioned_source in positioned {
            match positioned_source.source {
                ProjectedTokenSource::BlockSeparatorSpace => push_output(
                    &mut output,
                    SpanSourceEvidence::BlockSeparatorSpace,
                    self.limits,
                )?,
                ProjectedTokenSource::None => {}
                ProjectedTokenSource::Text(source) => {
                    for atom in &source.atoms {
                        if seen_atoms.contains(atom) {
                            continue;
                        }
                        reserve(&mut seen_atoms, 1, "span source atoms", self.limits)?;
                        seen_atoms.insert(atom);
                        match atom {
                            TextSourceAtom::Glyph(id) => {
                                self.push_glyph(&mut output, &mut seen_glyphs, *id)?;
                            }
                            TextSourceAtom::SyntheticSpace {
                                preceding,
                                following,
                            } => {
                                push_output(
                                    &mut output,
                                    SpanSourceEvidence::SyntheticSpace {
                                        preceding_glyph_id: *preceding,
                                        following_glyph_id: *following,
                                    },
                                    self.limits,
                                )?;
                                self.push_glyph(&mut output, &mut seen_glyphs, *preceding)?;
                                self.push_glyph(&mut output, &mut seen_glyphs, *following)?;
                            }
                            TextSourceAtom::LineBreak {
                                preceding,
                                following,
                            } => {
                                push_output(
                                    &mut output,
                                    SpanSourceEvidence::LineBreak {
                                        preceding_glyph_id: *preceding,
                                        following_glyph_id: *following,
                                    },
                                    self.limits,
                                )?;
                                self.push_glyph(&mut output, &mut seen_glyphs, *preceding)?;
                                self.push_glyph(&mut output, &mut seen_glyphs, *following)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(output)
    }

    fn block(&self, id: BlockId) -> Result<&'a BlockText> {
        self.blocks.get(&id).copied().ok_or_else(|| {
            Error::InvalidConfiguration(format!(
                "text span references block {} that has no normalized evidence",
                id.0
            ))
        })
    }

    fn push_glyph(
        &self,
        output: &mut Vec<SpanSourceEvidence>,
        seen: &mut HashSet<GlyphId>,
        id: GlyphId,
    ) -> Result<()> {
        if seen.contains(&id) {
            return Ok(());
        }
        reserve(seen, 1, "span source glyph ids", self.limits)?;
        seen.insert(id);
        let glyph = self
            .glyphs
            .get(&id)
            .copied()
            .ok_or_else(|| Error::Report(format!("missing glyph evidence for id {}", id.0)))?;
        push_output(
            output,
            SpanSourceEvidence::Glyph {
                glyph_id: glyph.id,
                page: glyph.page,
                bbox: glyph.bbox,
                content_stream: glyph.provenance.content_stream,
                operator_index: glyph.provenance.operator_index,
            },
            self.limits,
        )?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ProjectedTokenSource<'a> {
    Text(&'a TextSource),
    None,
    BlockSeparatorSpace,
}

struct ProjectedToken<'a> {
    token: ProjectedComparableToken,
    canonical_position: usize,
    source: ProjectedTokenSource<'a>,
}

struct PositionedSource<'a> {
    canonical_position: usize,
    tie_break: u8,
    sequence: usize,
    order: usize,
    source: ProjectedTokenSource<'a>,
}

#[derive(Clone, Copy)]
enum ProjectedComparableToken {
    Scalar(char),
    Unmapped,
}

impl ProjectedComparableToken {
    fn is_scalar(self) -> bool {
        matches!(self, Self::Scalar(_))
    }

    fn is_space(self) -> bool {
        matches!(self, Self::Scalar(scalar) if scalar.is_whitespace())
    }
}

struct BlockPreflight {
    token_count: usize,
    atom_work: usize,
}

fn preflight_block(block: &BlockText) -> Result<BlockPreflight> {
    let scalar_count = block.canonical.text.chars().count();
    let mut previous_unmapped_index = 0;
    let mut atom_work = 0_usize;
    for token in &block.canonical.unmapped {
        if token.scalar_index > scalar_count || token.scalar_index < previous_unmapped_index {
            return Err(Error::Unresolved(
                "unmapped token indices must be ordered scalar offsets within the text".to_owned(),
            ));
        }
        previous_unmapped_index = token.scalar_index;
        atom_work =
            atom_work
                .checked_add(token.source.atoms.len())
                .ok_or(Error::LimitExceeded {
                    resource: "span source atom traversal",
                    limit: usize::MAX,
                })?;
    }

    let mut previous_end = 0;
    for entry in &block.canonical.source_map {
        if entry.output_range.start > entry.output_range.end
            || entry.output_range.end > scalar_count
            || entry.output_range.start < previous_end
        {
            return Err(Error::Unresolved(
                "source map ranges must be ordered, non-overlapping scalar offsets within the text"
                    .to_owned(),
            ));
        }
        previous_end = entry.output_range.end;
        let covered_scalars = entry.output_range.end - entry.output_range.start;
        atom_work = atom_work
            .checked_add(
                covered_scalars
                    .checked_mul(entry.source.atoms.len())
                    .ok_or(Error::LimitExceeded {
                        resource: "span source atom traversal",
                        limit: usize::MAX,
                    })?,
            )
            .ok_or(Error::LimitExceeded {
                resource: "span source atom traversal",
                limit: usize::MAX,
            })?;
    }
    for event in &block.normalization_events {
        if event.canonical_range.start > event.canonical_range.end
            || event.canonical_range.end > scalar_count
        {
            return Err(Error::InvalidConfiguration(
                "normalization event range exceeds its normalized block".to_owned(),
            ));
        }
        atom_work =
            atom_work
                .checked_add(event.source.atoms.len())
                .ok_or(Error::LimitExceeded {
                    resource: "span source atom traversal",
                    limit: usize::MAX,
                })?;
    }
    let token_count = scalar_count
        .checked_add(block.canonical.unmapped.len())
        .ok_or(Error::LimitExceeded {
            resource: "span source comparable tokens",
            limit: usize::MAX,
        })?;
    Ok(BlockPreflight {
        token_count,
        atom_work,
    })
}

fn append_block_tokens<'a>(
    output: &mut Vec<ProjectedToken<'a>>,
    block: &'a BlockText,
    canonical_offset: usize,
) {
    let mut unmapped_index = 0;
    let mut source_index = 0;
    for (index, scalar) in block.canonical.text.chars().enumerate() {
        while block
            .canonical
            .unmapped
            .get(unmapped_index)
            .is_some_and(|token| token.scalar_index == index)
        {
            let token = &block.canonical.unmapped[unmapped_index];
            output.push(ProjectedToken {
                token: ProjectedComparableToken::Unmapped,
                canonical_position: canonical_offset + index,
                source: ProjectedTokenSource::Text(&token.source),
            });
            unmapped_index += 1;
        }
        while block
            .canonical
            .source_map
            .get(source_index)
            .is_some_and(|entry| entry.output_range.end <= index)
        {
            source_index += 1;
        }
        let source = block
            .canonical
            .source_map
            .get(source_index)
            .filter(|entry| entry.output_range.start <= index && index < entry.output_range.end)
            .map_or(ProjectedTokenSource::None, |entry| {
                ProjectedTokenSource::Text(&entry.source)
            });
        output.push(ProjectedToken {
            token: ProjectedComparableToken::Scalar(scalar),
            canonical_position: canonical_offset + index,
            source,
        });
    }
    for token in &block.canonical.unmapped[unmapped_index..] {
        output.push(ProjectedToken {
            token: ProjectedComparableToken::Unmapped,
            canonical_position: canonical_offset + block.canonical.text.chars().count(),
            source: ProjectedTokenSource::Text(&token.source),
        });
    }
}

fn block_boundary_tokens(
    block: &BlockText,
) -> (
    Option<ProjectedComparableToken>,
    Option<ProjectedComparableToken>,
) {
    let scalar_count = block.canonical.text.chars().count();
    let first = if block
        .canonical
        .unmapped
        .first()
        .is_some_and(|token| token.scalar_index == 0)
    {
        Some(ProjectedComparableToken::Unmapped)
    } else {
        block
            .canonical
            .text
            .chars()
            .next()
            .map(ProjectedComparableToken::Scalar)
            .or_else(|| {
                (!block.canonical.unmapped.is_empty()).then_some(ProjectedComparableToken::Unmapped)
            })
    };
    let last = if block
        .canonical
        .unmapped
        .last()
        .is_some_and(|token| token.scalar_index == scalar_count)
    {
        Some(ProjectedComparableToken::Unmapped)
    } else {
        block
            .canonical
            .text
            .chars()
            .next_back()
            .map(ProjectedComparableToken::Scalar)
            .or_else(|| {
                (!block.canonical.unmapped.is_empty()).then_some(ProjectedComparableToken::Unmapped)
            })
    };
    (first, last)
}

fn separator_inserts_space_tokens(
    separator: BlockSeparator,
    previous: Option<ProjectedComparableToken>,
    next: Option<ProjectedComparableToken>,
) -> bool {
    separator == BlockSeparator::Space
        && !previous.is_some_and(ProjectedComparableToken::is_space)
        && !next.is_some_and(ProjectedComparableToken::is_space)
}

fn normalization_event_selected(event: ScalarRange, span: ScalarRange) -> bool {
    if span.start == span.end {
        return span.start == event.start || span.start == event.end;
    }
    if event.start == event.end {
        return span.start < event.start && event.start < span.end;
    }
    event.start < span.end && event.end > span.start
}

/// Applies the generic predicate unless a zero-width event needs an owner.
///
/// The owner is the first token in the already-appended span slice whose
/// grouped canonical position equals the event position, or the preceding
/// token for a terminal event. The owner's grouped comparable index is
/// compared directly with the span's comparable range, so no canonical
/// subtraction or re-tokenization is involved.
fn resolution_event_selected(
    event: ScalarRange,
    span: &TextSpan,
    tokens: &[ProjectedToken<'_>],
    token_base: usize,
) -> Result<bool> {
    if event.start != event.end {
        return Ok(normalization_event_selected(event, span.canonical_range));
    }
    let slice = &tokens[token_base..];
    let index = slice.partition_point(|token| token.canonical_position < event.start);
    let local = if index < slice.len() && slice[index].canonical_position == event.start {
        index
    } else if index > 0 {
        // Terminal policy: a trailing event belongs to the preceding token.
        index - 1
    } else {
        // No token exists at or before the event in this block: keep the event
        // with the entry that ends at its position so nothing is lost.
        return Ok(span.canonical_range.end == event.start);
    };
    let grouped = token_base
        .checked_add(local)
        .ok_or_else(|| Error::Unresolved("span source token index overflow".to_owned()))?;
    if span.comparable_range.start < span.comparable_range.end {
        return Ok(grouped >= span.comparable_range.start && grouped < span.comparable_range.end);
    }
    Ok(normalization_event_selected(event, span.canonical_range))
}

fn push_output(
    output: &mut Vec<SpanSourceEvidence>,
    source: SpanSourceEvidence,
    limits: SpanSourceProjectionLimits,
) -> Result<()> {
    check_limit(
        "span source output evidence",
        output.len().saturating_add(1),
        limits.max_evidence_items,
    )?;
    try_reserve_vec(
        output,
        1,
        "span source output evidence",
        limits.max_evidence_items,
    )?;
    output.push(source);
    Ok(())
}

fn reserve<T>(
    collection: &mut impl TryReserve<T>,
    additional: usize,
    resource: &'static str,
    limits: SpanSourceProjectionLimits,
) -> Result<()> {
    let requested = collection.len().saturating_add(additional);
    check_limit(resource, requested, limits.max_evidence_items)?;
    collection
        .try_reserve(additional)
        .map_err(|()| Error::LimitExceeded {
            resource,
            limit: limits.max_evidence_items,
        })
}

trait TryReserve<T> {
    fn len(&self) -> usize;
    fn try_reserve(&mut self, additional: usize) -> std::result::Result<(), ()>;
}

impl<K: Eq + std::hash::Hash, V> TryReserve<(K, V)> for HashMap<K, V> {
    fn len(&self) -> usize {
        HashMap::len(self)
    }

    fn try_reserve(&mut self, additional: usize) -> std::result::Result<(), ()> {
        HashMap::try_reserve(self, additional).map_err(|_| ())
    }
}

impl<T: Eq + std::hash::Hash> TryReserve<T> for HashSet<T> {
    fn len(&self) -> usize {
        HashSet::len(self)
    }

    fn try_reserve(&mut self, additional: usize) -> std::result::Result<(), ()> {
        HashSet::try_reserve(self, additional).map_err(|_| ())
    }
}

fn try_reserve_vec<T>(
    output: &mut Vec<T>,
    additional: usize,
    resource: &'static str,
    limit: usize,
) -> Result<()> {
    output
        .try_reserve(additional)
        .map_err(|_| Error::LimitExceeded { resource, limit })
}

fn check_limit(resource: &'static str, count: usize, limit: usize) -> Result<()> {
    if count > limit {
        return Err(Error::LimitExceeded { resource, limit });
    }
    Ok(())
}

fn token_limit(limits: SpanSourceProjectionLimits) -> Error {
    Error::LimitExceeded {
        resource: "span source comparable tokens",
        limit: limits.max_comparable_tokens,
    }
}

fn evidence_limit(limits: SpanSourceProjectionLimits) -> Error {
    Error::LimitExceeded {
        resource: "span source evidence",
        limit: limits.max_evidence_items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GlyphProvenance, Vec2};

    fn glyph_evidence(id: u64) -> GlyphEvidence {
        GlyphEvidence {
            id: GlyphId(id),
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 0.0, y: 0.0 },
                max: Vec2 { x: 1.0, y: 1.0 },
            },
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: id as u32,
            },
        }
    }

    #[test]
    fn resolution_partition_owns_a_boundary_line_break_exactly_once() {
        use crate::{
            layout::{BlockId, BlockRole},
            normalize::{
                BlockText, MappedText, NormalizationEvent, NormalizationKind, ScalarRange,
                SourceMapEntry, TextSource, TextSourceAtom,
            },
            report::TextSpan,
        };
        let canonical = MappedText {
            text: "AB".to_owned(),
            source_map: vec![
                SourceMapEntry {
                    output_range: ScalarRange { start: 0, end: 1 },
                    source: TextSource {
                        atoms: vec![TextSourceAtom::Glyph(GlyphId(10))].into(),
                    },
                },
                SourceMapEntry {
                    output_range: ScalarRange { start: 1, end: 2 },
                    source: TextSource {
                        atoms: vec![TextSourceAtom::Glyph(GlyphId(11))].into(),
                    },
                },
            ],
            unmapped: Vec::new(),
        };
        let tokens = canonical.comparable_tokens().expect("tokens");
        let block = BlockText {
            block: BlockId(1),
            role: BlockRole::Body,
            raw: canonical.clone(),
            canonical,
            matching: "AB".to_owned(),
            matching_tokens: tokens,
            numeric_mask_applied: false,
            normalization_events: vec![NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                canonical_range: ScalarRange { start: 1, end: 1 },
                source: TextSource {
                    atoms: vec![TextSourceAtom::LineBreak {
                        preceding: GlyphId(10),
                        following: GlyphId(11),
                    }]
                    .into(),
                },
            }],
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        };
        let glyphs = [glyph_evidence(10), glyph_evidence(11)];
        let projector = SpanSourceProjector::new(
            std::slice::from_ref(&block),
            &glyphs,
            SpanSourceProjectionLimits::default(),
        )
        .expect("projector");
        let span = |start: usize, end: usize| TextSpan {
            blocks: vec![BlockId(1)],
            separator: None,
            canonical_range: ScalarRange { start, end },
            comparable_range: crate::diff::TokenRange { start, end },
        };
        let left = span(0, 1);
        let right = span(1, 2);
        let line_breaks = |evidence: &[SpanSourceEvidence]| {
            evidence
                .iter()
                .filter(|item| matches!(item, SpanSourceEvidence::LineBreak { .. }))
                .count()
        };
        // The generic boundary-span contract excludes the boundary event from
        // both sides, which is why the resolution partition must own it.
        assert_eq!(line_breaks(&projector.project(&left).expect("left")), 0);
        assert_eq!(line_breaks(&projector.project(&right).expect("right")), 0);
        let left_resolution = projector
            .project_resolution_range(&left)
            .expect("left resolution");
        let right_resolution = projector
            .project_resolution_range(&right)
            .expect("right resolution");
        assert_eq!(line_breaks(&left_resolution), 0);
        assert_eq!(line_breaks(&right_resolution), 1);
        assert_eq!(
            line_breaks(&left_resolution) + line_breaks(&right_resolution),
            1
        );
    }

    #[test]
    fn resolution_ownership_covers_leading_and_terminal_events() {
        // Leading event at canonical 0 belongs to the token starting there;
        // terminal event at the block end belongs to the preceding token.
        for (event_start, expected_left, expected_right) in
            [(0usize, true, false), (2usize, false, true)]
        {
            let mut block = boundary_block(5);
            block.normalization_events[0].canonical_range = ScalarRange {
                start: event_start,
                end: event_start,
            };
            let glyphs = [glyph_evidence(50), glyph_evidence(51)];
            let projector = SpanSourceProjector::new(
                std::slice::from_ref(&block),
                &glyphs,
                SpanSourceProjectionLimits::default(),
            )
            .expect("projector");
            let left = TextSpan {
                blocks: vec![BlockId(5)],
                separator: None,
                canonical_range: ScalarRange { start: 0, end: 1 },
                comparable_range: crate::diff::TokenRange { start: 0, end: 1 },
            };
            let right = TextSpan {
                blocks: vec![BlockId(5)],
                separator: None,
                canonical_range: ScalarRange { start: 1, end: 2 },
                comparable_range: crate::diff::TokenRange { start: 1, end: 2 },
            };
            let has_break = |span: &TextSpan| {
                projector
                    .project_resolution_range(span)
                    .expect("resolution evidence")
                    .iter()
                    .any(|item| matches!(item, SpanSourceEvidence::LineBreak { .. }))
            };
            assert_eq!(has_break(&left), expected_left, "event {event_start} left");
            assert_eq!(
                has_break(&right),
                expected_right,
                "event {event_start} right"
            );
        }
    }

    #[test]
    fn resolution_ownership_never_crosses_blocks() {
        // A boundary event in an unrelated block must not be selected by a
        // span that lists only the first block.
        let blocks = [boundary_block(1), boundary_block(2)];
        let glyphs = [
            glyph_evidence(10),
            glyph_evidence(11),
            glyph_evidence(20),
            glyph_evidence(21),
        ];
        let projector =
            SpanSourceProjector::new(&blocks, &glyphs, SpanSourceProjectionLimits::default())
                .expect("projector");
        let span = TextSpan {
            blocks: vec![BlockId(1)],
            separator: None,
            canonical_range: ScalarRange { start: 0, end: 2 },
            comparable_range: crate::diff::TokenRange { start: 0, end: 2 },
        };
        let evidence = projector
            .project_resolution_range(&span)
            .expect("resolution evidence");
        let breaks = evidence
            .iter()
            .filter(|item| matches!(item, SpanSourceEvidence::LineBreak { .. }))
            .count();
        assert_eq!(breaks, 1, "only the first block's boundary event appears");
    }

    fn boundary_block(id: u64) -> BlockText {
        use crate::{
            layout::{BlockId, BlockRole},
            normalize::{
                MappedText, NormalizationEvent, NormalizationKind, ScalarRange, SourceMapEntry,
                TextSource, TextSourceAtom,
            },
        };
        let entry = |index: usize, glyph: u64| SourceMapEntry {
            output_range: ScalarRange {
                start: index,
                end: index + 1,
            },
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(GlyphId(glyph))].into(),
            },
        };
        let canonical = MappedText {
            text: "AB".to_owned(),
            source_map: vec![entry(0, id * 10), entry(1, id * 10 + 1)],
            unmapped: Vec::new(),
        };
        let tokens = canonical.comparable_tokens().expect("tokens");
        BlockText {
            block: BlockId(id),
            role: BlockRole::Body,
            raw: canonical.clone(),
            canonical,
            matching: "AB".to_owned(),
            matching_tokens: tokens,
            numeric_mask_applied: false,
            normalization_events: vec![NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                canonical_range: ScalarRange { start: 1, end: 1 },
                source: TextSource {
                    atoms: vec![TextSourceAtom::LineBreak {
                        preceding: GlyphId(id * 10),
                        following: GlyphId(id * 10 + 1),
                    }]
                    .into(),
                },
            }],
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        }
    }

    #[test]
    fn resolution_ownership_handles_an_unmapped_boundary_slice() {
        use crate::{
            layout::{BlockId, BlockRole},
            normalize::{
                MappedText, NormalizationEvent, NormalizationKind, ScalarRange, SourceMapEntry,
                TextSource, TextSourceAtom, UnmappedToken,
            },
        };
        let entry = |index: usize, glyph: u64| SourceMapEntry {
            output_range: ScalarRange {
                start: index,
                end: index + 1,
            },
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(GlyphId(glyph))].into(),
            },
        };
        let mut canonical = MappedText {
            text: "AB".to_owned(),
            source_map: vec![entry(0, 10), entry(1, 11)],
            unmapped: Vec::new(),
        };
        canonical.unmapped = vec![UnmappedToken {
            scalar_index: 1,
            font_hash: crate::model::FontProgramHash(vec![0]),
            glyph_id: 99,
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(GlyphId(99))].into(),
            },
        }];
        let tokens = canonical.comparable_tokens().expect("tokens");
        let block = BlockText {
            block: BlockId(7),
            role: BlockRole::Body,
            raw: canonical.clone(),
            canonical,
            matching: "AB".to_owned(),
            matching_tokens: tokens,
            numeric_mask_applied: false,
            normalization_events: vec![NormalizationEvent {
                kind: NormalizationKind::SoftLineBreak,
                raw_range: ScalarRange { start: 1, end: 2 },
                canonical_range: ScalarRange { start: 1, end: 1 },
                source: TextSource {
                    atoms: vec![TextSourceAtom::LineBreak {
                        preceding: GlyphId(10),
                        following: GlyphId(11),
                    }]
                    .into(),
                },
            }],
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        };
        let glyphs = [glyph_evidence(10), glyph_evidence(11), glyph_evidence(99)];
        let projector = SpanSourceProjector::new(
            std::slice::from_ref(&block),
            &glyphs,
            SpanSourceProjectionLimits::default(),
        )
        .expect("projector");
        // The unmapped token owns the boundary position: canonical 1..1 but
        // comparable 1..2.
        let unmapped_span = TextSpan {
            blocks: vec![BlockId(7)],
            separator: None,
            canonical_range: ScalarRange { start: 1, end: 1 },
            comparable_range: crate::diff::TokenRange { start: 1, end: 2 },
        };
        let evidence = projector
            .project_resolution_range(&unmapped_span)
            .expect("unmapped resolution evidence");
        assert!(
            evidence
                .iter()
                .any(|item| matches!(item, SpanSourceEvidence::LineBreak { .. })),
            "the unmapped slice owns the boundary event"
        );
        let following_scalar_span = TextSpan {
            blocks: vec![BlockId(7)],
            separator: None,
            canonical_range: ScalarRange { start: 1, end: 2 },
            comparable_range: crate::diff::TokenRange { start: 2, end: 3 },
        };
        let follow = projector
            .project_resolution_range(&following_scalar_span)
            .expect("following resolution evidence");
        assert!(
            !follow
                .iter()
                .any(|item| matches!(item, SpanSourceEvidence::LineBreak { .. })),
            "the following scalar slice must not duplicate it"
        );
    }

    #[test]
    fn rejects_non_finite_glyph_geometry() {
        let glyph = GlyphEvidence {
            id: GlyphId(0),
            page: PageId(0),
            bbox: Rect {
                min: Vec2 {
                    x: f64::NAN,
                    y: 0.0,
                },
                max: Vec2 { x: 1.0, y: 1.0 },
            },
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: 0,
            },
        };

        let error = SpanSourceProjector::new(&[], &[glyph], SpanSourceProjectionLimits::default())
            .err()
            .expect("non-finite geometry must not enter a report");

        assert!(matches!(
            error,
            Error::InvalidConfiguration(message)
                if message.contains("glyph 0 has non-finite bbox.min.x")
        ));
    }
}
