//! Projection of a verified flow comparison into neutral review evidence.
//!
//! The review planner lives in `pdfdelta-core` and cannot depend on the flow
//! engine's types. This module copies only the small metadata a packet needs
//! (token text references, page/bounds, source glyph identifiers, change
//! ranges, issue messages) and leaves the comparison untouched.

use std::sync::Arc;

use pdfdelta_core::{
    document::Channel,
    review::{
        BundleIdentity, EngineOutcome, EngineStatus, FlowAmbiguity, FlowChangeKind, FlowChangeView,
        FlowConfidence, FlowFormattingView, FlowIssueKind, FlowIssueView, FlowPageOrderView,
        FlowSideView, FlowTextReview, FlowTokenView, POLICY_VERSION, PipelineContract,
        REVIEW_SCHEMA,
    },
    source::{ExtractionIssue, ExtractionScope},
};
use pdfdelta_flow::{AmbiguityReason, Confidence, FlowChange, PageOrderEvidence};
use sha2::{Digest, Sha256};

use crate::{
    flow_cmd::{FlowComparison, Side},
    flow_exit::IMAGE_TEXT_UNEXAMINED,
};

/// Owned projections that a [`FlowTextReview`] borrows.
pub(crate) struct OwnedReview<'a> {
    old_tokens: Vec<FlowTokenView<'a>>,
    new_tokens: Vec<FlowTokenView<'a>>,
    old_issues: Vec<FlowIssueView>,
    new_issues: Vec<FlowIssueView>,
    old_order: Vec<FlowPageOrderView>,
    new_order: Vec<FlowPageOrderView>,
    changes: Vec<FlowChangeView>,
    formatting: Vec<FlowFormattingView>,
}

impl<'a> OwnedReview<'a> {
    /// Borrows the projections as the core planner's input.
    pub(crate) fn review(&'a self, comparison: &FlowComparison) -> FlowTextReview<'a> {
        FlowTextReview {
            old: FlowSideView {
                tokens: &self.old_tokens,
                issues: &self.old_issues,
                order: &self.old_order,
                conservation_incomplete: !comparison.old_side.stream.conservation.is_complete(),
            },
            new: FlowSideView {
                tokens: &self.new_tokens,
                issues: &self.new_issues,
                order: &self.new_order,
                conservation_incomplete: !comparison.new_side.stream.conservation.is_complete(),
            },
            changes: &self.changes,
            formatting: &self.formatting,
            diff_verified: true,
        }
    }
}

/// Projects a flow comparison into review views.
pub(crate) fn project(comparison: &FlowComparison) -> OwnedReview<'_> {
    let changes = comparison
        .diff
        .changes
        .iter()
        .enumerate()
        .filter(|(_, change)| {
            !matches!(
                change,
                FlowChange::LayoutOnly { .. } | FlowChange::ReadingOrder { .. }
            )
        })
        .enumerate()
        .map(|(report_index, (index, change))| {
            let assessment = comparison.assessments.get(index);
            FlowChangeView {
                index: report_index,
                kind: change_kind(change),
                old: change.old_range(),
                new: change.new_range(),
                confidence: assessment.map_or(FlowConfidence::High, |assessment| {
                    confidence(assessment.confidence)
                }),
                ambiguity: assessment.map_or_else(Vec::new, |assessment| {
                    assessment
                        .ambiguity
                        .iter()
                        .map(|reason| ambiguity(*reason))
                        .collect()
                }),
            }
        })
        .collect();
    let formatting = comparison
        .formatting
        .iter()
        .enumerate()
        .map(|(index, change)| FlowFormattingView {
            index,
            old: change.old.clone(),
            new: change.new.clone(),
            reasons: change.reasons.iter().map(|reason| reason.label()).collect(),
            old_size: change.old_size,
            new_size: change.new_size,
        })
        .collect();
    OwnedReview {
        old_tokens: tokens(&comparison.old_side),
        new_tokens: tokens(&comparison.new_side),
        old_issues: issues(&comparison.old_side),
        new_issues: issues(&comparison.new_side),
        old_order: order(&comparison.old_side),
        new_order: order(&comparison.new_side),
        changes,
        formatting,
    }
}

