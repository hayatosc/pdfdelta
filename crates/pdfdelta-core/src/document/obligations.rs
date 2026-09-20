//! Classified reasons for retained comparison obligations.
//!
//! Every unresolved message the comparison retains is also classified at the
//! point it is emitted. Downstream consumers therefore never have to infer a
//! category by matching on message text, which would silently reclassify an
//! obligation whenever its wording changed.
//!
//! Classifications are index-aligned with their messages: entry *i* of an
//! `obligations` list classifies message *i* of the corresponding `unresolved`
//! list. Use the `retain_unresolved` methods rather than pushing to either list
//! directly so that the alignment holds.

use serde::{Deserialize, Serialize};

use super::{DocumentViewComparison, LocalViewComparison, ScopeViewComparison};

/// Why a comparison retains an obligation.
///
/// Variants name the situation at the emission site, not the consequence. A
/// consumer that needs a coarser grouping derives it; the engine does not
/// collapse distinct causes into a shared label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnresolvedReason {
    /// Candidate generation for the scope stopped before closing.
    ScopeCandidateEnumerationIncomplete,
    /// Source-backed candidate generation stopped inside dependency regions.
    SourceCandidateEnumerationIncomplete,
    /// Visual candidate generation stopped before closing.
    VisualCandidateEnumerationIncomplete,
    /// Text candidate generation stopped before closing.
    TextCandidateEnumerationIncomplete,
    /// Structural candidate generation stopped before closing.
    StructuralCandidateEnumerationIncomplete,
    /// A solver component depends on candidates that were never enumerated.
    ComponentDependsOnOmittedCandidates,
    /// A solver component exhausted its search budget.
    ComponentSearchBudget,
    /// A solver component has several optima that weighting cannot separate.
    CompetingOptima,
    /// An accepted correspondence depends on rivals that were never examined.
    UnexaminedRivals,
    /// Only an unpadded text boundary is established; the enclosing paragraph
    /// identity and any unaccounted sources remain open.
    UnpaddedTextBoundaryOnly,
    /// The comparison crosses an unresolved extraction boundary.
    ExtractionBoundary,
    /// The extraction dependency search exhausted its work budget.
    ExtractionDependencyBudget,
    /// The local character proof did not finish within its work budget.
    LocalProofBudget,
    /// The local text comparison hit a token or work limit.
    TextComparisonLimit,
    /// Normalization has no source-validated family of interpretations.
    NormalizationInterpretationMissing,
    /// Permitted normalization interpretations disagree about equality.
    NormalizationInterpretationsDisagree,
    /// Layout-derived spacing retains more than one interpretation.
    SpacingInterpretationsRetained,
    /// A source-count witness proves change without localizing a mask.
    MultiplicityProofWithoutMask,
    /// Native token order is unresolved, so no ordered mask can be produced.
    NativeOrderUnresolved,
    /// The pixel mask exceeded its output limit.
    PixelMaskOutputLimit,
    /// Two render regions do not share a declared profile and sample grid.
    IncompatibleRenderProfile,
    /// The local content types need a structural comparison or more evidence.
    UnsupportedLocalContent,
    /// A ranked adjacent-group suggestion whose correspondence is unproved.
    InferredGroupSuggestion,
    /// Typed relationship endpoints have no compatible correspondence.
    RelationEndpointsUnmatched,
    /// Graph relationships are incomplete, so absence cannot be established.
    RelationGraphIncomplete,
    /// Counterpart table refinement search did not close.
    CounterpartRefinementIncomplete,
    /// An obligation this vocabulary does not yet classify. The retained
    /// message is the only description; it is never discarded.
    Other,
}

/// One classified obligation, aligned with its retained message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedObligation {
    pub reason: UnresolvedReason,
    /// Correspondence proposal index inside the owning scope, when the
    /// obligation concerns one proposal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<usize>,
    /// Local comparison index inside the owning scope, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<usize>,
}

impl UnresolvedObligation {
    #[must_use]
    pub const fn new(reason: UnresolvedReason) -> Self {
        Self {
            reason,
            proposal: None,
            comparison: None,
        }
    }

    #[must_use]
    pub const fn for_proposal(reason: UnresolvedReason, proposal: usize) -> Self {
        Self {
            reason,
            proposal: Some(proposal),
            comparison: None,
        }
    }

    #[must_use]
    pub const fn for_comparison(reason: UnresolvedReason, comparison: usize) -> Self {
        Self {
            reason,
            proposal: None,
            comparison: Some(comparison),
        }
    }
}

/// Reads a classification that may be missing from an older result.
///
/// A message without a classification is reported as
/// [`UnresolvedReason::Other`] rather than dropped, so an obligation never
/// disappears because its classification is absent.
#[must_use]
pub fn classification(obligations: &[UnresolvedObligation], index: usize) -> UnresolvedObligation {
    obligations
        .get(index)
        .copied()
        .unwrap_or_else(|| UnresolvedObligation::new(UnresolvedReason::Other))
}

impl LocalViewComparison {
    /// Retains an unresolved message together with its classification.
    pub fn retain_unresolved(
        &mut self,
        obligation: UnresolvedObligation,
        message: impl Into<String>,
    ) {
        self.unresolved.push(message.into());
        self.obligations.push(obligation);
    }
}

impl ScopeViewComparison {
    /// Retains a scope-level unresolved message with its classification.
    pub fn retain_unresolved(
        &mut self,
        obligation: UnresolvedObligation,
        message: impl Into<String>,
    ) {
        self.unresolved.push(message.into());
        self.obligations.push(obligation);
    }
}

impl DocumentViewComparison {
    /// Retains a relationship-level unresolved message with its classification.
    pub fn retain_relation_unresolved(
        &mut self,
        obligation: UnresolvedObligation,
        message: impl Into<String>,
    ) {
        self.relation_unresolved.push(message.into());
        self.relation_obligations.push(obligation);
    }
}

/// A borrowed pair of aligned message and classification lists.
///
/// Helper routines that only collect obligations take this instead of a bare
/// message list, so they cannot record a message without classifying it.
pub struct UnresolvedSink<'a> {
    messages: &'a mut Vec<String>,
    obligations: &'a mut Vec<UnresolvedObligation>,
}

impl<'a> UnresolvedSink<'a> {
    #[must_use]
    pub fn new(
        messages: &'a mut Vec<String>,
        obligations: &'a mut Vec<UnresolvedObligation>,
    ) -> Self {
        Self {
            messages,
            obligations,
        }
    }

    pub fn retain(&mut self, obligation: UnresolvedObligation, message: impl Into<String>) {
        self.messages.push(message.into());
        self.obligations.push(obligation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_classification_is_reported_rather_than_dropped() {
        let obligations = [UnresolvedObligation::new(UnresolvedReason::CompetingOptima)];
        assert_eq!(
            classification(&obligations, 0).reason,
            UnresolvedReason::CompetingOptima
        );
        assert_eq!(
            classification(&obligations, 1).reason,
            UnresolvedReason::Other
        );
        assert_eq!(classification(&[], 0).reason, UnresolvedReason::Other);
    }
}
