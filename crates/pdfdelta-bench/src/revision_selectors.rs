//! Source-only resolution of reviewed selectors before comparison output exists.

use pdfdelta_core::normalize::{BlockText, ScalarRange, TextSourceAtom};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    ExpectedDocument,
    revision_diagnostics::{
        DiagnosticBudget, DiagnosticLimits, ScopedQuoteLocateOutcome, ScopedQuoteRange,
        locate_scoped_quote,
    },
    revision_scopes::{resolve_revision_scopes, validate_scoped_expected_changes},
};

#[derive(Debug, Serialize)]
pub struct SelectorValidation {
    pub schema_version: u32,
    pub pair: String,
    pub expectations: Vec<ExpectationSelector>,
}

#[derive(Debug, Serialize)]
pub struct ExpectationSelector {
    pub id: String,
    pub old: SideSelector,
    pub new: SideSelector,
    /// This validates annotation coordinates, not the correctness of a diff.
    pub changed_range_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SideSelector {
    pub status: String,
    pub sources: Vec<ScalarSource>,
}

/// Canonical scalar identity retains multiplicity when one glyph expands.
#[derive(Debug, Serialize)]
pub struct ScalarSource {
    pub block: u64,
    pub scalar: usize,
    pub value: char,
    /// Page context of the entire block, not a glyph bounding-box projection.
    pub block_pages: Vec<u32>,
    pub atoms: Vec<SelectorAtom>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectorAtom {
    Glyph { id: u64 },
    SyntheticSpace { preceding: u64, following: u64 },
    LineBreak { preceding: u64, following: u64 },
}

impl From<TextSourceAtom> for SelectorAtom {
    fn from(atom: TextSourceAtom) -> Self {
        match atom {
            TextSourceAtom::Glyph(id) => Self::Glyph { id: id.0 },
            TextSourceAtom::SyntheticSpace {
                preceding,
                following,
            } => Self::SyntheticSpace {
                preceding: preceding.0,
                following: following.0,
            },
            TextSourceAtom::LineBreak {
                preceding,
                following,
            } => Self::LineBreak {
                preceding: preceding.0,
                following: following.0,
            },
        }
    }
}

/// Resolves each expectation independently using only normalized source blocks.
/// Missing or uncertain selectors remain explicit. No comparison result is an
/// input, and a unique selector alone does not certify extraction completeness.
#[must_use]
pub fn validate(
    expected: &ExpectedDocument,
    old: &[BlockText],
    new: &[BlockText],
) -> SelectorValidation {
    let mut budget = DiagnosticBudget::default();
    let limits = DiagnosticLimits::default();
    let mut projection_work = 64_000_000usize;
    let expectations = expected
        .changes
        .iter()
        .map(|change| {
            let scopes = change.scope.as_ref().map(|id| {
                let selected = expected
                    .scopes
                    .iter()
                    .filter(|scope| &scope.id == id)
                    .cloned()
                    .collect::<Vec<_>>();
                if selected.len() != 1 {
                    return Err("scope selector is missing or duplicated".to_owned());
                }
                resolve_revision_scopes(&selected, old, new)
            });
            let changed_range_error = scopes.as_ref().and_then(|scopes| match scopes {
                Ok(scopes) => {
                    validate_scoped_expected_changes(std::slice::from_ref(change), scopes, old, new)
                        .err()
                }
                Err(error) => Some(error.clone()),
            });
            let mut resolve = |blocks: &[BlockText], quote: Option<&str>, old_side: bool| {
                let Some(quote) = quote else {
                    return unavailable("not_required");
                };
                let range = match &scopes {
                    Some(Ok(scopes)) => {
                        let scope = if old_side {
                            scopes[0].old
                        } else {
                            scopes[0].new
                        };
                        ScopedQuoteRange {
                            start_block: scope.start.block_order,
                            start_scalar: scope.start.scalar,
                            end_block: scope.end.block_order,
                            end_scalar: scope.end.scalar,
                        }
                    }
                    Some(Err(error)) => return unavailable(error),
                    None => {
                        let Some(end_block) = blocks
                            .iter()
                            .rposition(|block| !block.canonical.text.is_empty())
                        else {
                            return unavailable("missing");
                        };
                        ScopedQuoteRange {
                            start_block: 0,
                            start_scalar: 0,
                            end_block,
                            end_scalar: blocks[end_block].canonical.text.chars().count() - 1,
                        }
                    }
                };
                match locate_scoped_quote(blocks, quote, range, &mut budget, limits) {
                    Ok(ScopedQuoteLocateOutcome::Unique(location)) => {
                        let mut sources = Vec::new();
                        for (index, block) in blocks
                            .iter()
                            .enumerate()
                            .take(location.end_block + 1)
                            .skip(location.start_block)
                        {
                            if !block.issues.is_empty() || !block.canonical.unmapped.is_empty() {
                                return unavailable("uncertain_source");
                            }
                            let start = if index == location.start_block {
                                location.start_scalar
                            } else {
                                0
                            };
                            let end = if index == location.end_block {
                                location.end_scalar
                            } else {
                                block.canonical.text.chars().count()
                            };
                            for (scalar, value) in block
                                .canonical
                                .text
                                .chars()
                                .enumerate()
                                .take(end)
                                .skip(start)
                            {
                                if sources.len() == 65_536 {
                                    return unavailable("projection_limit");
                                }
                                let Some(remaining) =
                                    projection_work.checked_sub(block.canonical.source_map.len())
                                else {
                                    return unavailable("projection_limit");
                                };
                                projection_work = remaining;
                                let atoms = block.canonical.project_source_atoms(ScalarRange {
                                    start: scalar,
                                    end: scalar + 1,
                                });
                                if atoms.is_empty() {
                                    return unavailable("source_missing");
                                }
                                sources.push(ScalarSource {
                                    block: block.block.0,
                                    scalar,
                                    value,
                                    block_pages: block.pages.clone(),
                                    atoms: atoms.into_iter().map(SelectorAtom::from).collect(),
                                });
                            }
                        }
                        SideSelector {
                            status: "unique_source_selector".into(),
                            sources,
                        }
                    }
                    Ok(outcome) => unavailable(&format!("{outcome:?}").to_lowercase()),
                    Err(error) => unavailable(&error),
                }
            };
            ExpectationSelector {
                id: change.id.clone(),
                old: resolve(old, change.old_quote.as_deref(), true),
                new: resolve(new, change.new_quote.as_deref(), false),
                changed_range_error,
            }
        })
        .collect();
    SelectorValidation {
        schema_version: 1,
        pair: expected.pair.clone(),
        expectations,
    }
}

fn unavailable(status: &str) -> SideSelector {
    SideSelector {
        status: status.to_owned(),
        sources: Vec::new(),
    }
}

/// Extracts source blocks without invoking candidate retrieval or comparison.
///
/// # Errors
/// Preserves parser, extraction, and reconstruction failures as contextual errors.
pub fn prepare(bytes: Vec<u8>) -> Result<PreparedRevisionSource, String> {
    prepare_with_view(bytes, RevisionSourceView::Layout)
}

#[derive(Clone, Copy, Debug)]
pub enum RevisionSourceView {
    Layout,
    /// Raw decoded glyphs in page-local paint order, without inferred spaces,
    /// normalization, or a claim that paint order is logical reading order.
    PaintOrder,
}

/// Extracts a declared source representation without comparison or expected text.
/// Paint order is useful for reviewing coordinates when layout reorders fragments;
/// physical annotation review and visibility checks remain separate obligations.
///
/// # Errors
/// Preserves extraction failures and rejects ambiguous or oversized paint streams.
pub fn prepare_with_view(
    bytes: Vec<u8>,
    view: RevisionSourceView,
) -> Result<PreparedRevisionSource, String> {
    use pdfdelta_core::{
        layout::{BlockOptions, LineOptions, reconstruct_blocks, reconstruct_lines},
        normalize::normalize_blocks,
        pdf::{LopdfParser, ParseLimits},
        source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
    };
    let hash = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let outcome = source
        .extract_outcome(
            bytes.into(),
            ParseLimits::default(),
            ExtractionLimits::default(),
        )
        .map_err(|error| format!("source extraction: {error}"))?;
    let document = outcome.document();
    let (normalized, source_view) = match view {
        RevisionSourceView::Layout => {
            let lines = reconstruct_lines(document, LineOptions::default())
                .map_err(|error| format!("source lines: {error}"))?;
            let blocks = reconstruct_blocks(document, &lines, BlockOptions::default())
                .map_err(|error| format!("source blocks: {error}"))?;
            let normalized = normalize_blocks(document, &lines, &blocks)
                .map_err(|error| format!("source normalization: {error}"))?;
            (normalized, "default_native_layout_canonical_scalars_v1")
        }
        RevisionSourceView::PaintOrder => (
            paint_order_blocks(document)?,
            "native_page_paint_order_scalars_v1",
        ),
    };
    let metadata = RevisionSourceMetadata {
        sha256: hash,
        extraction_complete: outcome.is_complete(),
        extraction_issues: outcome
            .issues()
            .iter()
            .map(|issue| format!("{issue:?}"))
            .collect(),
        glyph_count: document.items().len(),
        block_count: normalized.len(),
        source_view,
    };
    Ok(PreparedRevisionSource {
        blocks: normalized,
        metadata,
    })
}

fn paint_order_blocks(
    document: &pdfdelta_core::model::Document<pdfdelta_core::model::Glyph>,
) -> Result<Vec<BlockText>, String> {
    use pdfdelta_core::{
        layout::{BlockId, BlockRole},
        model::DecodedText,
        normalize::{MappedText, SourceMapEntry, TextSource, UnmappedToken},
    };

    const MAX_TOKENS: usize = 2_000_000;
    if document.items().len() > MAX_TOKENS {
        return Err("source paint-order glyph limit".into());
    }
    let mut glyphs: Vec<_> = document.items().iter().collect();
    glyphs.sort_unstable_by_key(|glyph| (glyph.page, glyph.render_order));
    if glyphs
        .windows(2)
        .any(|pair| (pair[0].page, pair[0].render_order) == (pair[1].page, pair[1].render_order))
    {
        return Err("source paint order has duplicate ordinals on one page".into());
    }
    let mut blocks = Vec::new();
    let mut work = 0usize;
    for page in glyphs.chunk_by(|a, b| a.page == b.page) {
        let mut mapped = MappedText {
            text: String::new(),
            source_map: Vec::new(),
            unmapped: Vec::new(),
        };
        let mut position = 0usize;
        for glyph in page {
            let source = TextSource {
                atoms: [TextSourceAtom::Glyph(glyph.id)].into(),
            };
            match &glyph.text {
                DecodedText::Mapped(text) => {
                    // Byte charging bounds scanning even for multi-scalar glyphs.
                    work = work
                        .checked_add(text.len())
                        .filter(|work| *work <= MAX_TOKENS)
                        .ok_or("source paint-order text limit")?;
                    let length = text.chars().count();
                    if length == 0 {
                        return Err("source glyph has empty decoded text".into());
                    }
                    mapped.text.push_str(text);
                    mapped.source_map.push(SourceMapEntry {
                        output_range: ScalarRange {
                            start: position,
                            end: position + length,
                        },
                        source,
                    });
                    position += length;
                }
                DecodedText::Unmapped {
                    font_hash,
                    glyph_id,
                } => {
                    work = work
                        .checked_add(1)
                        .filter(|work| *work <= MAX_TOKENS)
                        .ok_or("source paint-order token limit")?;
                    mapped.unmapped.push(UnmappedToken {
                        scalar_index: position,
                        font_hash: font_hash.clone(),
                        glyph_id: *glyph_id,
                        source,
                    });
                }
            }
        }
        let matching_tokens = mapped
            .comparable_tokens()
            .map_err(|error| error.to_string())?;
        blocks.push(BlockText {
            block: BlockId(u64::from(page[0].page.0)),
            role: BlockRole::Body,
            raw: mapped.clone(),
            matching: mapped.text.clone(),
            canonical: mapped,
            matching_tokens,
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![page[0].page.0],
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: Some(Vec::new()),
        });
    }
    Ok(blocks)
}

pub struct PreparedRevisionSource {
    pub blocks: Vec<BlockText>,
    pub metadata: RevisionSourceMetadata,
}

#[derive(Debug, Serialize)]
pub struct RevisionSourceMetadata {
    pub sha256: String,
    pub extraction_complete: bool,
    pub extraction_issues: Vec<String>,
    pub glyph_count: usize,
    pub block_count: usize,
    pub source_view: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfdelta_core::{
        layout::{BlockId, BlockRole},
        model::GlyphId,
        normalize::{ComparableToken, MappedText, SourceMapEntry, TextSource},
    };

