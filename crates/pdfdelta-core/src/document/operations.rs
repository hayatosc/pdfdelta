use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    Result,
    diff::{LocalTextClaims, LocalTextSide, local_text_claims},
    normalize::ComparableToken,
};

use super::{
    EvidenceStore, FieldValue, GraphNode, NodeContent, NodeId, SourceRef, TextNormalization,
    TextView, evidence::bounded,
};

/// The correspondence premise is distinct from facts about the supplied values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterpretationStatus {
    ConditionalOnCorrespondence,
    Inferred,
}

/// A display unit is not a claim that every character in that unit changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TypedOperation {
    TextChanged {
        old: Option<String>,
        new: Option<String>,
    },
    ValueChanged {
        old: FieldValue,
        new: FieldValue,
    },
    RenderedRegionChanged,
    PageRenderingChanged,
}

/// Original token positions retain their evidence, including ligature/shared
/// glyph references. Counting distinct references is not a character count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedToken {
    pub position: usize,
    pub sources: Vec<SourceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactTextMask {
    pub convention: String,
    pub claims: LocalTextClaims,
    pub old: Vec<ChangedToken>,
    pub new: Vec<ChangedToken>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRun {
    pub row: u32,
    pub start: u32,
    pub end: u32,
}

/// Sample differences in the same declared raster grid and rendering profile.
/// This mask does not infer characters or a semantic change in an illustration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactPixelMask {
    pub width: u32,
    pub height: u32,
    pub changed_pixels: usize,
    pub runs: Vec<PixelRun>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalViewComparison {
    /// Ordered members of the compared view; masks index their concatenated text.
    pub old: Vec<NodeId>,
    pub new: Vec<NodeId>,
    pub interpretation: InterpretationStatus,
    pub operation: Option<TypedOperation>,
    pub text_mask: Option<ExactTextMask>,
    /// A source-count witness can prove change without localizing an exact mask.
    /// Only non-owning ranges use this relaxation of literal-minimal alignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_change_proof: Option<SourceTokenMultiplicity>,
    pub pixel_mask: Option<ExactPixelMask>,
    /// A content change can be known even when its character mask is unresolved.
    pub unresolved: Vec<String>,
    /// False if the selected local comparison could not determine equality/change.
    pub compared: bool,
}

/// Minimum source-backed and maximum total occurrences over all independently
/// optional source-normalization choices. A mandatory count exceeding the other
/// side's possible count proves change, but identifies no changed position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTokenMultiplicity {
    pub token: ComparableToken,
    pub old_required: usize,
    pub old_possible: usize,
    pub new_required: usize,
    pub new_possible: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalComparisonLimits {
    pub max_tokens: usize,
    pub max_pixels: usize,
    pub max_pixel_runs: usize,
    pub proof_work: usize,
}

impl Default for LocalComparisonLimits {
    fn default() -> Self {
        Self {
            max_tokens: 100_000,
            max_pixels: 16_000_000,
            max_pixel_runs: 100_000,
            proof_work: 1_000_000,
        }
    }
}

/// Compares one already proposed, source-validated pair without accepting its
/// correspondence. Graph/evidence validation and common ownership resolution
/// must precede publishing these results. No caller may infer correspondence
/// from a favorable local diff. Container relations are compared by the graph.
///
/// # Errors
/// Returns resource-limit errors for oversized local inputs and contextual
/// errors from the existing exact text kernel. Incomparable render grids and
/// unresolved normalization are retained as local unresolved results.
pub fn compare_local_views(
    old: &GraphNode,
    new: &GraphNode,
    old_store: &EvidenceStore,
    new_store: &EvidenceStore,
    limits: LocalComparisonLimits,
) -> Result<LocalViewComparison> {
    let mut result = LocalViewComparison {
        old: vec![old.id],
        new: vec![new.id],
        interpretation: if old.basis.is_inferred() || new.basis.is_inferred() {
            InterpretationStatus::Inferred
        } else {
            InterpretationStatus::ConditionalOnCorrespondence
        },
        operation: None,
        text_mask: None,
        text_change_proof: None,
        pixel_mask: None,
        unresolved: Vec::new(),
        compared: false,
    };
    let mut remaining_work = limits.proof_work;
    match (&old.content, &new.content) {
        (NodeContent::Text { view: a }, NodeContent::Text { view: b }) => {
            compare_text_content(&mut result, a, b, limits, &mut remaining_work)?;
        }
        (NodeContent::Value { value: a }, NodeContent::Value { value: b }) => {
            if matches!(a, FieldValue::Unresolved { .. })
                || matches!(b, FieldValue::Unresolved { .. })
            {
                result
                    .unresolved
                    .push("stored field value interpretation is unresolved".into());
                return Ok(result);
            }
            result.compared = true;
            if a != b {
                result.operation = Some(TypedOperation::ValueChanged {
                    old: a.clone(),
                    new: b.clone(),
                });
                if let (FieldValue::Text(a), FieldValue::Text(b)) = (a, b) {
                    let a = field_text(a, &old.sources, limits)?;
                    let b = field_text(b, &new.sources, limits)?;
                    result.text_mask =
                        compare_text(&a, &b, limits, &mut remaining_work, &mut result.unresolved)?;
                }
            }
        }
        (NodeContent::Visual { region: a }, NodeContent::Visual { region: b }) => {
            let a = old_store.rendered.iter().find(|region| region.id == *a);
            let b = new_store.rendered.iter().find(|region| region.id == *b);
            match (a, b) {
                (Some(a), Some(b))
                    if old_store.backends.get(a.backend) == new_store.backends.get(b.backend)
                        && old_store.backends.get(a.backend).is_some()
                        && a.raster.width == b.raster.width
                        && a.raster.height == b.raster.height
                        && a.composited_page == b.composited_page =>
                {
                    result.pixel_mask = compare_pixels(&a.raster, &b.raster, limits)?;
                    if let Some(mask) = &result.pixel_mask {
                        result.compared = true;
                        if mask.changed_pixels > 0 {
                            result.operation = Some(if a.composited_page {
                                TypedOperation::PageRenderingChanged
                            } else {
                                TypedOperation::RenderedRegionChanged
                            });
                        }
                    } else {
                        result
                            .unresolved
                            .push("pixel mask output limit reached".into());
                    }
                }
                _ => result.unresolved.push(
                    "render regions require the same declared profile and sample grid".into(),
                ),
            }
        }
        _ => result.unresolved.push(
            "local content types require a structural comparison or additional evidence".into(),
        ),
    }
    Ok(result)
}

/// Compares an ordered split/merge already validated by the common solver.
/// Concatenation preserves every token and origin without inserting separators
/// or choosing a normalization because it reduces the diff.
///
/// # Errors
/// Rejects empty/nontext groups, unresolved normalization, invalid token origins,
/// and local token/reference budgets. The caller must validate source ownership
/// and the retained order relations before publishing the result.
pub fn compare_text_group_views(
    old: &[&GraphNode],
    new: &[&GraphNode],
    limits: LocalComparisonLimits,
) -> Result<LocalViewComparison> {
    compare_text_groups(old, new, limits, false)
}

/// Layout separators retain their neighboring sources but do not establish a
/// literal word boundary. A non-owning native range quantifies over retaining
/// or removing these separators; literal space glyphs remain mandatory tokens.
/// An empty side is admitted only after the caller proves the corresponding
/// source interval empty. It is not a missing acquisition or a document dummy.
pub(super) fn compare_native_text_range(
    old: &[&GraphNode],
    new: &[&GraphNode],
    limits: LocalComparisonLimits,
) -> Result<LocalViewComparison> {
    compare_text_groups(old, new, limits, true)
}

/// Private-use scalars require a font-specific interpretation before they can
/// establish text identity across revisions. Preserve their raw evidence.
pub(super) fn private_use_scalar(scalar: char) -> bool {
    matches!(scalar, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{ffffd}' | '\u{100000}'..='\u{10fffd}')
}

fn compare_text_groups(
    old: &[&GraphNode],
    new: &[&GraphNode],
    limits: LocalComparisonLimits,
    layout_space_alternatives: bool,
) -> Result<LocalViewComparison> {
    if !layout_space_alternatives && (old.is_empty() || new.is_empty()) {
        return Err(super::evidence::invalid("empty local text group"));
    }
    let mut tokens = 0;
    let mut references = 0;
    let mut a = concatenate_text(old, limits, &mut tokens, &mut references)?;
    let mut b = concatenate_text(new, limits, &mut tokens, &mut references)?;
    let mut uncertain_spacing = false;
    let mut optional_normalization = false;
    if layout_space_alternatives {
        for view in [&mut a, &mut b] {
            let mut optional = view.optional_tokens().ok_or_else(|| {
                crate::Error::Unresolved("native range normalization is not validated".into())
            })?;
            for ((token, backed), position) in view
                .tokens
                .iter()
                .zip(&view.source_backed)
                .zip(&mut optional)
            {
                if token.as_scalar().is_none_or(private_use_scalar) {
                    return Err(crate::Error::Unresolved(
                        "unmapped or private-use glyph identities do not prove a native text change".into(),
                    ));
                }
                if token
                    .as_scalar()
                    .is_some_and(|scalar| scalar.is_ascii_whitespace())
                    && !backed
                {
                    *position = true;
                    uncertain_spacing = true;
                }
                optional_normalization |= *position;
            }
            if uncertain_spacing {
                view.bind_optional_positions(
                    optional
                        .into_iter()
                        .enumerate()
                        .filter_map(|(position, optional)| optional.then_some(position))
                        .collect(),
                );
            }
        }
    }
    let mut result = LocalViewComparison {
        old: old.iter().map(|node| node.id).collect(),
        new: new.iter().map(|node| node.id).collect(),
        interpretation: if old.iter().chain(new).any(|node| node.basis.is_inferred()) {
            InterpretationStatus::Inferred
        } else {
            InterpretationStatus::ConditionalOnCorrespondence
        },
        operation: None,
        text_mask: None,
        text_change_proof: None,
        pixel_mask: None,
        unresolved: Vec::new(),
        compared: false,
    };
    // Optional families reserve a count witness before exploring masks. Exact
    // ranges retain their full mask budget and use only unspent grid work for
    // a fallback after the exact kernel declines an unaffordable grid.
    let mut remaining_work = limits.proof_work;
    let mut multiplicity_change = (layout_space_alternatives && optional_normalization)
        .then(|| source_multiplicity_change(&a, &b, &mut remaining_work))
        .flatten();
    match compare_text_content(&mut result, &a, &b, limits, &mut remaining_work) {
        Err(error @ (crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_)))
            if layout_space_alternatives =>
        {
            result.unresolved.push(error.to_string());
        }
        outcome => outcome?,
    }
    if !result.compared && layout_space_alternatives && !optional_normalization {
        multiplicity_change = source_multiplicity_change(&a, &b, &mut remaining_work);
    }
    if !result.compared
        && let Some(proof) = multiplicity_change
    {
        result.compared = true;
        result.text_change_proof = Some(proof);
        result.operation = Some(TypedOperation::TextChanged {
            old: a.display_text(),
            new: b.display_text(),
        });
        result.unresolved.push(
            "source token multiplicity proves change; exact normalization masks remain unresolved"
                .into(),
        );
    }
    if uncertain_spacing {
        result
            .unresolved
            .push("layout-derived space boundaries retain both spacing interpretations".into());
    }
    Ok(result)
}

/// Any mandatory source token in excess of every possible counterpart occurrence
/// must remain unmatched under every permitted interpretation and alignment.
/// The converse is deliberately not used: overlapping count intervals prove
/// neither equality nor the absence of a spacing change. Spaces are excluded
/// from this fallback witness: a native layout may also have omitted a separator
/// at an implicit glyph gap. Literal-space changes still use the exact path when
/// there is no unresolved layout spacing. This proof has no mask.
fn source_multiplicity_change(
    old: &TextView,
    new: &TextView,
    remaining: &mut usize,
) -> Option<SourceTokenMultiplicity> {
    let count = old.tokens.len().checked_add(new.tokens.len())?;
    let work = count.checked_mul((count.saturating_add(1).ilog2() as usize + 1) * 4)?;
    let Some(next) = remaining.checked_sub(work) else {
        *remaining = 0;
        return None;
    };
    *remaining = next;
    let mut counts = BTreeMap::<&ComparableToken, [[usize; 2]; 2]>::new();
    for (side, view) in [old, new].into_iter().enumerate() {
        for ((token, backed), optional) in view
            .tokens
            .iter()
            .zip(&view.source_backed)
            .zip(view.optional_tokens()?)
        {
            let entry = counts.entry(token).or_default();
            entry[side][0] += usize::from(*backed && !optional);
            entry[side][1] += 1;
        }
    }
    counts.into_iter().find_map(|(token, [old, new])| {
        (!token
            .as_scalar()
            .is_some_and(|scalar| scalar.is_ascii_whitespace())
            && (old[0] > new[1] || new[0] > old[1]))
            .then(|| SourceTokenMultiplicity {
                token: token.clone(),
                old_required: old[0],
                old_possible: old[1],
                new_required: new[0],
                new_possible: new[1],
            })
    })
}

/// Recheck an already compared native range using an order-independent witness.
/// Whitespace cannot supply this witness, and optional source tokens retain
/// their full count intervals. The caller must discard the ordered mask.
pub(super) fn native_count_change(
    old: &[&GraphNode],
    new: &[&GraphNode],
    limits: LocalComparisonLimits,
    remaining: &mut usize,
) -> Result<Option<SourceTokenMultiplicity>> {
    let mut tokens = 0;
    let mut references = 0;
    let a = concatenate_text(old, limits, &mut tokens, &mut references)?;
    let b = concatenate_text(new, limits, &mut tokens, &mut references)?;
    let work = tokens
        .saturating_add(references)
        .saturating_add(old.len())
        .saturating_add(new.len());
    let Some(next) = remaining.checked_sub(work) else {
        *remaining = 0;
        return Ok(None);
    };
    *remaining = next;
    Ok(source_multiplicity_change(&a, &b, remaining))
}

fn concatenate_text(
    nodes: &[&GraphNode],
    limits: LocalComparisonLimits,
    tokens: &mut usize,
    references: &mut usize,
) -> Result<TextView> {
    bounded(nodes.len(), limits.max_tokens, "local group members")?;
    let mut result = TextView {
        tokens: Vec::new(),
        origins: Vec::new(),
        source_backed: Vec::new(),
        normalization: TextNormalization::Exact,
    };
    let mut optional_positions = Vec::new();
    for node in nodes {
        let NodeContent::Text { view } = &node.content else {
            return Err(crate::Error::Unresolved(
                "group contains a nontext view".into(),
            ));
        };
        if view.tokens.len() != view.origins.len() || view.tokens.len() != view.source_backed.len()
        {
            return Err(super::evidence::invalid(
                "group token origin dimensions differ",
            ));
        }
        *tokens = tokens.saturating_add(view.tokens.len());
        bounded(*tokens, limits.max_tokens, "local group text tokens")?;
        for origin in &view.origins {
            *references = references.saturating_add(origin.len());
            bounded(
                *references,
                limits.max_tokens,
                "local group source references",
            )?;
        }
        let optional = view.optional_tokens().ok_or_else(|| {
            crate::Error::Unresolved(
                "group text normalization has no source-validated interpretation family".into(),
            )
        })?;
        optional_positions.extend(
            optional
                .into_iter()
                .enumerate()
                .filter(|(_, optional)| *optional)
                .map(|(position, _)| result.tokens.len() + position),
        );
        result.tokens.extend_from_slice(&view.tokens);
        result.origins.extend_from_slice(&view.origins);
        result.source_backed.extend_from_slice(&view.source_backed);
    }
    if !optional_positions.is_empty() {
        result.bind_optional_positions(optional_positions);
    }
    Ok(result)
}

fn compare_text_content(
    result: &mut LocalViewComparison,
    old: &TextView,
    new: &TextView,
    limits: LocalComparisonLimits,
    remaining_work: &mut usize,
) -> Result<()> {
    result.text_mask = compare_text(old, new, limits, remaining_work, &mut result.unresolved)?;
    if let Some(mask) = &result.text_mask {
        result.compared = true;
        if mask.claims.changed_source_lower > 0 {
            result.operation = Some(TypedOperation::TextChanged {
                old: old.display_text(),
                new: new.display_text(),
            });
        } else if mask.claims.changed_source_upper > 0 {
            result.compared = false;
            result
                .unresolved
                .push("permitted normalization interpretations disagree on text equality".into());
        }
    }
    Ok(())
}

fn field_text(
    text: &str,
    sources: &[SourceRef],
    limits: LocalComparisonLimits,
) -> Result<TextView> {
    let count = text.chars().count();
    bounded(count, limits.max_tokens, "local text tokens")?;
    let references = count.saturating_mul(sources.len());
    bounded(
        references,
        limits.max_tokens,
        "local field source references",
    )?;
    Ok(TextView {
        tokens: text.chars().map(ComparableToken::Scalar).collect(),
        origins: vec![sources.to_vec(); count],
        source_backed: vec![true; count],
        normalization: TextNormalization::Exact,
    })
}

fn compare_text(
    old: &TextView,
    new: &TextView,
    limits: LocalComparisonLimits,
    remaining_work: &mut usize,
    unresolved: &mut Vec<String>,
) -> Result<Option<ExactTextMask>> {
    bounded(
        old.tokens.len().saturating_add(new.tokens.len()),
        limits.max_tokens,
        "local text tokens",
    )?;
    if old.origins.len() != old.tokens.len() || new.origins.len() != new.tokens.len() {
        return Err(super::evidence::invalid(
            "local text origin dimensions differ from tokens",
        ));
    }
    // Alternatives need a source-side normalization certificate. A bare external
    // list of optional characters is not such a certificate.
    let (Some(old_optional), Some(new_optional)) = (old.optional_tokens(), new.optional_tokens())
    else {
        unresolved
            .push("local text normalization has no source-validated interpretation family".into());
        return Ok(None);
    };
    let claims = local_text_claims(
        LocalTextSide {
            tokens: &old.tokens,
            source: &old.source_backed,
            optional: &old_optional,
        },
        LocalTextSide {
            tokens: &new.tokens,
            source: &new.source_backed,
            optional: &new_optional,
        },
        remaining_work,
    )?;
    let Some(claims) = claims else {
        unresolved.push("local character proof did not finish within its work budget".into());
        return Ok(None);
    };
    let project = |mask: &[bool], view: &TextView| {
        mask.iter()
            .enumerate()
            .filter(|(_, changed)| **changed)
            .map(|(position, _)| ChangedToken {
                position,
                sources: view.origins[position].clone(),
            })
            .collect()
    };
    Ok(Some(ExactTextMask {
        convention: "literal-minimal-source-tokens-v1".into(),
        old: project(&claims.mandatory_old, old),
        new: project(&claims.mandatory_new, new),
        claims,
    }))
}

fn compare_pixels(
    old: &super::Raster,
    new: &super::Raster,
    limits: LocalComparisonLimits,
) -> Result<Option<ExactPixelMask>> {
    let pixels = (old.width as usize).saturating_mul(old.height as usize);
    bounded(pixels, limits.max_pixels, "local raster pixels")?;
    if old.width == 0
        || old.height == 0
        || old.rgb.len() != pixels.saturating_mul(3)
        || new.rgb.len() != old.rgb.len()
    {
        return Err(super::evidence::invalid("invalid local raster payload"));
    }
    let mut mask = ExactPixelMask {
        width: old.width,
        height: old.height,
        changed_pixels: 0,
        runs: Vec::new(),
    };
    for row in 0..old.height {
        let mut start = None;
        for column in 0..=old.width {
            let offset = (row as usize * old.width as usize + column as usize) * 3;
            let changed =
                column < old.width && old.rgb[offset..offset + 3] != new.rgb[offset..offset + 3];
            if changed {
                mask.changed_pixels += 1;
                start.get_or_insert(column);
            } else if let Some(start) = start.take() {
                if mask.runs.len() == limits.max_pixel_runs {
                    return Ok(None);
                }
                mask.runs.push(PixelRun {
                    row,
                    start,
                    end: column,
                });
            }
        }
    }
    Ok(Some(mask))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplicity_witness_implies_change_in_every_exact_normalization_pair() {
        let mut views = Vec::new();
        for length in 0..=3 {
            for encoding in 0..4usize.pow(length) {
                let mut digits = encoding;
                let mut view = TextView {
                    tokens: Vec::new(),
                    origins: Vec::new(),
                    source_backed: Vec::new(),
                    normalization: TextNormalization::Exact,
                };
                let mut optional = Vec::new();
                for position in 0..length as usize {
                    let digit = digits % 4;
                    digits /= 4;
                    view.tokens
                        .push(ComparableToken::Scalar(if digit < 2 { 'a' } else { ' ' }));
                    view.source_backed.push(digit != 3);
                    view.origins.push(vec![]);
                    if digit % 2 == 1 {
                        optional.push(position);
                    }
                }
                view.bind_optional_positions(optional);
                views.push(view);
            }
        }
        for old in &views {
            for new in &views {
                let Some(proof) = source_multiplicity_change(old, new, &mut 100_000) else {
                    continue;
                };
                let old_optional = old.optional_tokens().expect("validated test family");
                let new_optional = new.optional_tokens().expect("validated test family");
                let claims = local_text_claims(
                    LocalTextSide {
                        tokens: &old.tokens,
                        source: &old.source_backed,
                        optional: &old_optional,
                    },
                    LocalTextSide {
                        tokens: &new.tokens,
                        source: &new.source_backed,
                        optional: &new_optional,
                    },
                    &mut 100_000,
                )
                .expect("exact small proof")
                .expect("complete exact small proof");
                assert!(claims.changed_source_lower > 0, "unsound witness {proof:?}");
            }
        }
        assert!(source_multiplicity_change(&views[1], &views[2], &mut 0).is_none());
    }
}