fn tokens(side: &Side) -> Vec<FlowTokenView<'_>> {
    side.stream
        .tokens
        .iter()
        .enumerate()
        .map(|(index, token)| FlowTokenView {
            text: &token.text,
            page: token.page,
            bounds: token.bounds,
            uncertain: token.uncertain,
            sources: side
                .stream
                .source_glyphs
                .get(index)
                .map_or(&[][..], Vec::as_slice),
        })
        .collect()
}

fn issues(side: &Side) -> Vec<FlowIssueView> {
    let total = side.issues.len();
    let paint_start = total.saturating_sub(side.paint_issues);
    side.issues
        .iter()
        .enumerate()
        .map(|(index, issue)| {
            let paint = index >= paint_start;
            FlowIssueView {
                kind: match issue.kind() {
                    pdfdelta_core::source::ExtractionIssueKind::Unsupported => {
                        FlowIssueKind::Unsupported
                    }
                    pdfdelta_core::source::ExtractionIssueKind::Unresolved => {
                        FlowIssueKind::Unresolved
                    }
                },
                page: issue_page(issue),
                unexamined: paint
                    || (issue.kind() == pdfdelta_core::source::ExtractionIssueKind::Unresolved
                        && issue.description().starts_with(IMAGE_TEXT_UNEXAMINED)),
                message: issue.description().to_owned(),
            }
        })
        .collect()
}

fn issue_page(issue: &ExtractionIssue) -> Option<pdfdelta_core::model::PageId> {
    match issue.scope() {
        ExtractionScope::Page(page) | ExtractionScope::PageGlyphGap { page, .. } => Some(page),
        _ => None,
    }
}

fn order(side: &Side) -> Vec<FlowPageOrderView> {
    side.order
        .iter()
        .filter(|order| order.uncertain)
        .map(order_view)
        .collect()
}

fn order_view(order: &PageOrderEvidence) -> FlowPageOrderView {
    FlowPageOrderView {
        page: order.page,
        uncertain: order.uncertain,
        agreement: order.agreement,
        coherence: order.coherence,
    }
}

fn change_kind(change: &FlowChange) -> FlowChangeKind {
    match change {
        FlowChange::Delete { .. } => FlowChangeKind::Delete,
        FlowChange::Insert { .. } => FlowChangeKind::Insert,
        FlowChange::Replace { .. } => FlowChangeKind::Replace,
        FlowChange::Move { .. } => FlowChangeKind::Move,
        FlowChange::ReadingOrder { .. } | FlowChange::LayoutOnly { .. } => FlowChangeKind::Replace,
    }
}

fn confidence(confidence: Confidence) -> FlowConfidence {
    match confidence {
        Confidence::High => FlowConfidence::High,
        Confidence::Medium => FlowConfidence::Medium,
        Confidence::Low => FlowConfidence::Low,
    }
}

fn ambiguity(reason: AmbiguityReason) -> FlowAmbiguity {
    match reason {
        AmbiguityReason::CoarseSpan => FlowAmbiguity::CoarseSpan,
        AmbiguityReason::RepeatedRun => FlowAmbiguity::RepeatedRun,
        AmbiguityReason::OrderUncertain => FlowAmbiguity::OrderUncertain,
        AmbiguityReason::ShortMove => FlowAmbiguity::ShortMove,
        AmbiguityReason::NumericOnly => FlowAmbiguity::NumericOnly,
        AmbiguityReason::NumericTable => FlowAmbiguity::NumericTable,
    }
}

/// Engine outcome copied from the flow comparison without reinterpretation.
pub(crate) fn outcome(comparison: &FlowComparison, exit: u8) -> EngineOutcome {
    // Exit 3 means at least one evidence reason remains even when both streams
    // are individually complete, for example an unexamined paint inventory.
    // The manifest carries the process verdict, never a more optimistic one.
    let complete = comparison.text_complete() && exit != 3;
    EngineOutcome {
        status: if complete {
            if comparison.non_layout_changes() > 0 {
                EngineStatus::CompleteChanged
            } else {
                EngineStatus::CompleteUnchanged
            }
        } else {
            EngineStatus::Incomplete
        },
        comparison_complete: complete,
        typed_changes: comparison.non_layout_changes(),
        inferred_changes: 0,
        scope_content_changes: 0,
        inferred_scope_changes: 0,
    }
}