    fn block(text: &str) -> BlockText {
        let mapped = MappedText {
            text: text.into(),
            source_map: vec![SourceMapEntry {
                output_range: ScalarRange {
                    start: 0,
                    end: text.chars().count(),
                },
                source: TextSource {
                    atoms: [TextSourceAtom::Glyph(GlyphId(7))].into(),
                },
            }],
            unmapped: Vec::new(),
        };
        BlockText {
            block: BlockId(0),
            role: BlockRole::Body,
            raw: mapped.clone(),
            canonical: mapped,
            matching: text.into(),
            matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        }
    }

    fn expected() -> ExpectedDocument {
        serde_json::from_value(serde_json::json!({
            "version": 1, "pair": "selector-test", "reviewed_on": "2026-09-10",
            "annotation": "partial", "changes": [{
                "id": "edit", "kind": "replacement", "old_quote": "ffi", "new_quote": "ffl"
            }],
        }))
        .expect("annotation")
    }

    fn paint_fixture() -> Vec<u8> {
        let plan = crate::mutation::RenderPlan::new(
            vec![vec!["first".into(), "second".into()], vec!["third".into()]],
            12,
        )
        .expect("page plan");
        crate::renderers::RendererKind::LopdfTj
            .render(&plan, Default::default())
            .expect("render source fixture")
    }

