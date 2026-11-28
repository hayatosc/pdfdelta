//! Exit selection shared by the flow command and the multi-channel flow path.
//!
//! The policy keeps exit 0 sound: a channel that detected a difference with
//! intact evidence selects exit 1 even when other channels are broken or were
//! not examined. Broken evidence blocks exit 1 only for the channel it belongs
//! to (or for every channel when its source cannot be attributed), because
//! broken evidence on a channel that did not detect anything cannot make a
//! detection on another channel false.

use std::collections::BTreeSet;

use pdfdelta_core::document::{
    Channel, ChannelCoverage, DocumentViewComparison, EvidenceFailure, EvidenceIssue, KeyDomain,
    ScopedKeyMissing, UnresolvedReason, unresolved_classification,
};
use serde::Serialize;

use crate::flow_cmd::FlowComparison;

/// One exit code with the basis recorded in flow reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FlowExit {
    pub(crate) code: u8,
    pub(crate) basis: &'static str,
    /// Channel whose intact detection justified exit 1. Never a cross-channel
    /// verdict, which has no detection of its own.
    pub(crate) justifying: Option<ChannelClass>,
}

/// Selects the exit code for a single-channel comparison (native flow and the
/// standalone flow command).
///
/// The single channel is text: broken text evidence blocks its detection, an
/// unexamined-only result stays at exit 3, and a detected difference moves to
/// exit 1 while recording that unexamined evidence remains.
pub(crate) fn select(detected: bool, broken: bool, unexamined: bool) -> FlowExit {
    if broken {
        FlowExit {
            code: 3,
            basis: "broken_evidence",
            justifying: None,
        }
    } else if detected {
        FlowExit {
            code: 1,
            basis: if unexamined {
                "detected_with_unexamined_evidence"
            } else {
                "complete_detected"
            },
            justifying: Some(ChannelClass::Text),
        }
    } else if unexamined {
        FlowExit {
            code: 3,
            basis: "unexamined_evidence",
            justifying: None,
        }
    } else {
        FlowExit {
            code: 0,
            basis: "complete_unchanged",
            justifying: None,
        }
    }
}

/// A channel with a verdict of its own, plus the unattributable bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChannelClass {
    Text,
    Visual,
    Forms,
    Relations,
    Presentation,
    /// A reason whose sources span channels or cannot be attributed. It blocks
    /// exit 1 for every channel.
    CrossChannel,
}

impl ChannelClass {
    const ORDER: [Self; 6] = [
        Self::Text,
        Self::Visual,
        Self::Forms,
        Self::Relations,
        Self::Presentation,
        Self::CrossChannel,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Visual => "visual",
            Self::Forms => "forms",
            Self::Relations => "relations",
            Self::Presentation => "presentation",
            Self::CrossChannel => "cross_channel",
        }
    }

    const fn channel(self) -> Option<Channel> {
        match self {
            Self::Text => Some(Channel::Text),
            Self::Visual => Some(Channel::Visual),
            Self::Forms => Some(Channel::Forms),
            Self::Relations => Some(Channel::Relations),
            Self::Presentation => Some(Channel::Presentation),
            Self::CrossChannel => None,
        }
    }
}

/// Per-channel evidence classes copied into the flow multi-channel report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ChannelVerdict {
    pub(crate) channel: ChannelClass,
    pub(crate) detected: bool,
    pub(crate) broken: bool,
    pub(crate) unexamined: bool,
}

impl ChannelVerdict {
    fn new(channel: ChannelClass, detected: bool, classes: Classes) -> Self {
        Self {
            channel,
            detected,
            broken: classes.broken,
            unexamined: classes.unexamined,
        }
    }
}

/// Evidence classes for the multi-channel flow path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DocumentVerdict {
    pub(crate) verdicts: Vec<ChannelVerdict>,
    pub(crate) broken: bool,
    pub(crate) unexamined: bool,
}

