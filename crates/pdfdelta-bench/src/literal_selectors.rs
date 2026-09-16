//! Version-2 literal source coordinates, resolved independently of comparison.

use pdfdelta_core::normalize::ScalarRange;
use serde::{Deserialize, Serialize};

use crate::revisions::revision_selectors::{
    PreparedRevisionSource, RevisionSourceView, SelectorAtom,
};

pub const MAX_ANNOTATION_BYTES: usize = 4 * 1024 * 1024;
const MAX_WORK: usize = 64_000_000;
const MAX_OUTPUT_ATOMS: usize = 65_536;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LiteralAnnotation {
    version: u32,
    pair: String,
    old_sha256: String,
    new_sha256: String,
    source_view: LiteralView,
    normalization: LiteralNormalization,
    selectors: Vec<LiteralSelector>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum LiteralView {
    LayoutRaw,
    PaintOrderRaw,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum LiteralNormalization {
    None,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Side {
    Old,
    New,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PositionUnit {
    UnicodeScalar,
    Utf8Byte,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LiteralSelector {
    id: String,
    side: Side,
    locator: Locator,
    literal_quote: String,
    position_unit: PositionUnit,
}

/// Page numbers are zero-based; ranges are half-open within a raw block.
/// A quote never bridges blocks, and an explicit range requires a block ID.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Locator {
    page: Option<u32>,
    block: Option<u64>,
    range: Option<Offsets>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Offsets {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    Unique,
    Missing,
    Ambiguous,
    InputHashMismatch,
    SourceViewMismatch,
    UnmappedSource,
    MissingSource,
    BudgetExceeded,
}

#[derive(Debug, Serialize)]
pub struct LiteralResolution {
    pub id: String,
    pub status: ResolutionStatus,
    /// At most two witnesses; two suffice to refute uniqueness.
    pub occurrences: Vec<LiteralOccurrence>,
    pub sources: Vec<LiteralScalarSource>,
}

#[derive(Debug, Serialize)]
pub struct LiteralOccurrence {
    pub block: u64,
    /// Block context, not a scalar bounding-box assertion.
    pub block_pages: Vec<u32>,
    pub unicode_scalar: Offsets,
    pub utf8_byte: Offsets,
}

#[derive(Debug, Serialize)]
pub struct LiteralScalarSource {
    pub scalar: usize,
    pub utf8_byte: usize,
    pub value: char,
    pub atoms: Vec<SelectorAtom>,
}

impl LiteralAnnotation {
    /// Reads the literal contract without changing version-1 selectors.
    ///
    /// # Errors
    /// Rejects oversized, malformed or unsupported annotations, invalid hashes,
    /// locators, duplicate IDs and empty quotes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_ANNOTATION_BYTES {
            return Err("literal annotation byte limit".into());
        }
        let annotation: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if annotation.version != 2 || annotation.pair.is_empty() {
            return Err("literal annotations require version 2 and a pair ID".into());
        }
        for hash in [&annotation.old_sha256, &annotation.new_sha256] {
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("literal annotation requires lowercase SHA-256 hashes".into());
            }
        }
        if annotation.selectors.len() > 1024 {
            return Err("literal selector count limit".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for selector in &annotation.selectors {
            if selector.id.is_empty() || !ids.insert(&selector.id) {
                return Err("literal selector IDs must be nonempty and unique".into());
            }
            if selector.literal_quote.is_empty() || selector.literal_quote.len() > 65_536 {
                return Err("literal quote must contain 1..=65536 UTF-8 bytes".into());
            }
            if selector
                .locator
                .range
                .is_some_and(|r| r.start >= r.end || selector.locator.block.is_none())
            {
                return Err("literal ranges require a block and nonempty half-open offsets".into());
            }
        }
        Ok(annotation)
    }

    #[must_use]
    pub fn preparation(&self) -> RevisionSourceView {
        match self.source_view {
            LiteralView::LayoutRaw => RevisionSourceView::Layout,
            LiteralView::PaintOrderRaw => RevisionSourceView::PaintOrder,
        }
    }

    /// Unique coordinates certify neither counterpart identity nor extraction
    /// completeness. Prepared-source metadata retains acquisition gaps.
    #[must_use]
    pub fn resolve(
        &self,
        old: &PreparedRevisionSource,
        new: &PreparedRevisionSource,
    ) -> Vec<LiteralResolution> {
        let mut work = MAX_WORK;
        let mut output_atoms = MAX_OUTPUT_ATOMS;
        self.selectors
            .iter()
            .map(|selector| {
                if old.metadata.sha256 != self.old_sha256 || new.metadata.sha256 != self.new_sha256
                {
                    return LiteralResolution {
                        id: selector.id.clone(),
                        status: ResolutionStatus::InputHashMismatch,
                        occurrences: Vec::new(),
                        sources: Vec::new(),
                    };
                }
                let (source, hash) = match selector.side {
                    Side::Old => (old, &self.old_sha256),
                    Side::New => (new, &self.new_sha256),
                };
                resolve_selector(
                    selector,
                    source,
                    hash,
                    self.source_view,
                    &mut work,
                    &mut output_atoms,
                )
            })
            .collect()
    }
}

fn charge(remaining: &mut usize, amount: usize) -> bool {
    if let Some(next) = remaining.checked_sub(amount) {
        *remaining = next;
        true
    } else {
        *remaining = 0;
        false
    }
}

fn resolve_selector(
    selector: &LiteralSelector,
    source: &PreparedRevisionSource,
    hash: &str,
    view: LiteralView,
    work: &mut usize,
    output_atoms: &mut usize,
) -> LiteralResolution {
    let mut result = LiteralResolution {
        id: selector.id.clone(),
        status: ResolutionStatus::Missing,
        occurrences: Vec::new(),
        sources: Vec::new(),
    };
    if source.metadata.sha256 != hash {
        result.status = ResolutionStatus::InputHashMismatch;
        return result;
    }
    let expected_view = match view {
        LiteralView::LayoutRaw => "default_native_layout_canonical_scalars_v1",
        LiteralView::PaintOrderRaw => "native_page_paint_order_scalars_v1",
    };
    if source.metadata.source_view != expected_view {
        result.status = ResolutionStatus::SourceViewMismatch;
        return result;
    }
    let mut source_bytes = 32 * 1024 * 1024;
    let mut matched_block = None;
    let quote_scalars = selector.literal_quote.chars().count();
    for block in &source.blocks {
        if !charge(work, 1) || !charge(&mut source_bytes, block.raw.text.len()) {
            result.status = ResolutionStatus::BudgetExceeded;
            return result;
        }
        if selector.locator.block.is_some_and(|id| id != block.block.0)
            || selector
                .locator
                .page
                .is_some_and(|page| !block.pages.contains(&page))
        {
            continue;
        }
        for (scalar, (byte, _)) in block.raw.text.char_indices().enumerate() {
            if !charge(work, selector.literal_quote.len()) {
                result.status = ResolutionStatus::BudgetExceeded;
                return result;
            }
            if !block.raw.text[byte..].starts_with(&selector.literal_quote) {
                continue;
            }
            let unicode_scalar = Offsets {
                start: scalar,
                end: scalar + quote_scalars,
            };
            let utf8_byte = Offsets {
                start: byte,
                end: byte + selector.literal_quote.len(),
            };
            let offsets = match selector.position_unit {
                PositionUnit::UnicodeScalar => unicode_scalar,
                PositionUnit::Utf8Byte => utf8_byte,
            };
            if selector.locator.range.is_some_and(|range| range != offsets) {
                continue;
            }
            matched_block = Some(block);
            result.occurrences.push(LiteralOccurrence {
                block: block.block.0,
                block_pages: block.pages.clone(),
                unicode_scalar,
                utf8_byte,
            });
            if result.occurrences.len() == 2 {
                result.status = ResolutionStatus::Ambiguous;
                return result;
            }
        }
    }
    let Some(block) = matched_block else {
        return result;
    };
    let occurrence = &result.occurrences[0];
    let range = occurrence.unicode_scalar;
    if !charge(work, block.raw.unmapped.len()) {
        result.status = ResolutionStatus::BudgetExceeded;
        return result;
    }
    // An unknown glyph at either boundary may belong to the quoted span.
    if block
        .raw
        .unmapped
        .iter()
        .any(|token| range.start <= token.scalar_index && token.scalar_index <= range.end)
    {
        result.status = ResolutionStatus::UnmappedSource;
        return result;
    }
    let projection_cost = block
        .raw
        .source_map
        .iter()
        .try_fold(0usize, |total, entry| {
            total.checked_add(entry.source.atoms.len().saturating_add(1))
        });
    let Some(cost) = projection_cost
        .and_then(|cost| cost.checked_mul(quote_scalars))
        .and_then(|cost| cost.checked_mul(2))
    else {
        result.status = ResolutionStatus::BudgetExceeded;
        return result;
    };
    if !charge(work, cost) {
        result.status = ResolutionStatus::BudgetExceeded;
        return result;
    }
    for (offset, (byte, value)) in selector.literal_quote.char_indices().enumerate() {
        let scalar = range.start + offset;
        let upper_atoms = block
            .raw
            .source_map
            .iter()
            .filter(|entry| entry.output_range.start <= scalar && scalar < entry.output_range.end)
            .try_fold(0usize, |total, entry| {
                total.checked_add(entry.source.atoms.len())
            });
        if upper_atoms.is_none_or(|count| count > *output_atoms) {
            result.sources.clear();
            result.status = ResolutionStatus::BudgetExceeded;
            return result;
        }
        let atoms = block.raw.project_source_atoms(ScalarRange {
            start: scalar,
            end: scalar + 1,
        });
        if atoms.is_empty() {
            result.sources.clear();
            result.status = ResolutionStatus::MissingSource;
            return result;
        }
        if !charge(output_atoms, atoms.len()) {
            result.sources.clear();
            result.status = ResolutionStatus::BudgetExceeded;
            return result;
        }
        result.sources.push(LiteralScalarSource {
            scalar,
            utf8_byte: occurrence.utf8_byte.start + byte,
            value,
            atoms: atoms.into_iter().map(SelectorAtom::from).collect(),
        });
    }
    result.status = ResolutionStatus::Unique;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revisions::revision_selectors::RevisionSourceMetadata;
    use pdfdelta_core::{
        layout::{BlockId, BlockRole},
        model::{FontProgramHash, GlyphId},
        normalize::{
            BlockText, ComparableToken, MappedText, SourceMapEntry, TextSource, TextSourceAtom,
            UnmappedToken,
        },
    };

    fn source(text: &str) -> PreparedRevisionSource {
        let mapped = MappedText {
            text: text.into(),
            source_map: vec![SourceMapEntry {
                output_range: ScalarRange {
                    start: 0,
                    end: text.chars().count(),
                },
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(7))].into(),
                },
            }],
            unmapped: Vec::new(),
        };
        PreparedRevisionSource {
            blocks: vec![BlockText {
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
            }],
            metadata: RevisionSourceMetadata {
                sha256: "a".repeat(64),
                extraction_complete: true,
                extraction_issues: Vec::new(),
                glyph_count: 1,
                block_count: 1,
                source_view: "default_native_layout_canonical_scalars_v1",
            },
        }
    }

    fn annotation(quote: &str) -> LiteralAnnotation {
        LiteralAnnotation::from_json(&serde_json::to_vec(&serde_json::json!({
            "version": 2, "pair": "literal", "old_sha256": "a".repeat(64), "new_sha256": "a".repeat(64),
            "source_view": "layout_raw", "normalization": "none",
            "selectors": [{"id": "change", "side": "old", "locator": {}, "literal_quote": quote,
                "position_unit": "unicode_scalar"}]
        })).expect("annotation JSON")).expect("valid annotation")
    }

    #[test]
    fn literal_whitespace_unicode_and_shared_glyphs_survive() {
        let source = source("日  ffi\n本");
        let result = annotation("  ffi\n").resolve(&source, &source);
        assert_eq!(result[0].status, ResolutionStatus::Unique);
        assert_eq!(
            result[0].occurrences[0].unicode_scalar,
            Offsets { start: 1, end: 7 }
        );
        assert_eq!(
            result[0].occurrences[0].utf8_byte,
            Offsets { start: 3, end: 9 }
        );
        assert_eq!(result[0].sources.len(), 6);
        assert!(
            result[0]
                .sources
                .iter()
                .all(|source| matches!(source.atoms.as_slice(), [SelectorAtom::Glyph { id: 7 }]))
        );
        assert_eq!(
            annotation("日 ffi").resolve(&source, &source)[0].status,
            ResolutionStatus::Missing
        );
        assert_eq!(
            annotation("日  ffi 本").resolve(&source, &source)[0].status,
            ResolutionStatus::Missing
        );
    }

    #[test]
    fn overlapping_and_cross_block_duplicates_do_not_pick_the_first_match() {
        let mut source = source("aaa");
        let result = annotation("aa").resolve(&source, &source);
        assert_eq!(result[0].status, ResolutionStatus::Ambiguous);
        assert_eq!(result[0].occurrences.len(), 2);
        assert!(result[0].sources.is_empty());
        let mut duplicate = source.blocks[0].clone();
        duplicate.block = BlockId(1);
        source.blocks.push(duplicate);
        assert_eq!(
            annotation("aaa").resolve(&source, &source)[0].status,
            ResolutionStatus::Ambiguous
        );
    }

    #[test]
    fn byte_locators_must_match_scalar_boundaries_and_exact_quote() {
        let source = source("日a日a");
        let mut annotation = annotation("日a");
        annotation.selectors[0].position_unit = PositionUnit::Utf8Byte;
        annotation.selectors[0].locator.block = Some(0);
        annotation.selectors[0].locator.range = Some(Offsets { start: 4, end: 8 });
        let result = annotation.resolve(&source, &source);
        assert_eq!(result[0].status, ResolutionStatus::Unique);
        assert_eq!(result[0].sources[0].scalar, 2);
        assert_eq!(result[0].sources[1].utf8_byte, 7);
        annotation.selectors[0].locator.range = Some(Offsets { start: 1, end: 5 });
        assert_eq!(
            annotation.resolve(&source, &source)[0].status,
            ResolutionStatus::Missing
        );
    }

    #[test]
    fn real_and_synthetic_spaces_keep_distinct_provenance() {
        let mut source = source("  ");
        source.blocks[0].raw.source_map = vec![
            SourceMapEntry {
                output_range: ScalarRange { start: 0, end: 1 },
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(1))].into(),
                },
            },
            SourceMapEntry {
                output_range: ScalarRange { start: 1, end: 2 },
                source: TextSource {
                    atoms: vec![TextSourceAtom::SyntheticSpace {
                        preceding: GlyphId(1),
                        following: GlyphId(2),
                    }]
                    .into(),
                },
            },
        ];
        let result = annotation("  ").resolve(&source, &source);
        assert_eq!(result[0].status, ResolutionStatus::Unique);
        assert!(matches!(
            result[0].sources[0].atoms[0],
            SelectorAtom::Glyph { id: 1 }
        ));
        assert!(matches!(
            result[0].sources[1].atoms[0],
            SelectorAtom::SyntheticSpace {
                preceding: 1,
                following: 2
            }
        ));
    }

    #[test]
    fn unknown_glyphs_and_missing_sources_cannot_be_empty_text() {
        let mut source = source("ab");
        source.blocks[0].raw.unmapped.push(UnmappedToken {
            scalar_index: 1,
            font_hash: FontProgramHash(vec![1]),
            glyph_id: 3,
            source: TextSource {
                atoms: vec![TextSourceAtom::Glyph(GlyphId(8))].into(),
            },
        });
        assert_eq!(
            annotation("ab").resolve(&source, &source)[0].status,
            ResolutionStatus::UnmappedSource
        );
        source.blocks[0].raw.unmapped.clear();
        source.blocks[0].raw.source_map.clear();
        assert_eq!(
            annotation("ab").resolve(&source, &source)[0].status,
            ResolutionStatus::MissingSource
        );
    }

    #[test]
    fn hash_view_and_budget_failures_remain_unresolved() {
        let mut source = source("ab");
        let annotation = annotation("ab");
        source.metadata.sha256 = "b".repeat(64);
        assert_eq!(
            annotation.resolve(&source, &source)[0].status,
            ResolutionStatus::InputHashMismatch
        );
        source.metadata.sha256 = "a".repeat(64);
        source.metadata.source_view = "native_page_paint_order_scalars_v1";
        assert_eq!(
            annotation.resolve(&source, &source)[0].status,
            ResolutionStatus::SourceViewMismatch
        );
        source.metadata.source_view = "default_native_layout_canonical_scalars_v1";
        let mut output_atoms = MAX_OUTPUT_ATOMS;
        let result = resolve_selector(
            &annotation.selectors[0],
            &source,
            &annotation.old_sha256,
            annotation.source_view,
            &mut 1,
            &mut output_atoms,
        );
        assert_eq!(result.status, ResolutionStatus::BudgetExceeded);
        assert!(result.sources.is_empty());
        let mut work = MAX_WORK;
        let result = resolve_selector(
            &annotation.selectors[0],
            &source,
            &annotation.old_sha256,
            annotation.source_view,
            &mut work,
            &mut 0,
        );
        assert_eq!(result.status, ResolutionStatus::BudgetExceeded);
        assert!(result.sources.is_empty());
    }

    #[test]
    fn version_normalization_and_locator_validation_is_explicit() {
        let mut value = serde_json::to_value(annotation("x")).expect("serialize");
        value["version"] = 1.into();
        assert!(LiteralAnnotation::from_json(&serde_json::to_vec(&value).expect("JSON")).is_err());
        value["version"] = 2.into();
        value["normalization"] = "collapse_whitespace".into();
        assert!(LiteralAnnotation::from_json(&serde_json::to_vec(&value).expect("JSON")).is_err());
        value["normalization"] = "none".into();
        value["selectors"][0]["locator"]["range"] = serde_json::json!({"start":0,"end":1});
        assert!(LiteralAnnotation::from_json(&serde_json::to_vec(&value).expect("JSON")).is_err());
    }
}
