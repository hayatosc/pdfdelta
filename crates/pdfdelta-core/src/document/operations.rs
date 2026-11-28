use serde::{Deserialize, Serialize};

use crate::{
    Result,
    normalize::ComparableToken,
    text_envelope::{LocalTextClaims, LocalTextSide, local_text_claims},
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
    pub pixel_mask: Option<ExactPixelMask>,
    /// A content change can be known even when its character mask is unresolved.
    pub unresolved: Vec<String>,
    /// Classification of each retained message, aligned by index. Older
    /// results carry none; a missing entry means the reason is unclassified,
    /// never that the message is absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligations: Vec<super::UnresolvedObligation>,
    /// False if the selected local comparison could not determine equality/change.
    pub compared: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalComparisonLimits {
    pub max_tokens: usize,
    pub max_pixels: usize,
    pub max_pixel_runs: usize,
    pub local_text_work: usize,
}

impl Default for LocalComparisonLimits {
    fn default() -> Self {
        Self {
            max_tokens: 100_000,
            max_pixels: 16_000_000,
            max_pixel_runs: 100_000,
            local_text_work: 1_000_000,
        }
    }
}

/// Compares one already proposed, source-validated pair without accepting its
/// correspondence. Graph/evidence validation and common ownership resolution
/// must precede publishing these results. No caller may infer correspondence
/// from a favorable local comparison. Container relations are compared by the graph.
///
/// Local value-field character evidence remains part of the serialized forms
/// contract. Native text-node comparisons are still computed when a caller
/// explicitly withholds supersession, but the CLI always lets flow own that
/// channel.
///
/// # Errors
/// Returns resource-limit errors for oversized text or raster inputs and
/// contextual errors for invalid render payloads. Incomparable render grids and
/// unresolved normalization remain local unresolved results.
pub fn compare_local_views(
    old: &GraphNode,
    new: &GraphNode,
    old_store: &EvidenceStore,
    new_store: &EvidenceStore,
    limits: LocalComparisonLimits,
) -> Result<LocalViewComparison> {
    let mut remaining_work = limits.local_text_work;
    let mut result = LocalViewComparison {
        obligations: Vec::new(),
        old: vec![old.id],
        new: vec![new.id],
        interpretation: if old.basis.is_inferred() || new.basis.is_inferred() {
            InterpretationStatus::Inferred
        } else {
            InterpretationStatus::ConditionalOnCorrespondence
        },
        operation: None,
        text_mask: None,
        pixel_mask: None,
        unresolved: Vec::new(),
        compared: false,
    };
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
                    result.text_mask = compare_text(
                        &a,
                        &b,
                        limits,
                        &mut remaining_work,
                        &mut super::UnresolvedSink::new(
                            &mut result.unresolved,
                            &mut result.obligations,
                        ),
                    )?;
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
                        result.retain_unresolved(
                            super::UnresolvedObligation::new(
                                super::UnresolvedReason::PixelMaskOutputLimit,
                            ),
                            "pixel mask output limit reached",
                        );
                    }
                }
                _ => result.retain_unresolved(
                    super::UnresolvedObligation::new(
                        super::UnresolvedReason::IncompatibleRenderProfile,
                    ),
                    "render regions require the same declared profile and sample grid",
                ),
            }
        }
        _ => result.retain_unresolved(
            super::UnresolvedObligation::new(super::UnresolvedReason::UnsupportedLocalContent),
            "local content types require a structural comparison or additional evidence",
        ),
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
    let mask = compare_text(
        old,
        new,
        limits,
        remaining_work,
        &mut super::UnresolvedSink::new(&mut result.unresolved, &mut result.obligations),
    )?;
    result.text_mask = mask;
    if let Some(mask) = &result.text_mask {
        result.compared = true;
        if mask.claims.changed_source_lower > 0 {
            result.operation = Some(TypedOperation::TextChanged {
                old: old.display_text(),
                new: new.display_text(),
            });
        } else if mask.claims.changed_source_upper > 0 {
            result.compared = false;
            result.retain_unresolved(
                super::UnresolvedObligation::new(
                    super::UnresolvedReason::NormalizationInterpretationsDisagree,
                ),
                "permitted normalization interpretations disagree on text equality",
            );
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
    unresolved: &mut super::UnresolvedSink<'_>,
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
        unresolved.retain(
            super::UnresolvedObligation::new(
                super::UnresolvedReason::NormalizationInterpretationMissing,
            ),
            "local text normalization has no source-validated interpretation family",
        );
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
        unresolved.retain(
            super::UnresolvedObligation::new(super::UnresolvedReason::LocalTextWorkBudget),
            "local character comparison did not finish within its work budget",
        );
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