    #[test]
    fn paint_order_has_literal_page_streams_and_stable_sources_under_storage_permutation() {
        use pdfdelta_core::{
            model::Document,
            pdf::LopdfParser,
            source::{ContentStreamGlyphExtractor, ParserBackedGlyphSource},
        };

        let bytes = paint_fixture();
        let prepared = prepare_with_view(bytes.clone(), RevisionSourceView::PaintOrder)
            .expect("raw source view");
        assert_eq!(
            prepared.metadata.source_view,
            "native_page_paint_order_scalars_v1"
        );
        assert_eq!(prepared.blocks.len(), 2);
        assert_eq!(prepared.blocks[0].canonical.text, "firstsecond");
        assert_eq!(prepared.blocks[1].canonical.text, "third");
        assert_eq!(prepared.blocks[0].pages, [0]);
        assert_eq!(prepared.blocks[1].pages, [1]);

        let outcome = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor)
            .extract_outcome(bytes.into(), Default::default(), Default::default())
            .expect("source extraction");
        let mut glyphs = outcome.document().items().to_vec();
        glyphs.reverse();
        assert_eq!(
            paint_order_blocks(&Document::new(glyphs.clone())).expect("permuted storage"),
            prepared.blocks,
        );
        // Paint order is evidence, so a duplicate cannot be broken by an ID tie.
        glyphs[1].render_order = glyphs[0].render_order;
        glyphs[1].page = glyphs[0].page;
        assert!(paint_order_blocks(&Document::new(glyphs)).is_err());
    }