/// Identity of a native-text flow bundle.
pub(crate) fn native_identity(
    comparison: &FlowComparison,
    limit_scale: f64,
    sources_retained: bool,
) -> BundleIdentity {
    let (Some(old), Some(new)) = (&comparison.old_bytes, &comparison.new_bytes) else {
        return BundleIdentity {
            policy_version: POLICY_VERSION,
            schema: REVIEW_SCHEMA.into(),
            old_sha256: String::new(),
            new_sha256: String::new(),
            old_bytes: 0,
            new_bytes: 0,
            options: format!(
                "native_text_only;limit_scale={limit_scale};sources={sources_retained}"
            ),
            old_revision: "flow-text-revision-1".to_owned(),
            new_revision: "flow-text-revision-1".to_owned(),
            backends: Vec::new(),
            pipeline: PipelineContract::NativeText,
            selected_channels: std::collections::BTreeSet::from([Channel::Text]),
        };
    };
    let digest = |bytes: &[u8]| {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        crate::fs::lowercase_hex(&hasher.finalize())
    };
    BundleIdentity {
        policy_version: POLICY_VERSION,
        schema: REVIEW_SCHEMA.into(),
        old_sha256: digest(old),
        new_sha256: digest(new),
        old_bytes: old.len(),
        new_bytes: new.len(),
        options: format!("native_text_only;limit_scale={limit_scale};sources={sources_retained}"),
        // Flow has no extraction-store revision. The token-stream policy is
        // the execution snapshot that can be named for this contract.
        old_revision: "flow-text-revision-1".to_owned(),
        new_revision: "flow-text-revision-1".to_owned(),
        backends: Vec::new(),
        pipeline: PipelineContract::NativeText,
        selected_channels: std::collections::BTreeSet::from([Channel::Text]),
    }
}

/// The exact acquired bytes of both sides.
pub(crate) type SourcePair = (Arc<[u8]>, Arc<[u8]>);

/// Exact acquired bytes for a native bundle.
pub(crate) fn source_bytes(comparison: &FlowComparison) -> Option<SourcePair> {
    let old: Arc<[u8]> = comparison.old_bytes.clone()?;
    let new: Arc<[u8]> = comparison.new_bytes.clone()?;
    Some((old, new))
}

/// Flow issues as the comparison JSON carries them.
///
/// The HTML review links each text channel reason here, so the bundle's own
/// JSON is the complete record rather than a summary beside rendered prose.
pub(crate) fn issues_json(comparison: &FlowComparison) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    for (side, view) in [("old", &comparison.old_side), ("new", &comparison.new_side)] {
        let total = view.issues.len();
        let paint_start = total.saturating_sub(view.paint_issues);
        for (index, issue) in view.issues.iter().enumerate() {
            let paint = index >= paint_start;
            values.push(serde_json::json!({
                "side": side,
                "kind": match issue.kind() {
                    pdfdelta_core::source::ExtractionIssueKind::Unsupported => "unsupported",
                    pdfdelta_core::source::ExtractionIssueKind::Unresolved => "unresolved",
                },
                "scope": match issue.scope() {
                    ExtractionScope::Document => "document",
                    ExtractionScope::Page(_) => "page",
                    ExtractionScope::PageGap { .. } => "page_gap",
                    ExtractionScope::GlyphGap { .. } => "glyph_gap",
                    ExtractionScope::PageGlyphGap { .. } => "page_glyph_gap",
                    _ => "other",
                },
                "page": issue_page(issue).map(|page| u64::from(page.0) + 1),
                "unexamined": paint
                    || issue.description().starts_with(IMAGE_TEXT_UNEXAMINED),
                "message": issue.description(),
            }));
        }
    }
    values
}

/// Per-page order evidence, one array per side.
pub(crate) fn order_json(comparison: &FlowComparison) -> serde_json::Value {
    let side = |side: &Side| {
        side.order
            .iter()
            .map(|order| {
                serde_json::json!({
                    "page": u64::from(order.page.0) + 1,
                    "order": format!("{:?}", order.order).to_lowercase(),
                    "uncertain": order.uncertain,
                    "agreement": order.agreement,
                    "coherence": order.coherence,
                    "layout_score": order.layout_score,
                    "stream_score": order.stream_score,
                    "baseline_score": order.baseline_score,
                })
            })
            .collect::<Vec<_>>()
    };
    serde_json::json!({
        "old": side(&comparison.old_side),
        "new": side(&comparison.new_side),
    })
}