/// Selects the exit from per-channel verdicts.
///
/// A detected difference selects exit 1 only through a channel whose own
/// evidence is not broken. Cross-channel broken reasons are represented as a
/// verdict with `detected: false, broken: true`, so they cannot justify exit 1.
/// Exit 0 still requires the whole comparison to be complete.
pub(crate) fn select_channels(verdicts: &[ChannelVerdict], complete: bool) -> FlowExit {
    let any_broken = verdicts.iter().any(|verdict| verdict.broken);
    let any_unexamined = verdicts.iter().any(|verdict| verdict.unexamined);
    let cross_broken = verdicts
        .iter()
        .any(|verdict| verdict.channel == ChannelClass::CrossChannel && verdict.broken);
    if !cross_broken
        && let Some(verdict) = verdicts
            .iter()
            .find(|verdict| verdict.detected && !verdict.broken)
    {
        let incomplete = any_broken || any_unexamined || !complete;
        return FlowExit {
            code: 1,
            basis: if incomplete {
                "detected_with_unexamined_evidence"
            } else {
                "complete_detected"
            },
            justifying: Some(verdict.channel),
        };
    }
    if any_broken {
        FlowExit {
            code: 3,
            basis: "broken_evidence",
            justifying: None,
        }
    } else if any_unexamined || !complete {
        FlowExit {
            code: 3,
            basis: "unexamined_evidence",
            justifying: None,
        }
    } else {
        FlowExit {
            code: 0,
            basis: "complete_unchanged",
            justifying: None,
        }
    }
}

/// Returns whether a relation message describes evidence the engine did not
/// obtain rather than evidence it examined and found untrustworthy. Unknown
/// messages stay broken.
pub(crate) fn relation_issue_is_unexamined(reason: &str) -> bool {
    reason.contains("unresolved content: native tags do not establish complete semantic relationships")
        || reason.contains("marked content has no complete native glyph membership")
        || reason.contains("overlapping or mixed structure memberships require additional views")
        || reason.contains("structure object-reference binding is not implemented")
        || reason.contains(
            "typed relationships have endpoints without established relationship-compatible correspondences",
        )
}

/// A text extraction issue about a page whose only possible text is inside
/// painted images. It records an unexamined text source, not broken extraction:
/// the flow token stream cannot see that text.
pub(crate) const IMAGE_TEXT_UNEXAMINED: &str =
    "page has a potentially visible image and no comparison-visible native glyphs";

fn visual_unexamined(reason: UnresolvedReason) -> bool {
    matches!(
        reason,
        UnresolvedReason::VisualCandidateEnumerationIncomplete
            | UnresolvedReason::IncompatibleRenderProfile
            | UnresolvedReason::PixelMaskOutputLimit
    )
}