    #[test]
    fn paint_order_retains_unmapped_glyphs_and_rejects_empty_or_oversized_text() {
        use pdfdelta_core::{
            model::{DecodedText, Document, FontProgramHash},
            pdf::LopdfParser,
            source::{ContentStreamGlyphExtractor, ParserBackedGlyphSource},
        };

        let outcome = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor)
            .extract_outcome(
                paint_fixture().into(),
                Default::default(),
                Default::default(),
            )
            .expect("source extraction");
        let mut glyphs = outcome.document().items().to_vec();
        glyphs[0].text = DecodedText::Unmapped {
            font_hash: FontProgramHash(vec![1; 32]),
            glyph_id: 9,
        };
        let blocks = paint_order_blocks(&Document::new(glyphs.clone())).expect("unmapped evidence");
        assert_eq!(blocks[0].canonical.unmapped.len(), 1);
        assert_eq!(blocks[0].canonical.unmapped[0].scalar_index, 0);
        assert_eq!(blocks[0].canonical.text, "irstsecond");
        assert_eq!(blocks[0].matching_tokens.len(), 11);
        glyphs[0].text = DecodedText::Mapped(String::new());
        assert!(paint_order_blocks(&Document::new(glyphs.clone())).is_err());
        glyphs[0].text = DecodedText::Mapped("x".repeat(2_000_001));
        assert!(paint_order_blocks(&Document::new(glyphs)).is_err());
    }

    #[test]
    fn expanded_glyph_preserves_three_scalar_coordinates() {
        let result = validate(&expected(), &[block("ffi")], &[block("ffl")]);
        let old = &result.expectations[0].old;
        assert_eq!(old.status, "unique_source_selector");
        assert_eq!(
            old.sources
                .iter()
                .map(|source| source.scalar)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(
            old.sources
                .iter()
                .all(|source| matches!(source.atoms.as_slice(), [SelectorAtom::Glyph { id: 7 }]))
        );
    }

    #[test]
    fn duplicate_missing_and_sourceless_selectors_are_not_unique() {
        for (mut input, status) in [
            (block("ffi ffi"), "ambiguous"),
            (block("other"), "missing"),
            (block("ffi"), "source_missing"),
        ] {
            if status == "source_missing" {
                input.canonical.source_map.clear();
            }
            let result = validate(&expected(), &[input], &[block("ffl")]);
            assert_eq!(result.expectations[0].old.status, status);
            assert!(result.expectations[0].old.sources.is_empty());
        }
    }

    #[test]
    fn unique_scope_excludes_duplicate_quotes_outside_its_boundaries() {
        let mut annotation = expected();
        annotation.changes[0].scope = Some("reviewed".into());
        annotation.scopes = serde_json::from_value(serde_json::json!([{
            "id": "reviewed",
            "old": { "start_quote": "left", "end_quote": "right" },
            "new": { "start_quote": "left", "end_quote": "right" },
        }]))
        .expect("scope");
        let result = validate(
            &annotation,
            &[block("left ffi right ffi")],
            &[block("left ffl right ffl")],
        );
        assert_eq!(result.expectations[0].old.status, "unique_source_selector");
        assert_eq!(result.expectations[0].old.sources[0].scalar, 5);
        let invalid = validate(
            &annotation,
            &[block("left ffi right left")],
            &[block("left ffl right")],
        );
        assert!(invalid.expectations[0].changed_range_error.is_some());
        assert!(invalid.expectations[0].old.sources.is_empty());
    }
}