/// Text-attributed comparison reasons that a superseded text channel cannot
/// leave as global blockers. They remain broken text evidence when they appear
/// because the supersede pass did not remove them.
fn text_broken(reason: UnresolvedReason) -> bool {
    matches!(
        reason,
        UnresolvedReason::TextCandidateEnumerationIncomplete
            | UnresolvedReason::UnpaddedTextBoundaryOnly
            | UnresolvedReason::TextComparisonLimit
            | UnresolvedReason::LocalTextWorkBudget
            | UnresolvedReason::ExtractionBoundary
            | UnresolvedReason::ExtractionDependencyBudget
            | UnresolvedReason::NativeOrderUnresolved
            | UnresolvedReason::NormalizationInterpretationMissing
            | UnresolvedReason::NormalizationInterpretationsDisagree
            | UnresolvedReason::SpacingInterpretationsRetained
            | UnresolvedReason::SourceCountWitnessWithoutMask
            | UnresolvedReason::InferredGroupSuggestion
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Classes {
    broken: bool,
    unexamined: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Attributions {
    text: Classes,
    visual: Classes,
    forms: Classes,
    relations: Classes,
    presentation: Classes,
    cross: Classes,
}

impl Attributions {
    fn any_broken(&self) -> bool {
        ChannelClass::ORDER
            .iter()
            .any(|class| self.classes_value(*class).broken)
    }

    fn any_unexamined(&self) -> bool {
        ChannelClass::ORDER
            .iter()
            .any(|class| self.classes_value(*class).unexamined)
    }

    fn classes_value(&self, class: ChannelClass) -> Classes {
        match class {
            ChannelClass::Text => self.text,
            ChannelClass::Visual => self.visual,
            ChannelClass::Forms => self.forms,
            ChannelClass::Relations => self.relations,
            ChannelClass::Presentation => self.presentation,
            ChannelClass::CrossChannel => self.cross,
        }
    }
}

/// Inputs for the multi-channel evidence attribution.
pub(crate) struct DocumentEvidence<'a> {
    pub(crate) comparison: &'a DocumentViewComparison,
    pub(crate) coverage: &'a [ChannelCoverage],
    pub(crate) channels: &'a BTreeSet<Channel>,
    pub(crate) flow: Option<&'a FlowComparison>,
    pub(crate) image_incomplete: bool,
    pub(crate) old_issues: &'a [EvidenceIssue],
    pub(crate) new_issues: &'a [EvidenceIssue],
    pub(crate) detected_channels: &'a BTreeSet<Channel>,
}

/// Attributes every exit-relevant reason to a channel or to `cross_channel`.
///
/// Text evidence comes from the flow sides and from text-attributed store
/// issues. Visual, forms, and relations evidence keeps its own coverage and
/// issues. Key-presence obligations are attributed by key domain: field names
/// belong to forms, structure IDs to relations/structure. Scope-wide sources
/// and unknown reasons are cross-channel and block every channel.
pub(crate) fn document_verdicts(evidence: DocumentEvidence<'_>) -> DocumentVerdict {
    let DocumentEvidence {
        comparison,
        coverage,
        channels,
        flow,
        image_incomplete,
        old_issues,
        new_issues,
        detected_channels,
    } = evidence;
    let mut attributed = Attributions::default();
    attribute_issues(&mut attributed, old_issues, new_issues, channels);

    if let Some(flow) = flow {
        attributed.text.broken |= flow.text_broken_evidence();
        attributed.text.unexamined |= flow.text_unexamined_evidence();
    } else if channels.contains(&Channel::Text) {
        // The flow engine always supplies its text comparison when text is
        // selected; an absent one cannot certify the channel.
        attributed.text.broken = true;
    }

    for entry in coverage {
        if entry.complete {
            continue;
        }
        match entry.channel {
            Channel::Visual => {
                // A visual backend failure is broken; renderer coverage,
                // raster budgets, and interpretation warnings are unexamined.
                if !attributed.visual.broken {
                    attributed.visual.unexamined = true;
                }
            }
            Channel::Presentation => attributed.presentation.unexamined = true,
            Channel::Forms => attributed.forms.broken = true,
            Channel::Relations => {
                // A relation inventory that is merely missing binding or
                // correspondence evidence is unexamined; a recognized broken
                // relation issue stays broken.
                if !attributed.relations.broken {
                    attributed.relations.unexamined = true;
                }
            }
            Channel::Text => {
                if flow.is_none() {
                    attributed.text.broken = true;
                }
            }
        }
    }
    if image_incomplete && channels.contains(&Channel::Visual) {
        attributed.visual.unexamined = true;
    }

    // Text is superseded when flow supplied it or when the caller did not
    // select the text channel. An unselected channel's reasons must not decide
    // the exit, even though its views remain relationship correspondence
    // context.
    let text_superseded = flow.is_some() || !channels.contains(&Channel::Text);
    comparison_attributions(&mut attributed, comparison, channels, text_superseded);

    let mut verdicts = Vec::new();
    for class in ChannelClass::ORDER {
        let classes = attributed.classes_value(class);
        let selected = class
            .channel()
            .is_some_and(|channel| channels.contains(&channel));
        // Only a selected channel can justify exit 1. A typed difference the
        // caller did not request is context, not an answer to their question.
        let detected = selected
            && class
                .channel()
                .is_some_and(|channel| detected_channels.contains(&channel));
        if selected || detected || classes.broken || classes.unexamined {
            verdicts.push(ChannelVerdict::new(class, detected, classes));
        }
    }
    let broken = attributed.any_broken();
    let unexamined = attributed.any_unexamined();
    DocumentVerdict {
        verdicts,
        broken,
        unexamined,
    }
}

/// Attributes acquisition issues to their channels.
///
/// Rendering may retain visual issues as review context for a text-only run, so
/// evidence for an unselected channel cannot decide the exit. A text issue
/// about text potentially inside painted images is unexamined, not broken.
fn attribute_issues(
    attributed: &mut Attributions,
    old_issues: &[EvidenceIssue],
    new_issues: &[EvidenceIssue],
    channels: &BTreeSet<Channel>,
) {
    for issue in old_issues.iter().chain(new_issues) {
        if !channels.contains(&issue.channel) {
            continue;
        }
        match issue.channel {
            Channel::Text => {
                if issue.reason.contains(IMAGE_TEXT_UNEXAMINED) {
                    attributed.text.unexamined = true;
                } else {
                    attributed.text.broken = true;
                }
            }
            Channel::Visual => {
                if matches!(issue.kind, EvidenceFailure::BackendFailure) {
                    attributed.visual.broken = true;
                } else {
                    attributed.visual.unexamined = true;
                }
            }
            Channel::Forms => attributed.forms.broken = true,
            Channel::Relations => {
                if relation_issue_is_unexamined(&issue.reason) {
                    attributed.relations.unexamined = true;
                } else {
                    attributed.relations.broken = true;
                }
            }
            Channel::Presentation => attributed.presentation.unexamined = true,
        }
    }
}

/// Marks one key domain's channel unexamined.
fn key_domain_unexamined(attributed: &mut Attributions, domain: KeyDomain) {
    match domain {
        KeyDomain::PdfFieldName => attributed.forms.unexamined = true,
        KeyDomain::PdfStructureId => attributed.relations.unexamined = true,
    }
}

/// Marks one key domain's channel broken.
fn key_domain_broken(attributed: &mut Attributions, domain: KeyDomain) {
    match domain {
        KeyDomain::PdfFieldName => attributed.forms.broken = true,
        KeyDomain::PdfStructureId => attributed.relations.broken = true,
    }
}

/// Attributes comparison-level obligations and exhausted searches.
///
/// `text_superseded` means flow owns the text channel (or text was excluded),
/// so the shared comparison no longer retains text-only obligations and its
/// raw candidate/text-search flags cannot decide the exit by themselves. Any
/// remaining source-incompleteness obligation was kept because it can involve
/// non-text candidates and is therefore cross-channel.
fn comparison_attributions(
    attributed: &mut Attributions,
    comparison: &DocumentViewComparison,
    channels: &BTreeSet<Channel>,
    text_superseded: bool,
) {
    let visual_selected = channels.contains(&Channel::Visual);
    if let Some(keys) = &comparison.key_presence {
        if !keys.exhaustive {
            // An unfinished raw key population has no domain to attribute.
            attributed.cross.broken = true;
        }
        for obligation in &keys.obligations {
            // A key obligation states why a population or identity claim could
            // not be established: a missing inventory, unnamed or duplicate
            // members, a retained rename alternative, or an unavailable
            // acquisition path. That is evidence the engine did not obtain,
            // not evidence it examined and found untrustworthy. An exhausted
            // key search already clears `keys.exhaustive`, which is reported
            // as cross-channel broken evidence.
            key_domain_unexamined(attributed, obligation.domain);
        }
        if let Some(scoped) = &keys.scoped {
            // A scoped obligation names the claim whose scoped identity could
            // not be established. Its domain is the claim's own key domain
            // (field names belong to forms, structure IDs to relations).
            let domains = |claim: Option<usize>| {
                claim
                    .and_then(|index| keys.claims.get(index))
                    .map(|key| key.domain)
            };
            let mut exhausted_domains = BTreeSet::new();
            if !scoped.exhaustive {
                exhausted_domains.extend(keys.claims.iter().map(|key| key.domain));
            }
            for obligation in &scoped.obligations {
                if obligation.missing == ScopedKeyMissing::WorkBudget {
                    // An exhausted scoped search is broken evidence. The
                    // budget obligation normally has no claim, so every
                    // domain with a claim is owning; a claim-specific budget
                    // failure names only that claim's domain.
                    if let Some(domain) = domains(obligation.claim) {
                        exhausted_domains.insert(domain);
                    } else {
                        exhausted_domains.extend(keys.claims.iter().map(|key| key.domain));
                    }
                } else if let Some(domain) = domains(obligation.claim) {
                    key_domain_unexamined(attributed, domain);
                } else {
                    // A scoped obligation without a claim index cannot name a
                    // domain. Every domain with a claim is a possible owner,
                    // so keep it unexamined on each.
                    for key in &keys.claims {
                        key_domain_unexamined(attributed, key.domain);
                    }
                }
            }
            for domain in exhausted_domains {
                key_domain_broken(attributed, domain);
            }
        }
    }
    for (index, message) in comparison.relation_unresolved.iter().enumerate() {
        let reason = comparison
            .relation_obligations
            .get(index)
            .map(|item| item.reason);
        if relation_issue_is_unexamined(message)
            || reason == Some(UnresolvedReason::RelationEndpointsUnmatched)
        {
            attributed.relations.unexamined = true;
        } else {
            attributed.relations.broken = true;
        }
    }
    if comparison.relation_obligations.len() > comparison.relation_unresolved.len() {
        attributed.cross.broken = true;
    }
    for scope in &comparison.scopes {
        if !scope.result.structural_correspondences.is_empty() {
            attributed.cross.broken = true;
        }
        if !scope.result.candidates.exhaustive && !text_superseded {
            // Source candidate omission without a retained obligation is
            // scope-wide and cannot be attributed to one channel.
            attributed.cross.broken = true;
        }
        if !scope.result.text_search.exhaustive && !text_superseded {
            attributed.text.broken = true;
        }
        if !scope.result.visual_search.exhaustive {
            if visual_selected {
                attributed.visual.unexamined = true;
            } else {
                attributed.cross.broken = true;
            }
        }
        for index in 0..scope.result.unresolved.len() {
            let reason = unresolved_classification(&scope.result.obligations, index).reason;
            if visual_unexamined(reason) {
                if visual_selected {
                    attributed.visual.unexamined = true;
                } else {
                    attributed.cross.broken = true;
                }
            } else if reason == UnresolvedReason::RelationEndpointsUnmatched {
                attributed.relations.unexamined = true;
            } else if reason == UnresolvedReason::RelationGraphIncomplete {
                attributed.relations.broken = true;
            } else if text_broken(reason) {
                attributed.text.broken = true;
            } else {
                // Scope-wide, component-scoped, or unknown reasons may involve
                // any content family. Keep them broken for every channel.
                attributed.cross.broken = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use pdfdelta_core::document::{
        Channel, DocumentViewComparison, EvidenceFailure, EvidenceIssue, KeyCounterpart, KeyDomain,
        KeyEvidenceAction, KeyPresenceClaim, KeyWorkStatus, PresenceSide, ScopedKeyClaim,
        ScopedKeyComparison, ScopedKeyCounterpart, ScopedKeyMissing, ScopedKeyObligation,
        ScopedKeyPolicy, SourceRef,
    };

    use super::{
        Attributions, ChannelClass, ChannelVerdict, Classes, IMAGE_TEXT_UNEXAMINED,
        attribute_issues, comparison_attributions, relation_issue_is_unexamined, select,
        select_channels,
    };

    /// One scope with empty candidates, no comparisons, and caller-selected
    /// search completeness flags.
    fn comparison(
        candidates_exhaustive: bool,
        text_exhaustive: bool,
        obligations: serde_json::Value,
        unresolved: serde_json::Value,
    ) -> DocumentViewComparison {
        serde_json::from_value(serde_json::json!({
            "relations": [],
            "relation_unresolved": [],
            "scopes": [{
                "parent": null,
                "depth": 0,
                "interpretation": "conditional_on_correspondence",
                "result": {
                    "candidates": {
                        "proposals": [],
                        "examined_pairs": 0,
                        "group_token_checks": 0,
                        "group_constraint_checks": 0,
                        "exhaustive": candidates_exhaustive
                    },
                    "matching": {
                        "channels": {
                            "text": false, "visual": true, "forms": true,
                            "relations": true, "presentation": true
                        },
                        "objective": "scoped_identity_then_literal_then_inferred_structure_v3",
                        "scope": {"old": 0, "new": 0},
                        "components": [],
                        "conflict_checks": 0,
                        "ownership_visits": 0,
                        "source_only_mandatory": [],
                        "inferred_proposals": [],
                        "conflict_search_complete": true
                    },
                    "visual_search": {
                        "examined_pairs": 0, "compared_pixels": 0, "exhaustive": true
                    },
                    "text_search": {
                        "examined_pairs": 0, "source_ownership_visits": 0,
                        "source_conflict_checks": 0, "source_search_states": 0,
                        "protected_correspondences": [], "old_nodes": [], "new_nodes": [],
                        "token_visits": 0, "feature_entries": 0, "exhaustive": text_exhaustive
                    },
                    "comparisons": [],
                    "accepted_correspondences": [],
                    "structural_correspondences": [],
                    "unresolved": unresolved,
                    "obligations": obligations
                }
            }]
        }))
        .expect("fixture comparison")
    }

    fn verdict(
        channel: ChannelClass,
        detected: bool,
        broken: bool,
        unexamined: bool,
    ) -> ChannelVerdict {
        ChannelVerdict {
            channel,
            detected,
            broken,
            unexamined,
        }
    }

    #[test]
    fn broken_evidence_wins_over_detected_difference_for_a_single_channel() {
        let exit = select(true, true, true);
        assert_eq!(exit.code, 3);
        assert_eq!(exit.basis, "broken_evidence");
    }

    #[test]
    fn single_channel_detected_difference_keeps_its_previous_basis() {
        let exit = select(true, false, true);
        assert_eq!(exit.code, 1);
        assert_eq!(exit.basis, "detected_with_unexamined_evidence");
        assert_eq!(exit.justifying, Some(ChannelClass::Text));
        let clean = select(true, false, false);
        assert_eq!(clean.code, 1);
        assert_eq!(clean.basis, "complete_detected");
    }

    #[test]
    fn single_channel_unexamined_only_is_exit_three_and_clean_is_zero() {
        let unexamined = select(false, false, true);
        assert_eq!(unexamined.code, 3);
        assert_eq!(unexamined.basis, "unexamined_evidence");
        let clean = select(false, false, false);
        assert_eq!(clean.code, 0);
        assert_eq!(clean.basis, "complete_unchanged");
    }

    #[test]
    fn broken_evidence_on_another_channel_does_not_block_a_clean_detection() {
        let verdicts = [
            verdict(ChannelClass::Text, true, false, true),
            verdict(ChannelClass::Forms, false, true, false),
        ];
        let exit = select_channels(&verdicts, false);
        assert_eq!(exit.code, 1);
        assert_eq!(exit.basis, "detected_with_unexamined_evidence");
        assert_eq!(exit.justifying, Some(ChannelClass::Text));
    }

    #[test]
    fn broken_evidence_on_the_detecting_channel_blocks_its_detection() {
        let verdicts = [
            verdict(ChannelClass::Text, true, true, false),
            verdict(ChannelClass::Forms, false, false, false),
        ];
        let exit = select_channels(&verdicts, false);
        assert_eq!(exit.code, 3);
        assert_eq!(exit.basis, "broken_evidence");
        let forms_clean = [
            verdict(ChannelClass::Text, true, true, false),
            verdict(ChannelClass::Forms, true, false, false),
        ];
        let exit = select_channels(&forms_clean, false);
        assert_eq!(exit.code, 1);
        assert_eq!(exit.justifying, Some(ChannelClass::Forms));
    }

    #[test]
    fn cross_channel_broken_evidence_blocks_every_detection() {
        let verdicts = [
            verdict(ChannelClass::Text, true, false, false),
            verdict(ChannelClass::CrossChannel, false, true, false),
        ];
        let exit = select_channels(&verdicts, false);
        assert_eq!(exit.code, 3);
        assert_eq!(exit.basis, "broken_evidence");
    }

    #[test]
    fn no_detection_with_broken_evidence_is_exit_three() {
        let verdicts = [verdict(ChannelClass::Forms, false, true, false)];
        let exit = select_channels(&verdicts, false);
        assert_eq!(exit.code, 3);
        assert_eq!(exit.basis, "broken_evidence");
    }

    #[test]
    fn superseded_text_flags_do_not_add_broken_evidence() {
        let channels = BTreeSet::from([Channel::Visual, Channel::Forms, Channel::Relations]);
        let incomplete = comparison(false, false, serde_json::json!([]), serde_json::json!([]));
        let mut attributed = Attributions::default();
        comparison_attributions(&mut attributed, &incomplete, &channels, true);
        assert!(!attributed.any_broken());
        let mut strict = Attributions::default();
        comparison_attributions(&mut strict, &incomplete, &channels, false);
        assert!(strict.cross.broken || strict.text.broken);
    }

    #[test]
    fn retained_source_incompleteness_is_cross_channel_broken() {
        let channels = BTreeSet::from([Channel::Visual, Channel::Forms, Channel::Relations]);
        let retained = comparison(
            false,
            true,
            serde_json::json!([{"reason": "source_candidate_enumeration_incomplete"}]),
            serde_json::json!(["source candidate enumeration is incomplete"]),
        );
        let mut attributed = Attributions::default();
        comparison_attributions(&mut attributed, &retained, &channels, true);
        assert!(attributed.cross.broken);
        assert_eq!(attributed.text, Classes::default());
    }

    fn comparison_with_key_obligation(domain: &str) -> DocumentViewComparison {
        let mut value = serde_json::to_value(comparison(
            true,
            true,
            serde_json::json!([]),
            serde_json::json!([]),
        ))
        .expect("fixture JSON");
        value["key_presence"] = serde_json::json!({
            "policy": "native_document_keys_v1",
            "old_revision": "old",
            "new_revision": "new",
            "populations": [],
            "claims": [],
            "obligations": [{
                "domain": domain,
                "side": "old",
                "sources": [],
                "dependent_claims": [],
                "missing": "named_members",
                "next_action": "unavailable",
                "work": "unavailable"
            }],
            "work": 0,
            "exhaustive": true
        });
        serde_json::from_value(value).expect("key fixture")
    }

    fn comparison_with_scoped_obligation(
        domain: KeyDomain,
        missing: ScopedKeyMissing,
        exhaustive: bool,
    ) -> DocumentViewComparison {
        let domain_name = match domain {
            KeyDomain::PdfFieldName => "pdf_field_name",
            KeyDomain::PdfStructureId => "pdf_structure_id",
        };
        let mut comparison = comparison_with_key_obligation(domain_name);
        let keys = comparison.key_presence.as_mut().expect("key fixture");
        keys.claims.push(KeyPresenceClaim {
            side: PresenceSide::Old,
            domain,
            source: SourceRef::Structured { element: 0 },
            key: b"key".to_vec(),
            counterpart: KeyCounterpart::Unknown,
        });
        let (claim, next_action, work) = if missing == ScopedKeyMissing::WorkBudget {
            (
                None,
                KeyEvidenceAction::IncreaseSearchBudget,
                KeyWorkStatus::Limited,
            )
        } else {
            (
                Some(0),
                KeyEvidenceAction::Unavailable,
                KeyWorkStatus::Unavailable,
            )
        };
        keys.scoped = Some(ScopedKeyComparison {
            policy: ScopedKeyPolicy::NativeKeyMembershipV1,
            claims: vec![ScopedKeyClaim {
                claim: 0,
                node: None,
                scope: None,
                counterpart: ScopedKeyCounterpart::Unknown,
            }],
            witnesses: Vec::new(),
            operations: Vec::new(),
            obligations: vec![ScopedKeyObligation {
                claim,
                scope: None,
                missing,
                next_action,
                work,
            }],
            work: 1,
            exhaustive,
        });
        comparison
    }

    fn issue(channel: Channel, kind: EvidenceFailure, reason: &str) -> EvidenceIssue {
        EvidenceIssue {
            page: None,
            channel,
            sources: Vec::new(),
            boundary: None,
            kind,
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn key_presence_domains_are_unexamined_on_their_own_channel() {
        let channels = BTreeSet::from([
            Channel::Text,
            Channel::Visual,
            Channel::Forms,
            Channel::Relations,
        ]);
        let structure = comparison_with_key_obligation("pdf_structure_id");
        let mut attributed = Attributions::default();
        comparison_attributions(&mut attributed, &structure, &channels, true);
        assert!(attributed.relations.unexamined);
        assert!(!attributed.relations.broken);
        assert_eq!(attributed.forms, Classes::default());
        assert!(!attributed.text.broken);
        let field = comparison_with_key_obligation("pdf_field_name");
        let mut attributed = Attributions::default();
        comparison_attributions(&mut attributed, &field, &channels, true);
        assert!(attributed.forms.unexamined);
        assert!(!attributed.forms.broken);
        assert_eq!(attributed.relations, Classes::default());
        assert!(!attributed.text.broken);
    }

    #[test]
    fn exhausted_key_search_is_cross_channel_broken() {
        let channels = BTreeSet::from([Channel::Forms, Channel::Relations]);
        let mut structure = comparison_with_key_obligation("pdf_structure_id");
        structure.key_presence.as_mut().expect("keys").exhaustive = false;
        let mut attributed = Attributions::default();
        comparison_attributions(&mut attributed, &structure, &channels, true);
        assert!(attributed.cross.broken);
        // The obligation itself still records a missing membership, not a
        // contradiction on the relations channel.
        assert!(attributed.relations.unexamined);
        assert!(!attributed.relations.broken);
    }

    #[test]
    fn scoped_exhausted_search_is_broken_on_its_claim_domain() {
        let channels = BTreeSet::from([Channel::Forms, Channel::Relations]);
        let comparison = comparison_with_scoped_obligation(
            KeyDomain::PdfStructureId,
            ScopedKeyMissing::NativeBinding,
            false,
        );
        let mut attributed = Attributions::default();
        comparison_attributions(&mut attributed, &comparison, &channels, true);
        assert!(attributed.relations.broken);
        assert!(!attributed.cross.broken);
        assert_eq!(attributed.forms, Classes::default());
    }

    #[test]
    fn scoped_work_budget_is_broken_on_every_claim_domain() {
        let channels = BTreeSet::from([Channel::Forms, Channel::Relations]);
        let mut comparison = comparison_with_scoped_obligation(
            KeyDomain::PdfStructureId,
            ScopedKeyMissing::WorkBudget,
            true,
        );
        comparison
            .key_presence
            .as_mut()
            .expect("keys")
            .claims
            .push(KeyPresenceClaim {
                side: PresenceSide::New,
                domain: KeyDomain::PdfFieldName,
                source: SourceRef::Structured { element: 1 },
                key: b"field".to_vec(),
                counterpart: KeyCounterpart::Unknown,
            });
        let mut attributed = Attributions::default();
        comparison_attributions(&mut attributed, &comparison, &channels, true);
        assert!(attributed.relations.broken);
        assert!(attributed.forms.broken);
        assert!(!attributed.cross.broken);
    }

    #[test]
    fn scoped_non_budget_obligation_is_unexamined_on_its_domain() {
        let channels = BTreeSet::from([Channel::Forms, Channel::Relations]);
        let comparison = comparison_with_scoped_obligation(
            KeyDomain::PdfFieldName,
            ScopedKeyMissing::NativeBinding,
            true,
        );
        let mut attributed = Attributions::default();
        comparison_attributions(&mut attributed, &comparison, &channels, true);
        assert!(attributed.forms.unexamined);
        assert!(!attributed.forms.broken);
        assert_eq!(attributed.relations, Classes::default());
        assert!(!attributed.cross.broken);
    }

    #[test]
    fn unexamined_key_obligations_do_not_block_an_intact_text_detection() {
        let channels = BTreeSet::from([Channel::Text, Channel::Relations]);
        let structure = comparison_with_key_obligation("pdf_structure_id");
        let mut attributed = Attributions::default();
        comparison_attributions(&mut attributed, &structure, &channels, true);
        let verdicts = [
            verdict(
                ChannelClass::Text,
                true,
                attributed.text.broken,
                attributed.text.unexamined,
            ),
            verdict(
                ChannelClass::Relations,
                false,
                attributed.relations.broken,
                attributed.relations.unexamined,
            ),
        ];
        let exit = select_channels(&verdicts, false);
        assert_eq!(exit.code, 1);
        assert_eq!(exit.justifying, Some(ChannelClass::Text));
    }

    #[test]
    fn image_only_text_issues_are_unexamined_not_broken() {
        let channels = BTreeSet::from([Channel::Text]);
        let mut attributed = Attributions::default();
        attribute_issues(
            &mut attributed,
            &[issue(
                Channel::Text,
                EvidenceFailure::Unresolved,
                IMAGE_TEXT_UNEXAMINED,
            )],
            &[],
            &channels,
        );
        assert!(attributed.text.unexamined);
        assert!(!attributed.text.broken);
        let mut attributed = Attributions::default();
        attribute_issues(
            &mut attributed,
            &[issue(
                Channel::Text,
                EvidenceFailure::Unresolved,
                "PDF stream 1 0 R: FlateDecode stage 0 is malformed",
            )],
            &[],
            &channels,
        );
        assert!(attributed.text.broken);
        assert!(!attributed.text.unexamined);
    }

    #[test]
    fn issues_on_unselected_channels_are_ignored() {
        let channels = BTreeSet::from([Channel::Text]);
        let mut attributed = Attributions::default();
        attribute_issues(
            &mut attributed,
            &[issue(
                Channel::Visual,
                EvidenceFailure::BackendFailure,
                "evidence process failed",
            )],
            &[],
            &channels,
        );
        assert_eq!(attributed.visual, Classes::default());
    }

    #[test]
    fn visual_backend_failures_are_broken_but_renderer_gaps_are_unexamined() {
        let channels = BTreeSet::from([Channel::Visual]);
        let mut backend = Attributions::default();
        attribute_issues(
            &mut backend,
            &[issue(
                Channel::Visual,
                EvidenceFailure::BackendFailure,
                "evidence process failed",
            )],
            &[],
            &channels,
        );
        assert!(backend.visual.broken);
        let mut coverage = Attributions::default();
        attribute_issues(
            &mut coverage,
            &[issue(
                Channel::Visual,
                EvidenceFailure::ResourceLimit,
                "retained raster budget exhausted",
            )],
            &[],
            &channels,
        );
        assert!(coverage.visual.unexamined);
        assert!(!coverage.visual.broken);
    }

    #[test]
    fn relation_binding_gaps_are_unexamined_but_graph_failures_are_broken() {
        assert!(relation_issue_is_unexamined(
            "unresolved content: native tags do not establish complete semantic relationships"
        ));
        assert!(relation_issue_is_unexamined(
            "typed relationships have endpoints without established relationship-compatible correspondences"
        ));
        assert!(!relation_issue_is_unexamined(
            "typed relationship enumeration exceeded its work budget"
        ));
        assert!(!relation_issue_is_unexamined(
            "an absent typed relationship cannot be established from incomplete graph relations"
        ));
    }
}
